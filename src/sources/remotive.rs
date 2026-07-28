//! Remotive: `https://remotive.com/api/remote-jobs` (aggregator — slug ignored).
//!
//! Response shape (verified live): `{ jobs: [ ... ] }`, each job with `id`
//! (int), `url`, `title`, `company_name`, `candidate_required_location`,
//! `description` (HTML). Every posting is remote.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct Remotive {
    client: Client,
}

impl Remotive {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct RmvResponse {
    #[serde(default)]
    jobs: Vec<RmvJob>,
}

#[derive(Deserialize)]
struct RmvJob {
    id: i64,
    title: String,
    #[serde(default)]
    company_name: Option<String>,
    #[serde(default)]
    candidate_required_location: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[async_trait]
impl JobSource for Remotive {
    fn ats(&self) -> &'static str {
        "remotive"
    }

    async fn fetch(&self, _slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let resp = self
            .client
            .get("https://remotive.com/api/remote-jobs")
            .send()
            .await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Err(FetchError::NotFound),
            other => {
                return Err(FetchError::Status {
                    code: other.as_u16(),
                })
            }
        }

        let body: RmvResponse = resp.json().await?;
        let postings = body
            .jobs
            .into_iter()
            .map(|j| {
                let title = match j.company_name {
                    Some(c) if !c.is_empty() => format!("{c} — {}", j.title),
                    _ => j.title,
                };
                RawPosting {
                    external_id: j.id.to_string(),
                    title,
                    location: j.candidate_required_location.filter(|l| !l.is_empty()),
                    description: j.description.unwrap_or_default(),
                    apply_url: j.url.unwrap_or_default(),
                    remote: Some("remote".to_string()),
                }
            })
            .collect();
        Ok(postings)
    }
}
