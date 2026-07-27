//! Greenhouse: `https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=true`
//!
//! Response shape (verified live): `{ "jobs": [ { id, title, absolute_url,
//! location: { name }, content } ] }`. `content` is HTML with the entities
//! escaped once (`&lt;p&gt;`), so we decode entities here; tag stripping happens
//! in `normalize`.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct Greenhouse {
    client: Client,
}

impl Greenhouse {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct GhResponse {
    jobs: Vec<GhJob>,
}

#[derive(Deserialize)]
struct GhJob {
    id: i64,
    title: String,
    absolute_url: String,
    #[serde(default)]
    location: Option<GhLocation>,
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct GhLocation {
    #[serde(default)]
    name: Option<String>,
}

#[async_trait]
impl JobSource for Greenhouse {
    fn ats(&self) -> &'static str {
        "greenhouse"
    }

    async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let url = format!("https://boards-api.greenhouse.io/v1/boards/{slug}/jobs?content=true");
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

        let body: GhResponse = resp.json().await?;
        let postings = body
            .jobs
            .into_iter()
            .map(|j| {
                let description = j
                    .content
                    .map(|c| html_escape::decode_html_entities(&c).into_owned())
                    .unwrap_or_default();
                RawPosting {
                    external_id: j.id.to_string(),
                    title: j.title,
                    location: j.location.and_then(|l| l.name),
                    description,
                    apply_url: j.absolute_url,
                }
            })
            .collect();
        Ok(postings)
    }
}
