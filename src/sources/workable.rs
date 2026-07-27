//! Workable: `https://apply.workable.com/api/v1/widget/accounts/{slug}?details=true`
//!
//! Response shape (verified live): `{ name, description, jobs: [ ... ] }`. Each
//! job has `shortcode` (id), `title`, `city`/`state`/`country`, `locations[]`,
//! `telecommuting` (bool), `description` (HTML), `application_url`/`url`.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct Workable {
    client: Client,
}

impl Workable {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct WkResponse {
    #[serde(default)]
    jobs: Vec<WkJob>,
}

#[derive(Deserialize)]
struct WkJob {
    shortcode: String,
    title: String,
    #[serde(default)]
    city: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    country: Option<String>,
    // Option, not bool: guards against an explicit `null` failing decode.
    #[serde(default)]
    telecommuting: Option<bool>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    application_url: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

#[async_trait]
impl JobSource for Workable {
    fn ats(&self) -> &'static str {
        "workable"
    }

    async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let url = format!("https://apply.workable.com/api/v1/widget/accounts/{slug}?details=true");
        let resp = self.client.get(&url).send().await?;
        match resp.status() {
            StatusCode::OK => {}
            StatusCode::NOT_FOUND => return Err(FetchError::NotFound),
            other => {
                return Err(FetchError::Status {
                    code: other.as_u16(),
                })
            }
        }

        let body: WkResponse = resp.json().await?;
        let postings = body
            .jobs
            .into_iter()
            .map(|j| {
                let parts: Vec<String> = [j.city, j.state, j.country]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect();
                let location = if parts.is_empty() {
                    None
                } else {
                    Some(parts.join(", "))
                };
                let remote = j
                    .telecommuting
                    .unwrap_or(false)
                    .then(|| "remote".to_string());

                RawPosting {
                    external_id: j.shortcode,
                    title: j.title,
                    location,
                    description: j.description.unwrap_or_default(),
                    apply_url: j.application_url.or(j.url).unwrap_or_default(),
                    remote,
                }
            })
            .collect();
        Ok(postings)
    }
}
