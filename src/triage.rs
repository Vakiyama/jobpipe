//! LLM scoring pass. Untriaged postings are first run through a cheap code-side
//! pre-filter (obvious-reject titles), then the survivors are batched to the
//! Anthropic Messages API for scoring against the candidate profile.
//!
//! The API key is read from `ANTHROPIC_API_KEY`. The model is a fast/cheap tier
//! (Haiku) — this is classification, not reasoning. Only postings with
//! `triaged_at IS NULL` are ever sent; a posting is never re-scored.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use futures::stream::{self, StreamExt};
use regex::Regex;
use reqwest::Client;
use sea_orm::DatabaseConnection;
use serde::Deserialize;
use serde_json::json;
use std::io::{self, Write};
use std::time::Duration;
use tracing::{info, warn};

use crate::config::Prefilter;
use crate::db::entities::posting;
use crate::db::queries;

const ANTHROPIC_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
/// Fast/cheap classification tier, per the spec — not a reasoning model.
const MODEL: &str = "claude-haiku-4-5";
/// Postings per API request (spec: batch 10-20 to keep cost down).
const BATCH_SIZE: usize = 15;
/// Batches scored concurrently. The scoring calls are network-bound, so running
/// several in flight turns a long sequential run into a short one; DB writes are
/// still applied serially on the main task to avoid SQLite writer contention. The
/// first batch is sent alone to warm the prompt cache before this wave (see `run`).
const SCORE_CONCURRENCY: usize = 8;
const MAX_TOKENS: u32 = 4096;
/// Descriptions are truncated before they hit the prompt to bound token spend.
const DESC_LIMIT: usize = 1200;

/// The rubric caps any posting whose location fails the hard gate at this score.
/// The model is told to do this itself, but a fast model applies it inconsistently
/// (it will emit a location-failure flag and still return an 8), so we re-apply the
/// cap deterministically from its own flags — see [`enforce_location_gate`].
const LOCATION_FAIL_CAP: i32 = 3;

/// Flags the model emits when a posting's location fails the hard gate. Their
/// presence is the model's own signal that the rubric's location cap must hold,
/// regardless of the raw score it returned alongside them.
const LOCATION_FAIL_FLAGS: [&str; 2] = ["no_location", "hybrid_offsite"];

// claude-haiku-4-5 pricing, USD per million tokens. Update if Anthropic changes
// rates. Cache writes bill at 1.25x input (5-minute TTL); cache reads at ~0.1x.
const PRICE_INPUT_PER_MTOK: f64 = 1.00;
const PRICE_OUTPUT_PER_MTOK: f64 = 5.00;
const PRICE_CACHE_WRITE_PER_MTOK: f64 = 1.25;
const PRICE_CACHE_READ_PER_MTOK: f64 = 0.10;
/// Rough output tokens per posting for the pre-run estimate (small JSON object).
const EST_OUTPUT_TOKENS_PER_POSTING: usize = 50;

/// Compiled code-side pre-filter, built from the profile's `[prefilter]` config.
/// A posting is rejected when its title matches `reject` unless its description
/// matches `keep` (the rescue keyword).
struct Screen {
    reject: Option<Regex>,
    keep: Option<Regex>,
}

impl Screen {
    fn build(cfg: &Prefilter) -> Result<Self> {
        Ok(Self {
            reject: compile_alternation(&cfg.reject_titles)?,
            keep: compile_alternation(&cfg.keep_keywords)?,
        })
    }

    /// True if the posting should be dropped before the LLM call.
    fn is_reject(&self, p: &posting::Model) -> bool {
        let Some(reject) = &self.reject else {
            return false; // no reject terms configured → nothing pre-filtered
        };
        reject.is_match(&p.title)
            && !self
                .keep
                .as_ref()
                .is_some_and(|k| k.is_match(&p.description))
    }
}

