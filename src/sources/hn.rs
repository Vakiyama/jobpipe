//! Hacker News "Ask HN: Who is hiring?" via the Algolia API (aggregator — the
//! slug is ignored). Disproportionately good for Rust roles.
//!
//! Two steps (both verified live):
//! 1. `search_by_date?tags=story,author_whoishiring` → newest monthly thread.
//! 2. `items/{objectID}` → top-level comments, each a job posting (freeform
//!    HTML). We derive a title from the first line; the full comment is the
//!    description for triage; the apply URL is the HN comment permalink.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};
use crate::normalize;

pub struct HnWhoIsHiring {
    client: Client,
}

impl HnWhoIsHiring {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    hits: Vec<SearchHit>,
}

#[derive(Deserialize)]
struct SearchHit {
    #[serde(rename = "objectID")]
    object_id: String,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct Item {
    #[serde(default)]
    children: Vec<Comment>,
}

#[derive(Deserialize)]
struct Comment {
    id: i64,
    #[serde(default)]
    text: Option<String>,
}

async fn get_json<T: for<'de> Deserialize<'de>>(
    client: &Client,
    url: &str,
) -> Result<T, FetchError> {
    let resp = client.get(url).send().await?;
    match resp.status() {
        StatusCode::OK => Ok(resp.json().await?),
        StatusCode::NOT_FOUND => Err(FetchError::NotFound),
        other => Err(FetchError::Status {
            code: other.as_u16(),
        }),
    }
}

/// A ~110-char single-line title from the comment's first line of text.
fn derive_title(html: &str) -> String {
    let text = normalize::clean_html(html);
    // Only the first non-empty line is title material; the rest is the job
    // description and must never leak in, or the title wraps across several rows
    // in `track` / the apply picker.
    let first_line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    // HN posts usually lead "Company | Location | Role | …"; the piece before the
    // first pipe reads best as a title, but fall back to the whole first line
    // when that piece is too short to stand on its own.
    let head = first_line.split('|').next().unwrap_or(first_line).trim();
    let base = if head.chars().count() >= 15 {
        head
    } else {
        first_line
    };
    let mut title: String = base.chars().take(110).collect();
    if base.chars().count() > 110 {
        title.push('…');
    }
    title
}

#[async_trait]
impl JobSource for HnWhoIsHiring {
    fn ats(&self) -> &'static str {
        "hn-whoishiring"
    }

    async fn fetch(&self, _slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let search: Search = get_json(
            &self.client,
            "https://hn.algolia.com/api/v1/search_by_date?tags=story,author_whoishiring&hitsPerPage=8",
        )
        .await?;

        // Newest "Who is hiring?" thread (skip the "wants to be hired" siblings).
        let Some(thread) = search.hits.into_iter().find(|h| {
            h.title
                .as_deref()
                .map(|t| {
                    let t = t.to_lowercase();
                    t.contains("who is hiring") && !t.contains("wants to be hired")
                })
                .unwrap_or(false)
        }) else {
            // No thread found — treat like an empty board.
            return Ok(Vec::new());
        };

        let item: Item = get_json(
            &self.client,
            &format!("https://hn.algolia.com/api/v1/items/{}", thread.object_id),
        )
        .await?;

        let postings = item
            .children
            .into_iter()
            .filter_map(|c| {
                let text = c.text.filter(|t| !t.trim().is_empty())?;
                Some(RawPosting {
                    external_id: c.id.to_string(),
                    title: derive_title(&text),
                    location: None, // freeform; the LLM reads it from the description
                    description: text,
                    apply_url: format!("https://news.ycombinator.com/item?id={}", c.id),
                    remote: None,
                })
            })
            .collect();
        Ok(postings)
    }
}

#[cfg(test)]
mod tests {
    use super::derive_title;

    #[test]
    fn substantial_pipe_head_stands_alone_as_the_title() {
        let html = "<p>Fastly Edge Cloud | San Francisco | Senior Rust Engineer | Remote</p>\
                    <p>We build a global edge network. Apply within.</p>";
        assert_eq!(derive_title(html), "Fastly Edge Cloud");
    }

    #[test]
    fn short_head_falls_back_to_the_whole_first_line_not_the_body() {
        // "Acme" alone is uninformative, so keep the rest of the first line —
        // but never spill into the second paragraph (that's the description).
        let html = "<p>Acme | Berlin | Backend Engineer (Go)</p>\
                    <p>Series B. We ship daily and pay well.</p>";
        assert_eq!(derive_title(html), "Acme | Berlin | Backend Engineer (Go)");
    }

    #[test]
    fn title_never_spans_multiple_lines() {
        let html = "<p>Widgets Inc — we are hiring</p>\
                    <p>Location: remote</p><p>Role: platform</p>";
        assert!(!derive_title(html).contains('\n'));
    }
}
