//! Recruitee: `https://{slug}.recruitee.com/api/offers/`
//!
//! Response shape (verified live): `{ offers: [ ... ] }`. Each offer has `id`,
//! `title`, `location`, `remote`/`on_site`/`hybrid` (bools), `description`
//! (HTML), `careers_apply_url`/`careers_url`.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct Recruitee {
    client: Client,
}

impl Recruitee {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct RcResponse {
    #[serde(default)]
    offers: Vec<RcOffer>,
}

#[derive(Deserialize)]
struct RcOffer {
    id: i64,
    title: String,
    #[serde(default)]
    location: Option<String>,
    // Option, not bool: these can arrive as explicit `null`, which a bare `bool`
    // field would reject (serde `default` only covers absent fields).
    #[serde(default)]
    remote: Option<bool>,
    #[serde(default)]
    hybrid: Option<bool>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    careers_apply_url: Option<String>,
    #[serde(default)]
    careers_url: Option<String>,
}

#[async_trait]
impl JobSource for Recruitee {
    fn ats(&self) -> &'static str {
        "recruitee"
    }

    async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let url = format!("https://{slug}.recruitee.com/api/offers/");
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

        let body: RcResponse = resp.json().await?;
        let postings = body
            .offers
            .into_iter()
            .map(|o| {
                let remote = if o.remote.unwrap_or(false) {
                    Some("remote".to_string())
                } else if o.hybrid.unwrap_or(false) {
                    Some("hybrid".to_string())
                } else {
                    None
                };
                RawPosting {
                    external_id: o.id.to_string(),
                    title: o.title,
                    location: o.location,
                    description: o.description.unwrap_or_default(),
                    apply_url: o.careers_apply_url.or(o.careers_url).unwrap_or_default(),
                    remote,
                }
            })
            .collect();
        Ok(postings)
    }
}
