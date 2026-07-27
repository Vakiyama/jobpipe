//! Ashby: `https://api.ashbyhq.com/posting-api/job-board/{slug}`
//!
//! Response shape (verified live): `{ "jobs": [ ... ], "apiVersion": ... }`.
//! Each job has `id`, `title`, `location`, `secondaryLocations[].location`
//! (where "Remote (Canada)" often lives — merged so the location gate sees it),
//! `isRemote`, `workplaceType`, `descriptionHtml`, `applyUrl`.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct Ashby {
    client: Client,
}

impl Ashby {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
struct AshbyResponse {
    #[serde(default)]
    jobs: Vec<AshbyJob>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AshbyJob {
    id: String,
    title: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    secondary_locations: Vec<AshbySecondary>,
    // Option, not bool: Ashby sends explicit `null` here on some postings, and
    // `#[serde(default)]` only covers absent fields — a null into `bool` fails
    // the whole board's decode.
    #[serde(default)]
    is_remote: Option<bool>,
    #[serde(default)]
    workplace_type: Option<String>,
    #[serde(default)]
    description_html: Option<String>,
    #[serde(default)]
    apply_url: Option<String>,
    #[serde(default)]
    job_url: Option<String>,
}

#[derive(Deserialize)]
struct AshbySecondary {
    #[serde(default)]
    location: Option<String>,
}

#[async_trait]
impl JobSource for Ashby {
    fn ats(&self) -> &'static str {
        "ashby"
    }

    async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let url = format!("https://api.ashbyhq.com/posting-api/job-board/{slug}");
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

        let body: AshbyResponse = resp.json().await?;
        let postings = body
            .jobs
            .into_iter()
            .map(|j| {
                // Merge primary + secondary locations so remote-in-Canada surfaces.
                let mut locs: Vec<String> = j.location.into_iter().collect();
                locs.extend(j.secondary_locations.into_iter().filter_map(|s| s.location));
                let location = if locs.is_empty() {
                    None
                } else {
                    Some(locs.join(" | "))
                };

                let remote = match j.workplace_type.as_deref().map(str::to_lowercase) {
                    Some(w) if w.contains("hybrid") => Some("hybrid".to_string()),
                    Some(w) if w.contains("remote") => Some("remote".to_string()),
                    Some(w) if w.contains("site") => Some("onsite".to_string()),
                    _ if j.is_remote.unwrap_or(false) => Some("remote".to_string()),
                    _ => None,
                };

                RawPosting {
                    external_id: j.id,
                    title: j.title,
                    location,
                    description: j.description_html.unwrap_or_default(),
                    apply_url: j.apply_url.or(j.job_url).unwrap_or_default(),
                    remote,
                }
            })
            .collect();
        Ok(postings)
    }
}