/// Compile a word list into one case-insensitive, word-boundary alternation regex,
/// or `None` when the list is empty. Terms are escaped so punctuation is literal.
fn compile_alternation(words: &[String]) -> Result<Option<Regex>> {
    if words.is_empty() {
        return Ok(None);
    }
    let alt = words
        .iter()
        .map(|w| regex::escape(w))
        .collect::<Vec<_>>()
        .join("|");
    let re = Regex::new(&format!(r"(?i)\b(?:{alt})\b"))
        .with_context(|| format!("building prefilter regex from {words:?}"))?;
    Ok(Some(re))
}

/// One posting's score as returned by the model.
#[derive(Debug, Deserialize)]
struct Scored {
    external_id: String,
    score: i64,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    flags: Vec<String>,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    #[serde(default)]
    content: Vec<ContentBlock>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Usage,
}

/// Token usage from one response. Summed across batches to report real spend.
#[derive(Debug, Default, Clone, Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

impl Usage {
    fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_creation_input_tokens += other.cache_creation_input_tokens;
        self.cache_read_input_tokens += other.cache_read_input_tokens;
    }

    /// Dollar cost of this usage at the module's `PRICE_*` rates.
    fn cost_usd(&self) -> f64 {
        (self.input_tokens as f64 * PRICE_INPUT_PER_MTOK
            + self.output_tokens as f64 * PRICE_OUTPUT_PER_MTOK
            + self.cache_creation_input_tokens as f64 * PRICE_CACHE_WRITE_PER_MTOK
            + self.cache_read_input_tokens as f64 * PRICE_CACHE_READ_PER_MTOK)
            / 1_000_000.0
    }
}

#[derive(Deserialize)]
struct ContentBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: String,
}

