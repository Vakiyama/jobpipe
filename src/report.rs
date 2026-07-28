//! Digest rendering. Phase 1 is deliberately dumb: it prints every open posting
//! with title, location, and apply URL. Scoring/thresholds arrive in phase 2.

use crate::db::entities::{company, posting};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Term,
    Md,
}

/// Render the digest. `rows` is newest-first, each posting paired with its company.
///
/// `hyperlinks` turns the term-format apply URL into a clickable OSC 8 link;
/// callers should pass this only when stdout is a real terminal, since the
/// escape sequences would otherwise leak into pipes and files.
pub fn render(
    rows: &[(posting::Model, Option<company::Model>)],
    format: Format,
    hyperlinks: bool,
) -> String {
    if rows.is_empty() {
        return match format {
            Format::Term => "No open postings.\n".to_string(),
            Format::Md => "# jobpipe digest\n\n_No open postings._\n".to_string(),
        };
    }

    match format {
        Format::Term => render_term(rows, hyperlinks),
        Format::Md => render_md(rows),
    }
}

/// Wrap `text` in an OSC 8 terminal hyperlink pointing at `url`. Terminals that
/// don't understand the sequence ignore it and show `text` alone.
fn osc8(url: &str, text: &str) -> String {
    format!("\x1b]8;;{url}\x1b\\{text}\x1b]8;;\x1b\\")
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

fn render_term(rows: &[(posting::Model, Option<company::Model>)], hyperlinks: bool) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "jobpipe digest — {} open posting(s)\n",
        rows.len()
    ));
    out.push_str(&"=".repeat(48));
    out.push('\n');
    for (p, c) in rows {
        out.push_str(&format!(
            "\n{} #{}  {}  —  {}\n",
            badge(p),
            p.id,
            p.title,
            company_name(c)
        ));
        out.push_str(&format!("  {}\n", loc(p)));
        if let Some(reason) = &p.score_reason {
            out.push_str(&format!("  {reason}\n"));
        }
        if hyperlinks {
            out.push_str(&format!("  {}\n", osc8(&p.apply_url, "Apply in browser ↗")));
        } else {
            out.push_str(&format!("  {}\n", p.apply_url));
        }
        out.push_str(&format!("  → jobpipe apply {}\n", p.id));
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
            "- **{} #{} [{}]({})** — {} · {}\n",
            badge(p),
            p.id,
            p.title,
            p.apply_url,
            company_name(c),
            loc(p)
        ));
        if let Some(reason) = &p.score_reason {
            out.push_str(&format!("  - {reason}\n"));
        }
        out.push_str(&format!("  - `jobpipe apply {}`\n", p.id));
    }
    out
}
