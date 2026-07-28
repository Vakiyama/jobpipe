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
    let first_line = text.split('|').next().unwrap_or(&text).trim();
    let base = if first_line.len() >= 15 {
        first_line
    } else {
        &text
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
