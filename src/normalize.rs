//! `RawPosting` -> `NormalizedPosting`: strip HTML from the description and
//! canonicalize the location into a coarse remote flag.

use crate::sources::RawPosting;

/// A posting after edge normalization, ready to upsert.
#[derive(Debug, Clone)]
pub struct NormalizedPosting {
    pub external_id: String,
    pub title: String,
    pub location: Option<String>,
    /// remote | hybrid | onsite | unknown
    pub remote: String,
    pub description: String,
    pub apply_url: String,
}

pub fn normalize(raw: RawPosting) -> NormalizedPosting {
    let location = raw
        .location
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty());
    let remote = infer_remote(location.as_deref());
    NormalizedPosting {
        external_id: raw.external_id,
        title: raw.title.trim().to_string(),
        location,
        remote,
        description: strip_html(&raw.description),
        apply_url: raw.apply_url,
    }
}

/// Coarse remote classification from a location string. Phase 1 keeps this cheap;
/// the description isn't consulted yet.
fn infer_remote(location: Option<&str>) -> String {
    match location {
        None => "unknown".to_string(),
        Some(loc) => {
            let l = loc.to_lowercase();
            if l.contains("hybrid") {
                "hybrid".to_string()
            } else if l.contains("remote") {
                "remote".to_string()
            } else {
                "onsite".to_string()
            }
        }
    }
}

/// Strip HTML tags and collapse whitespace, yielding readable plain text.
/// Entities are assumed already decoded by the source.
fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if in_tag => {}
            c => out.push(c),
        }
    }
    // Collapse runs of whitespace to single spaces.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}
