//! RemoteOK: `https://remoteok.com/api` (aggregator — the slug is ignored).
//!
//! Response shape (verified live): a JSON array whose first element is a
//! `{ last_updated, legal }` notice, followed by postings with `id` (string),
//! `company`, `position`, `location`, `tags`, `description` (HTML), `apply_url`,
//! `url`. Every posting is remote by definition.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct RemoteOk {
    client: Client,
}

impl RemoteOk {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

/// All fields optional so the leading legal-notice element deserializes to an
/// all-`None` row that we filter out by the absent `id`.
#[derive(Deserialize)]
struct RokEntry {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    company: Option<String>,
    #[serde(default)]
    position: Option<String>,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    apply_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[async_trait]
impl JobSource for RemoteOk {
    fn ats(&self) -> &'static str {
        "remoteok"
    }

    async fn fetch(&self, _slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let resp = self.client.get("https://remoteok.com/api").send().await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Err(FetchError::NotFound),
            other => {
                return Err(FetchError::Status {
                    code: other.as_u16(),
                })
            }
        }

        let entries: Vec<RokEntry> = resp.json().await?;
        let postings = entries
            .into_iter()
            .filter_map(|e| {
                let id = e.id?;
                let position = e.position?;
                let title = match e.company {
                    Some(c) if !c.is_empty() => format!("{c} — {position}"),
                    _ => position,
                };
                Some(RawPosting {
                    external_id: id,
                    title,
                    location: e.location.filter(|l| !l.is_empty()),
                    description: e.description.unwrap_or_default(),
                    apply_url: e.apply_url.or(e.url).unwrap_or_default(),
                    remote: Some("remote".to_string()),
                })
            })
            .collect();
        Ok(postings)
    }
}
