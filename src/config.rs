//! Loading of `companies.toml` (the seed list). `profile.toml` parsing lands in
//! phase 2 with triage.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::path::Path;

use crate::db::queries::SeedCompany;

/// Default company seed list, baked into the binary so `init` works with no
/// `companies.toml` on disk — e.g. `nix run github:.../jobpipe -- init`.
pub const DEFAULT_COMPANIES_TOML: &str = include_str!("../companies.toml");
/// Starter profile template, written to the working directory by `jobpipe setup`.
pub const PROFILE_TEMPLATE_TOML: &str = include_str!("../profile.example.toml");

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

/// Parse company seed TOML text into seed rows. `source` labels errors.
fn parse_companies(text: &str, source: &str) -> Result<Vec<SeedCompany>> {
    let parsed: CompaniesFile =
        toml::from_str(text).with_context(|| format!("parsing {source}"))?;
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

/// Parse `companies.toml` at `path` into seed rows.
pub fn load_companies(path: &Path) -> Result<Vec<SeedCompany>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading company seed file {}", path.display()))?;
    parse_companies(&text, &path.display().to_string())
}

/// The built-in company seed list, for when no `companies.toml` is on disk.
pub fn default_companies() -> Result<Vec<SeedCompany>> {
    parse_companies(DEFAULT_COMPANIES_TOML, "built-in company list")
}

/// Load the raw `profile.toml` text for embedding in the triage prompt. We pass
/// the TOML through verbatim rather than reshaping it — the candidate profile is
/// human-authored context for the model, and every field is relevant.
pub fn load_profile_text(path: &Path) -> Result<String> {
    std::fs::read_to_string(path)
        .with_context(|| format!("reading candidate profile {}", path.display()))
}

/// Just the `[prefilter]` section of `profile.toml`, if present. The rest of the
/// file is ignored here — it's parsed as raw text elsewhere for the prompt.
#[derive(Debug, Deserialize)]
struct ProfilePrefilter {
    #[serde(default)]
    prefilter: Option<Prefilter>,
}

/// Code-side pre-filter tuning. A posting whose title matches any `reject_titles`
/// term is dropped before the LLM ever scores it — *unless* its description
/// mentions one of `keep_keywords`, which rescues it. Both lists are matched
/// case-insensitively on word boundaries. This is a cheap cost-saver; the real
/// scoring is the LLM's job.
#[derive(Debug, Clone, Deserialize)]
pub struct Prefilter {
    #[serde(default)]
    pub reject_titles: Vec<String>,
    #[serde(default)]
    pub keep_keywords: Vec<String>,
}

impl Default for Prefilter {
    /// Absent `[prefilter]` → reject only unambiguously off-discipline titles,
    /// with no rescue keyword. Seniority-specific filtering is opt-in, since a
    /// senior candidate would want those roles.
    fn default() -> Self {
        Self {
            reject_titles: ["Recruiter", "Sales", "Marketing"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            keep_keywords: Vec::new(),
        }
    }
}

/// Load the `[prefilter]` section from `profile.toml`, falling back to
/// [`Prefilter::default`] when the section is absent. An explicit but empty
/// `[prefilter]` disables pre-filtering entirely (every posting reaches the LLM).
pub fn load_prefilter(path: &Path) -> Result<Prefilter> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading candidate profile {}", path.display()))?;
    let parsed: ProfilePrefilter =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(parsed.prefilter.unwrap_or_default())
}
