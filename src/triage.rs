//! LLM scoring pass. Untriaged postings are first run through a cheap code-side
//! pre-filter (obvious-reject titles), then the survivors are batched to the
//! Anthropic Messages API for scoring against the candidate profile.
//!
//! The API key is read from `ANTHROPIC_API_KEY`. The model is a fast/cheap tier
//! (Haiku) — this is classification, not reasoning. Only postings with
//! `triaged_at IS NULL` are ever sent; a posting is never re-scored.

use anyhow::{bail, Context, Result};
use chrono::Utc;
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
const MAX_TOKENS: u32 = 4096;
/// Descriptions are truncated before they hit the prompt to bound token spend.
const DESC_LIMIT: usize = 1200;

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
    let now = Utc::now().to_rfc3339();
    for p in &rejects {
        queries::set_triage(
            db,
            p.id,
            0,
            "pre-filtered: title matched obvious-reject pattern",
            "[\"prefilter_reject\"]",
            &now,
        )
        .await?;
    }
    if !rejects.is_empty() {
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

    let total_batches = candidates.chunks(BATCH_SIZE).count();
    let total = candidates.len();
    println!(
        "Scoring {total} posting(s) in {total_batches} batch(es) via {MODEL} \
         ({} pre-filtered already). Safe to Ctrl-C — progress is saved per batch.",
        rejects.len()
    );

    let mut scored = 0u64;
    let mut failed = 0u64;
    let mut usage = Usage::default();
    for (i, batch) in candidates.chunks(BATCH_SIZE).enumerate() {
        match score_batch(&client, &api_key, &system, batch).await {
            Ok((results, batch_usage)) => {
                usage.add(&batch_usage);
                scored += apply_scores(db, batch, &results, &now).await?;
            }
            Err(e) => {
                failed += batch.len() as u64;
                warn!(error = %e, batch = batch.len(), "batch scoring failed — leaving untriaged");
            }
        }
        // Live progress line, redrawn in place on stderr.
        let done = scored + failed;
        eprint!(
            "\r  batch {}/{total_batches} · {done}/{total} postings · {scored} scored, {failed} failed",
            i + 1
        );
        let _ = io::stderr().flush();
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
        let score = r.score.clamp(0, 10) as i32;
        let flags_json = serde_json::to_string(&r.flags).unwrap_or_else(|_| "[]".to_string());
        queries::set_triage(db, p.id, score, &r.reason, &flags_json, now).await?;
        n += 1;
    }
    Ok(n)
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

HYBRID IS NOT REMOTE. "Hybrid" means mandatory in-office days at a specific office, so a hybrid
role is workable ONLY when that office is in the candidate's `base` metro. A hybrid role tied to
any other city — even another city inside their work-authorization country — is NOT workable,
because the candidate cannot commute there. Being in the right country does NOT rescue a hybrid
role; only a base-metro hybrid or a fully-remote role in the authorized region qualifies.

A location is NOT workable if the only options are: onsite or hybrid outside the `base` metro,
remote restricted to a region that excludes the candidate's `work_authorization`, or any country
where the candidate is not authorized to work — even when the role is otherwise partly remote.

If a posting lists several locations and ANY one is workable, treat it as workable and score
normally. If NONE is workable, you MUST cap the score at 3 and add the "no_location" flag (and, for
a hybrid role tied to an offsite metro, also the "hybrid_offsite" flag) — no matter how strong the
stack, seniority, or role fit is. Only when the location is genuinely unstated should you skip the
cap and score on the other axes.

Scoring rubric (0-10), applied only after a posting passes the location gate. Judge fit from the
profile's `[preferences]` (`priority` is the highest-value target, `also_strong` is a genuine
second specialty, `avoid` lists dealbreakers), `[skills]`, and the candidate's seniority
(`seniority_band`, `years_experience`):
- 9-10: A role squarely in the candidate's `priority` area at or near their experience level, or a
  role that explicitly names their `priority` stack. Do not down-rank a priority-stack role for
  wanting commercial experience the profile shows the candidate already has.
- 7-8: A strong match on the candidate's core `[skills]` within their seniority band, or a role in
  their `also_strong` secondary specialty. (Location is already confirmed workable by the gate.)
- 4-6: Plausible but mismatched on one axis (slightly senior, adjacent stack).
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
