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
use std::sync::OnceLock;
use std::time::Duration;
use tracing::{info, warn};

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

/// Obvious-reject title pattern. A matching title is dropped *before* the LLM
/// call unless its description mentions Rust. Word boundaries keep it from
/// firing on substrings like "Salesforce".
fn reject_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(Senior Staff|Principal|Director|VP|Manager|Intern|Recruiter|Sales|Marketing)\b")
            .expect("valid reject regex")
    })
}

fn mentions_rust(text: &str) -> bool {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\brust\b").expect("valid rust regex"))
        .is_match(text)
}

/// True if the posting should be dropped by the code-side pre-filter.
fn is_prefilter_reject(p: &posting::Model) -> bool {
    reject_re().is_match(&p.title) && !mentions_rust(&p.description)
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
) -> Result<()> {
    let postings = queries::untriaged_postings(db, limit).await?;
    if postings.is_empty() {
        println!("No untriaged postings. Run `jobpipe fetch` first.");
        return Ok(());
    }

    // Split into code-side rejects and LLM candidates.
    let mut rejects = Vec::new();
    let mut candidates = Vec::new();
    for p in postings {
        if is_prefilter_reject(&p) {
            rejects.push(p);
        } else {
            candidates.push(p);
        }
    }

    let system = system_prompt(profile_text);

    if dry_run {
        let batches = candidates.chunks(BATCH_SIZE);
        let n_batches = batches.len();
        let mut est_tokens = 0usize;
        for batch in candidates.chunks(BATCH_SIZE) {
            // ~4 chars/token is the standard rough approximation.
            est_tokens += (system.len() + user_message(batch).len()) / 4;
        }
        println!(
            "Dry run: {} untriaged, {} pre-filtered out, {} would be sent to {MODEL} \
             across {n_batches} request(s).",
            rejects.len() + candidates.len(),
            rejects.len(),
            candidates.len(),
        );
        println!("Estimated input tokens: ~{est_tokens} (output extra). No spend.");
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
    for (i, batch) in candidates.chunks(BATCH_SIZE).enumerate() {
        match score_batch(&client, &api_key, &system, batch).await {
            Ok(results) => {
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

/// Score one batch via the Messages API. Returns the parsed per-posting results.
async fn score_batch(
    client: &Client,
    api_key: &str,
    system: &str,
    batch: &[posting::Model],
) -> Result<Vec<Scored>> {
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

    parse_scores(raw).with_context(|| format!("parsing model output as JSON: {raw}"))
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
/// profile loaded from profile.toml.
fn system_prompt(profile_text: &str) -> String {
    format!(
        r#"You are screening job postings against one candidate's profile. Score each posting
for fit and output JSON only — no prose, no markdown fences.

Return a JSON array with one object per posting, in this exact shape:
[{{"external_id": "...", "score": 0, "reason": "one sentence, max 20 words", "flags": ["rust", "no_canada", "contract"]}}]

Scoring rubric (0-10):
- 9-10: Rust role at or near this experience level, or a backend/full-stack role that explicitly
  mentions Rust in the stack. The candidate has PRODUCTION Rust (a shipped, in-use system), not
  just side projects — do not down-rank Rust roles for lack of commercial experience.
- 7-8: Strong TS/React/Node full-stack or frontend role in the 2-5 YOE band with a workable
  location; or an AI/LLM application-engineering role (agent orchestration, tool use, LLM API
  integration), which is a genuine second specialty here.
- 4-6: Plausible but mismatched on one axis (slightly senior, adjacent stack, unclear location).
- 0-3: Wrong discipline, wrong seniority by 5+ years, requires clearance/citizenship the candidate
  doesn't have, or location is not workable.

Useful flags (include any that apply, free-form): requires_citizenship, senior_only, rust,
no_canada, contract, remote, ai_llm.

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
