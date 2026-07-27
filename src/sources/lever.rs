//! Lever: `https://api.lever.co/v0/postings/{slug}?mode=json`
//!
//! Response shape (verified live): a **bare array** of postings, each with
//! `id`, `text` (title), `categories` (`{ location, allLocations, commitment }`),
//! `workplaceType` (remote|hybrid|on-site), `country`, `applyUrl`/`hostedUrl`,
//! and body fields `description`/`descriptionPlain`, `lists[]`, `additional`.

use async_trait::async_trait;
use reqwest::{Client, StatusCode};
use serde::Deserialize;

use super::{FetchError, JobSource, RawPosting};

pub struct Lever {
    client: Client,
}

impl Lever {
    pub fn new(client: Client) -> Self {
        Self { client }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LvPosting {
    id: String,
    text: String,
    #[serde(default)]
    categories: LvCategories,
    #[serde(default)]
    workplace_type: Option<String>,
    #[serde(default)]
    country: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    lists: Vec<LvList>,
    #[serde(default)]
    additional: Option<String>,
    /// Some boards misplace the body here; used only as a fallback.
    #[serde(default)]
    salary_description: Option<String>,
    #[serde(default)]
    apply_url: Option<String>,
    #[serde(default)]
    hosted_url: Option<String>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LvCategories {
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    all_locations: Vec<String>,
}

#[derive(Deserialize)]
struct LvList {
    #[serde(default)]
    content: String,
}

fn canon_remote(workplace_type: Option<&str>) -> Option<String> {
    match workplace_type.map(|w| w.to_lowercase()) {
        Some(w) if w.contains("remote") => Some("remote".to_string()),
        Some(w) if w.contains("hybrid") => Some("hybrid".to_string()),
        Some(w) if w.contains("site") || w.contains("office") => Some("onsite".to_string()),
        _ => None,
    }
}

#[async_trait]
impl JobSource for Lever {
    fn ats(&self) -> &'static str {
        "lever"
    }

    async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>, FetchError> {
        let url = format!("https://api.lever.co/v0/postings/{slug}?mode=json");
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

        let jobs: Vec<LvPosting> = resp.json().await?;
        let postings = jobs
            .into_iter()
            .map(|j| {
                // Location: prefer the explicit list, else the single field, then country.
                let location = if !j.categories.all_locations.is_empty() {
                    Some(j.categories.all_locations.join(" | "))
                } else {
                    j.categories.location.clone().or(j.country.clone())
                };

                // Body: intro + each list section + closing, all HTML.
                let mut body = j.description.unwrap_or_default();
                for list in &j.lists {
                    body.push_str(&list.content);
                }
                if let Some(a) = &j.additional {
                    body.push_str(a);
                }
                if body.trim().is_empty() {
                    body = j.salary_description.unwrap_or_default();
                }

                RawPosting {
                    external_id: j.id,
                    title: j.text,
                    location,
                    description: body,
                    apply_url: j.apply_url.or(j.hosted_url).unwrap_or_default(),
                    remote: canon_remote(j.workplace_type.as_deref()),
                }
            })
            .collect();
        Ok(postings)
    }
}