/// Run the triage pass. With `dry_run`, no request is made and no spend occurs —
/// it reports how many postings would be sent and a rough input-token estimate.
pub async fn run(
    db: &DatabaseConnection,
    limit: Option<u64>,
    dry_run: bool,
    profile_text: &str,
    prefilter: &Prefilter,
) -> Result<()> {
    let postings = queries::untriaged_postings(db, limit).await?;
    if postings.is_empty() {
        println!("No untriaged postings. Run `jobpipe fetch` first.");
        return Ok(());
    }

    let screen = Screen::build(prefilter)?;

    // Split into code-side rejects and LLM candidates.
    let mut rejects = Vec::new();
    let mut candidates = Vec::new();
    for p in postings {
        if screen.is_reject(&p) {
            rejects.push(p);
        } else {
            candidates.push(p);
        }
    }

    let system = system_prompt(profile_text);

    if dry_run {
        let n_batches = candidates.chunks(BATCH_SIZE).count();
        let mut est_input_tokens = 0usize;
        for batch in candidates.chunks(BATCH_SIZE) {
            // ~4 chars/token is the standard rough approximation.
            est_input_tokens += (system.len() + user_message(batch).len()) / 4;
        }
        let est_output_tokens = candidates.len() * EST_OUTPUT_TOKENS_PER_POSTING;
        // Upper bound: prices input at full rate (ignores the cache discount that
        // makes real runs cheaper), so the actual bill lands at or below this.
        let est_cost = (est_input_tokens as f64 * PRICE_INPUT_PER_MTOK
            + est_output_tokens as f64 * PRICE_OUTPUT_PER_MTOK)
            / 1_000_000.0;
        println!(
            "Dry run: {} untriaged, {} pre-filtered out, {} would be sent to {MODEL} \
             across {n_batches} request(s).",
            rejects.len() + candidates.len(),
            rejects.len(),
            candidates.len(),
        );
        println!(
            "Estimated tokens: ~{est_input_tokens} in, ~{est_output_tokens} out. \
             Estimated cost if you run it: ~${est_cost:.2} (upper bound — prompt caching makes real runs cheaper)."
        );
        println!("This was a dry run: no API request was made and nothing was charged.");
        return Ok(());
    }

    // Persist the code-side rejects without spending — score 0, never re-sent.
    // One bulk UPDATE, not a write per row: on a large re-triage this is thousands
    // of postings, and per-row writes each fsync, which otherwise stalls the run
    // silently for minutes before the first LLM batch.
    let now = Utc::now().to_rfc3339();
    if !rejects.is_empty() {
        let ids: Vec<i32> = rejects.iter().map(|p| p.id).collect();
        println!("Pre-filtering {} posting(s) by title (no API cost)…", ids.len());
        queries::set_triage_bulk(
            db,
            &ids,
            0,
            "pre-filtered: title matched obvious-reject pattern",
            "[\"prefilter_reject\"]",
            &now,
        )
        .await?;
        info!(
            count = rejects.len(),
            "pre-filtered postings marked score 0"
        );
    }

    if candidates.is_empty() {
        println!(
            "Triage complete: {} pre-filtered, 0 scored by LLM.",
            rejects.len()
        );
        return Ok(());
    }

    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .context("ANTHROPIC_API_KEY not set — required for triage (use --dry-run to preview)")?;
    let client = Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .context("building HTTP client")?;

    let batches: Vec<&[posting::Model]> = candidates.chunks(BATCH_SIZE).collect();
    let total_batches = batches.len();
    let total = candidates.len();
    println!(
        "Scoring {total} posting(s) in {total_batches} batch(es) via {MODEL}, up to \
         {SCORE_CONCURRENCY} at a time ({} pre-filtered already). Safe to Ctrl-C — \
         progress is saved per batch.",
        rejects.len()
    );

    let mut scored = 0u64;
    let mut failed = 0u64;
    let mut usage = Usage::default();
    let mut completed = 0usize;

    // Redraw the live progress line in place on stderr. Counts completed batches,
    // which under concurrency finish out of submission order — that's fine, it's a
    // "N of total done" indicator, not a position.
    let draw = |completed: usize, scored: u64, failed: u64| {
        eprint!(
            "\r  batch {completed}/{total_batches} · {}/{total} postings · {scored} scored, {failed} failed",
            scored + failed
        );
        let _ = io::stderr().flush();
    };

    // Apply one batch's outcome: sum usage and persist scores (serially), or count
    // the whole batch failed and leave it untriaged for a later run to retry.
    async fn absorb(
        db: &DatabaseConnection,
        now: &str,
        batch: &[posting::Model],
        res: Result<(Vec<Scored>, Usage)>,
        scored: &mut u64,
        failed: &mut u64,
        usage: &mut Usage,
    ) -> Result<()> {
        match res {
            Ok((results, batch_usage)) => {
                usage.add(&batch_usage);
                *scored += apply_scores(db, batch, &results, now).await?;
            }
            Err(e) => {
                *failed += batch.len() as u64;
                warn!(error = %e, batch = batch.len(), "batch scoring failed — leaving untriaged");
            }
        }
        Ok(())
    }

    // Send the first batch alone so it writes the ephemeral prompt cache; the
    // concurrent wave that follows then reads that cache at ~0.1x instead of every
    // request racing to write it and paying full input price.
    if let Some((first, rest)) = batches.split_first() {
        let res = score_batch(&client, &api_key, &system, first).await;
        absorb(db, &now, first, res, &mut scored, &mut failed, &mut usage).await?;
        completed += 1;
        draw(completed, scored, failed);

        let mut stream = stream::iter(rest.iter().copied())
            .map(|batch| {
                let client = &client;
                let api_key = &api_key;
                let system = &system;
                async move { (batch, score_batch(client, api_key, system, batch).await) }
            })
            .buffer_unordered(SCORE_CONCURRENCY);

        while let Some((batch, res)) = stream.next().await {
            absorb(db, &now, batch, res, &mut scored, &mut failed, &mut usage).await?;
            completed += 1;
            draw(completed, scored, failed);
        }
    }
    eprintln!();

    println!(
        "Triage complete: {} pre-filtered, {scored} scored, {failed} failed (left untriaged).",
        rejects.len()
    );
    println!(
        "Actual spend: ${:.4} ({} input, {} output, {} cache-write, {} cache-read tokens).",
        usage.cost_usd(),
        usage.input_tokens,
        usage.output_tokens,
        usage.cache_creation_input_tokens,
        usage.cache_read_input_tokens,
    );
    Ok(())
}

