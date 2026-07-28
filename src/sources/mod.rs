//! Job sources. The [`JobSource`] trait is the key seam: adding an ATS means one
//! new file here and one line in [`source_for`].

pub mod ashby;
pub mod greenhouse;
pub mod hn;
pub mod lever;
pub mod recruitee;
pub mod remoteok;
pub mod remotive;
pub mod workable;

use async_trait::async_trait;
use reqwest::Client;
use std::time::Duration;

/// The User-Agent every request identifies itself with. Per the spec: be polite,
/// no evasion — announce the tool honestly.
pub const USER_AGENT: &str = concat!(
    "jobpipe/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/jobpipe; personal job search)"
);

/// A posting as it comes off an ATS board, before normalization. Fields are the
/// lowest common denominator across ATS schemas; `description` may contain HTML.
#[derive(Debug, Clone)]
pub struct RawPosting {
    pub external_id: String,
    pub title: String,
    pub location: Option<String>,
    pub description: String,
    pub apply_url: String,
    /// A canonical remote flag when the ATS states it explicitly
    /// (remote | hybrid | onsite). `None` lets normalize infer from location.
    pub remote: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("http request failed: {0}")]
    Request(#[from] reqwest::Error),
    /// Board not found (404) — company likely changed ATS.
    #[error("board not found (404)")]
    NotFound,
    #[error("board returned unexpected status {code}")]
    Status { code: u16 },
}

#[async_trait]
pub trait JobSource: Send + Sync {
    /// The ATS identifier this source handles (`greenhouse`, `lever`, ...).
    /// Used by later phases (e.g. `--only <ats>` verification and sniffing).
    #[allow(dead_code)]
    fn ats(&self) -> &'static str;

    /// Fetch every open posting for `slug`. An empty `Vec` and a `NotFound`
    /// error are both signals the caller treats as "needs review".
    async fn fetch(&self, slug: &str) -> Result<Vec<RawPosting>, FetchError>;
}

/// Build the shared HTTP client used by every source.
pub fn http_client() -> Client {
    Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .expect("building reqwest client")
}

/// Registry: map an ATS name to its source. Phase 1 handles Greenhouse only;
/// later phases add one arm each.
pub fn source_for(ats: &str, client: &Client) -> Option<Box<dyn JobSource>> {
    match ats {
        "greenhouse" => Some(Box::new(greenhouse::Greenhouse::new(client.clone()))),
        "lever" => Some(Box::new(lever::Lever::new(client.clone()))),
        "ashby" => Some(Box::new(ashby::Ashby::new(client.clone()))),
        "workable" => Some(Box::new(workable::Workable::new(client.clone()))),
        "recruitee" => Some(Box::new(recruitee::Recruitee::new(client.clone()))),
        "remoteok" => Some(Box::new(remoteok::RemoteOk::new(client.clone()))),
        "remotive" => Some(Box::new(remotive::Remotive::new(client.clone()))),
        "hn-whoishiring" => Some(Box::new(hn::HnWhoIsHiring::new(client.clone()))),
        _ => None,
    }
}
