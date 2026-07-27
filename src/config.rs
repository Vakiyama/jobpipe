//! Loading of `companies.toml` (the seed list). `profile.toml` parsing lands in
//! phase 2 with triage.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::db::queries::SeedCompany;

#[derive(Debug, Deserialize)]
struct CompaniesFile {
    #[serde(default, rename = "companies")]
    companies: Vec<CompanyEntry>,
}

#[derive(Debug, Deserialize)]
struct CompanyEntry {
    name: String,
    ats: String,
    slug: String,
    #[serde(default)]
    careers_url: Option<String>,
    #[serde(default)]
    tags: Option<String>,
}

/// Parse `companies.toml` at `path` into seed rows.
pub fn load_companies(path: &Path) -> Result<Vec<SeedCompany>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading company seed file {}", path.display()))?;
    let parsed: CompaniesFile =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed
        .companies
        .into_iter()
        .map(|c| SeedCompany {
            name: c.name,
            ats: c.ats,
            slug: c.slug,
            careers_url: c.careers_url,
            tags: c.tags,
        })
        .collect())
}

/// Load the raw `profile.toml` text for embedding in the triage prompt. We pass
/// the TOML through verbatim rather than reshaping it — the candidate profile is
/// human-authored context for the model, and every field is relevant.
pub fn load_profile_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading candidate profile {}", path.display()))
}