/// Apply a batch's returned scores to the DB, matched by `external_id`. Postings
/// the model omitted are left untriaged so a later run retries them.
async fn apply_scores(
    db: &DatabaseConnection,
    batch: &[posting::Model],
    results: &[Scored],
    now: &str,
) -> Result<u64> {
    let mut n = 0;
    for p in batch {
        let Some(r) = results.iter().find(|r| r.external_id == p.external_id) else {
            warn!(external_id = %p.external_id, "no score returned for posting");
            continue;
        };
        let raw = r.score.clamp(0, 10) as i32;
        let (score, reason) = enforce_location_gate(raw, &r.reason, &r.flags);
        let flags_json = serde_json::to_string(&r.flags).unwrap_or_else(|_| "[]".to_string());
        queries::set_triage(db, p.id, score, &reason, &flags_json, now).await?;
        n += 1;
    }
    Ok(n)
}

/// Enforce the rubric's hard location cap from the model's own flags. A fast model
/// frequently emits a location-failure flag (`no_location` / `hybrid_offsite`) yet
/// still returns a high score for the stack — this re-applies the cap so those
/// postings can't surface in the digest. Returns the (possibly lowered) score and
/// its reason, prefixed to make the code-side cap visible in `show` / the digest.
fn enforce_location_gate(score: i32, reason: &str, flags: &[String]) -> (i32, String) {
    let location_failed = flags
        .iter()
        .any(|f| LOCATION_FAIL_FLAGS.contains(&f.as_str()));
    if location_failed && score > LOCATION_FAIL_CAP {
        (LOCATION_FAIL_CAP, format!("[location gate] {reason}"))
    } else {
        (score, reason.to_string())
    }
}

/// Score one batch via the Messages API. Returns the parsed per-posting results
/// and the request's token usage (for spend reporting).
async fn score_batch(
    client: &Client,
    api_key: &str,
    system: &str,
    batch: &[posting::Model],
) -> Result<(Vec<Scored>, Usage)> {
    // The system prompt (rubric + full candidate profile) is byte-identical
    // across every batch in a run, so cache it — batches after the first read it
    // at ~0.1x instead of paying full input price each time.
    let body = json!({
        "model": MODEL,
        "max_tokens": MAX_TOKENS,
        "system": [{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        }],
        "messages": [{ "role": "user", "content": user_message(batch) }],
    });

    let resp = client
        .post(ANTHROPIC_URL)
        .header("x-api-key", api_key)
        .header("anthropic-version", ANTHROPIC_VERSION)
        .header("content-type", "application/json")
        .json(&body)
        .send()
        .await
        .context("sending request to Anthropic")?;

    let status = resp.status();
    let text = resp
        .text()
        .await
        .context("reading Anthropic response body")?;
    if !status.is_success() {
        bail!("Anthropic API returned {status}: {text}");
    }

    let parsed: AnthropicResponse =
        serde_json::from_str(&text).context("decoding Anthropic response envelope")?;
    if parsed.stop_reason.as_deref() == Some("refusal") {
        bail!("model refused to score this batch");
    }
    let raw = parsed
        .content
        .iter()
        .find(|b| b.kind == "text")
        .map(|b| b.text.as_str())
        .context("no text block in Anthropic response")?;

    let scores = parse_scores(raw).with_context(|| format!("parsing model output as JSON: {raw}"))?;
    Ok((scores, parsed.usage))
}

