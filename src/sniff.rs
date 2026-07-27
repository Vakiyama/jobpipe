//! ATS detection for `companies add <url>`. First tries to read the ATS + slug
//! straight from the URL pattern; falls back to fetching the page and scanning
//! for known ATS embed markers.

use anyhow::{bail, Context, Result};
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub ats: String,
    pub slug: String,
    pub name: String,
}

impl Detected {
    fn new(ats: &str, slug: &str) -> Self {
        Detected {
            ats: ats.to_string(),
            slug: slug.to_string(),
            name: title_case(slug),
        }
    }
}

/// Detect ATS + slug for a careers URL: URL pattern first, page scan second.
pub async fn detect(client: &Client, url: &str) -> Result<Detected> {
    if let Some(d) = from_url(url) {
        return Ok(d);
    }
    let resp = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("fetching {url} for ATS sniffing"))?;
    let body = resp.text().await.context("reading page body")?;
    from_markers(&body).with_context(|| format!("could not detect a known ATS from {url}"))
}

/// Parse `(host, first_path_segment, query)` from a URL that may lack a scheme.
fn parts(url: &str) -> (String, Option<String>, Option<String>) {
    let s = url.trim();
    let s = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .unwrap_or(s);
    let (path_part, query) = match s.split_once('?') {
        Some((p, q)) => (p, Some(q.to_string())),
        None => (s, None),
    };
    let mut segs = path_part.split('/').filter(|x| !x.is_empty());
    let host = segs.next().unwrap_or("").to_lowercase();
    let seg0 = segs.next().map(|x| x.to_string());
    (host, seg0, query)
}

fn query_value<'a>(query: Option<&'a str>, key: &str) -> Option<&'a str> {
    query?.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v)
    })
}

fn subdomain(host: &str, root: &str) -> Option<String> {
    host.strip_suffix(root)
        .map(|s| s.trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty() && !s.contains('.'))
}

/// Detect from the URL shape alone (no network).
pub fn from_url(url: &str) -> Option<Detected> {
    let (host, seg0, query) = parts(url);

    if host.ends_with("greenhouse.io") {
        // boards.greenhouse.io/acme, job-boards.greenhouse.io/acme, or
        // boards.greenhouse.io/embed/job_board?for=acme
        if seg0.as_deref() == Some("embed") {
            return query_value(query.as_deref(), "for").map(|s| Detected::new("greenhouse", s));
        }
        if let Some(s) = seg0.filter(|s| s != "job_board") {
            return Some(Detected::new("greenhouse", &s));
        }
        return subdomain(&host, "greenhouse.io")
            .filter(|s| !["boards", "job-boards", "boards-api"].contains(&s.as_str()))
            .map(|s| Detected::new("greenhouse", &s));
    }
    if host == "jobs.lever.co" {
        return seg0.map(|s| Detected::new("lever", &s));
    }
    if host == "jobs.ashbyhq.com" || host == "ashbyhq.com" {
        return seg0.map(|s| Detected::new("ashby", &s));
    }
    if host == "apply.workable.com" {
        return seg0.map(|s| Detected::new("workable", &s));
    }
    if let Some(s) = subdomain(&host, "workable.com").filter(|s| s != "apply" && s != "www") {
        return Some(Detected::new("workable", &s));
    }
    if let Some(s) = subdomain(&host, "recruitee.com").filter(|s| s != "www") {
        return Some(Detected::new("recruitee", &s));
    }
    None
}

/// Detect by scanning fetched HTML for embedded ATS references.
fn from_markers(body: &str) -> Result<Detected> {
    // Ordered: greenhouse embed's `for=` is the most specific.
    static PATTERNS: Lazy<Vec<(&'static str, Regex)>> = Lazy::new(|| {
        vec![
            ("greenhouse", Regex::new(r"greenhouse\.io/embed/job_board\?for=([a-zA-Z0-9_-]+)").unwrap()),
            ("greenhouse", Regex::new(r"(?:job-boards|boards)\.greenhouse\.io/([a-zA-Z0-9_-]+)").unwrap()),
            ("lever", Regex::new(r"jobs\.lever\.co/([a-zA-Z0-9_-]+)").unwrap()),
            ("ashby", Regex::new(r"(?:jobs\.ashbyhq\.com|api\.ashbyhq\.com/posting-api/job-board)/([a-zA-Z0-9_-]+)").unwrap()),
            ("workable", Regex::new(r"apply\.workable\.com/([a-zA-Z0-9_-]+)").unwrap()),
            ("workable", Regex::new(r"([a-zA-Z0-9-]+)\.workable\.com").unwrap()),
            ("recruitee", Regex::new(r"([a-zA-Z0-9-]+)\.recruitee\.com").unwrap()),
        ]
    });
    for (ats, re) in PATTERNS.iter() {
        if let Some(caps) = re.captures(body) {
            let slug = caps.get(1).unwrap().as_str();
            if !["apply", "www", "boards", "job-boards"].contains(&slug) {
                return Ok(Detected::new(ats, slug));
            }
        }
    }
    bail!("no known ATS markers found in page")
}

/// Turn a slug like `acme-corp` into a display name `Acme Corp`.
fn title_case(slug: &str) -> String {
    slug.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) => f.to_uppercase().collect::<String>() + ch.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_url_patterns() {
        let cases = [
            ("https://boards.greenhouse.io/acme", "greenhouse", "acme"),
            (
                "https://boards.greenhouse.io/embed/job_board?for=acme",
                "greenhouse",
                "acme",
            ),
            (
                "job-boards.greenhouse.io/acme/jobs/123",
                "greenhouse",
                "acme",
            ),
            ("https://jobs.lever.co/acme", "lever", "acme"),
            ("https://jobs.ashbyhq.com/acme", "ashby", "acme"),
            ("https://apply.workable.com/acme/", "workable", "acme"),
            ("https://acme.recruitee.com/", "recruitee", "acme"),
        ];
        for (url, ats, slug) in cases {
            let d = from_url(url).unwrap_or_else(|| panic!("no detection for {url}"));
            assert_eq!((d.ats.as_str(), d.slug.as_str()), (ats, slug), "for {url}");
        }
    }

    #[test]
    fn title_cases_slugs() {
        assert_eq!(title_case("acme-corp"), "Acme Corp");
        assert_eq!(title_case("openai"), "Openai");
    }
}
