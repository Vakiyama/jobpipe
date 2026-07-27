//! Digest rendering. Phase 1 is deliberately dumb: it prints every open posting
//! with title, location, and apply URL. Scoring/thresholds arrive in phase 2.

use crate::db::entities::{company, posting};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Term,
    Md,
}

/// Render the digest. `rows` is newest-first, each posting paired with its company.
pub fn render(rows: &[(posting::Model, Option<company::Model>)], format: Format) -> String {
    if rows.is_empty() {
        return match format {
            Format::Term => "No open postings.\n".to_string(),
            Format::Md => "# jobpipe digest\n\n_No open postings._\n".to_string(),
        };
    }

    match format {
        Format::Term => render_term(rows),
        Format::Md => render_md(rows),
    }
}

fn company_name(c: &Option<company::Model>) -> &str {
    c.as_ref().map(|c| c.name.as_str()).unwrap_or("(unknown)")
}

fn loc(p: &posting::Model) -> String {
    match (&p.location, &p.remote) {
        (Some(l), Some(r)) if r != "onsite" && r != "unknown" => format!("{l} · {r}"),
        (Some(l), _) => l.clone(),
        (None, Some(r)) => r.clone(),
        (None, None) => "location unknown".to_string(),
    }
}

/// A score badge like `[9]`, or `[–]` when a posting hasn't been triaged yet.
fn badge(p: &posting::Model) -> String {
    match p.score {
        Some(s) => format!("[{s:>2}]"),
        None => "[ –]".to_string(),
    }
}

fn render_term(rows: &[(posting::Model, Option<company::Model>)]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "jobpipe digest — {} open posting(s)\n",
        rows.len()
    ));
    out.push_str(&"=".repeat(48));
    out.push('\n');
    for (p, c) in rows {
        out.push_str(&format!(
            "\n{} {}  —  {}\n",
            badge(p),
            p.title,
            company_name(c)
        ));
        out.push_str(&format!("  {}\n", loc(p)));
        if let Some(reason) = &p.score_reason {
            out.push_str(&format!("  {reason}\n"));
        }
        out.push_str(&format!("  {}\n", p.apply_url));
    }
    out
}

fn render_md(rows: &[(posting::Model, Option<company::Model>)]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "# jobpipe digest\n\n{} open posting(s).\n\n",
        rows.len()
    ));
    for (p, c) in rows {
        out.push_str(&format!(
            "- **{} [{}]({})** — {} · {}\n",
            badge(p),
            p.title,
            p.apply_url,
            company_name(c),
            loc(p)
        ));
        if let Some(reason) = &p.score_reason {
            out.push_str(&format!("  - {reason}\n"));
        }
    }
    out
}