/// Parse the model's JSON array output, tolerating stray prose or code fences by
/// slicing to the outermost brackets.
fn parse_scores(raw: &str) -> Result<Vec<Scored>> {
    let trimmed = raw.trim();
    let start = trimmed.find('[').context("no JSON array in output")?;
    let end = trimmed
        .rfind(']')
        .context("no JSON array terminator in output")?;
    let slice = &trimmed[start..=end];
    Ok(serde_json::from_str(slice)?)
}

/// The system prompt: role, output contract, scoring rubric, and the candidate
/// profile loaded from profile.toml. Every candidate-specific fact — location
/// constraints, skills, seniority, and role preferences — is read by the model
/// from the embedded profile, so this prompt stays candidate-agnostic.
fn system_prompt(profile_text: &str) -> String {
    format!(
        r#"You are screening job postings against one candidate's profile. Score each posting
for fit and output JSON only — no prose, no markdown fences.

Return a JSON array with one object per posting, in this exact shape:
[{{"external_id": "...", "score": 0, "reason": "one sentence, max 20 words", "flags": ["priority_stack", "no_location", "hybrid_offsite"]}}]

The candidate's profile is embedded at the end of this prompt as TOML. Everything you need —
location constraints, skills, seniority, and role preferences — comes from it. Read it first.

HARD LOCATION GATE — apply this FIRST, before the rubric. From the profile's `[location]` block:
`base` is the candidate's home metro (they cannot relocate or commute beyond it), `acceptable`
lists the location arrangements they can take, and `work_authorization` is the country/region
where they are legally allowed to work. A posting's location is WORKABLE only if it matches one of
the `acceptable` arrangements — generally at least one of:
  - Onsite OR hybrid where the office is in the candidate's `base` metro, or
  - Fully remote within their `work_authorization` region, or
  - Fully remote that explicitly includes their `work_authorization` region (e.g. a multi-region
    remote role that names it).

HYBRID AND ONSITE ARE NOT REMOTE. "Hybrid" means mandatory in-office days and "onsite" means
full-time in-office, both at a specific office — so a hybrid OR onsite role is workable ONLY when
that office is in the candidate's `base` metro. A hybrid/onsite role tied to any other city is NOT
workable, because the candidate cannot commute there. This is the single most common mistake to
avoid: being in the SAME COUNTRY does NOT rescue a hybrid/onsite role. A hybrid or onsite office in
another city of the work-authorization country (for a Vancouver-based candidate: Toronto, Montreal,
Ottawa, Waterloo, etc.) FAILS the gate exactly like a foreign office does. "Canada remote" in the
`acceptable` list means fully-remote-anywhere-in-Canada; it does NOT mean "any office located in
Canada". Only a base-metro office or a fully-remote role in the authorized region qualifies.

When a posting lists SEVERAL cities with a hybrid/onsite arrangement (e.g. "San Francisco | Toronto
| New York | Montreal", hybrid), the candidate would have to be in ONE of those offices — none of
which is the base metro — so it FAILS the gate. Do not pass it just because one listed city is in
the authorized country. It passes ONLY if one listed option is the base metro, or one option is
genuine full remote covering the authorized region.

A location is NOT workable if the only options are: onsite or hybrid outside the `base` metro,
remote restricted to a region that excludes the candidate's `work_authorization`, or any country
where the candidate is not authorized to work — even when the role is otherwise partly remote.

If a posting lists several locations and ANY one is workable, treat it as workable and score
normally. If NONE is workable, you MUST cap the score at 3 and add the "no_location" flag (and, for
a hybrid or onsite role tied to an offsite metro, also the "hybrid_offsite" flag) — no matter how
strong the stack, seniority, or role fit is. Emitting the "no_location" flag whenever the location
fails is MANDATORY and non-optional: it is the machine-readable signal that the cap applies, and a
downstream check re-enforces the cap from it, so a failed location with a high score and no flag is
a contradiction you must never produce. Only when the location is genuinely unstated should you skip
the cap and score on the other axes.

Scoring rubric (0-10), applied only after a posting passes the location gate. Judge fit from the
profile's `[preferences]` (`priority` is the highest-value target, `also_strong` is a genuine
second specialty, `avoid` lists dealbreakers), `[skills]`, and the candidate's seniority
(`seniority_band`, `years_experience`):
- 9-10: A role squarely in the candidate's `priority` area at or near their experience level, or a
  role that explicitly names their `priority` stack. Do not down-rank a priority-stack role for
  wanting commercial experience the profile shows the candidate already has.
- 7-8: A strong match on the candidate's core `[skills]` within their seniority band, or a role in
  their `also_strong` secondary specialty. (Location is already confirmed workable by the gate.)
  `also_strong` is a SPECIFIC stack (for this candidate, Rust systems work) — not "any backend or
  infrastructure role". A role whose primary language is one the candidate does not work in
  (e.g. Python, Go, Scala, Java — anything absent from `[skills].languages`) is NOT an `also_strong`
  match just because it is backend/infra/platform work; treat a primary language the candidate lacks
  as a stack mismatch, not a strength.
- 4-6: Plausible but mismatched on one axis (slightly senior, adjacent stack, or a backend role in a
  language the candidate does not use but could plausibly pick up).
- 0-3: Wrong discipline, wrong seniority by 5+ years, hits an `avoid` dealbreaker the candidate
  cannot satisfy (e.g. clearance or citizenship they lack), or location failed the gate above.

Useful flags (include any that apply, free-form): priority_stack, also_strong, senior_only,
requires_citizenship, no_location, hybrid_offsite (hybrid role tied to an offsite metro), contract,
remote.

CANDIDATE PROFILE (TOML):
{profile_text}"#
    )
}

/// Build the user message: a compact, numbered list of the batch's postings.
fn user_message(batch: &[posting::Model]) -> String {
    let mut s = String::from("Score these postings:\n\n");
    for p in batch {
        let desc: String = p.description.chars().take(DESC_LIMIT).collect();
        let location = p.location.as_deref().unwrap_or("unspecified");
        let remote = p.remote.as_deref().unwrap_or("unknown");
        s.push_str(&format!(
            "---\nexternal_id: {}\ntitle: {}\nlocation: {} ({})\ndescription: {}\n",
            p.external_id, p.title, location, remote, desc
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{enforce_location_gate, LOCATION_FAIL_CAP};

    fn flags(fs: &[&str]) -> Vec<String> {
        fs.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_location_flag_forces_the_cap_even_on_a_high_score() {
        let (score, reason) = enforce_location_gate(8, "great AI fit", &flags(&["no_location"]));
        assert_eq!(score, LOCATION_FAIL_CAP);
        assert!(reason.starts_with("[location gate]"));
    }

    #[test]
    fn hybrid_offsite_flag_also_forces_the_cap() {
        let (score, _) =
            enforce_location_gate(9, "Toronto hybrid", &flags(&["priority_stack", "hybrid_offsite"]));
        assert_eq!(score, LOCATION_FAIL_CAP);
    }

    #[test]
    fn a_workable_posting_is_left_untouched() {
        let (score, reason) =
            enforce_location_gate(9, "Vancouver, priority stack", &flags(&["priority_stack"]));
        assert_eq!(score, 9);
        assert_eq!(reason, "Vancouver, priority stack");
    }

    #[test]
    fn an_already_capped_score_keeps_its_original_reason() {
        // The flag is present but the score already honours the cap — don't relabel.
        let (score, reason) = enforce_location_gate(2, "SF onsite", &flags(&["no_location"]));
        assert_eq!(score, 2);
        assert_eq!(reason, "SF onsite");
    }
}
