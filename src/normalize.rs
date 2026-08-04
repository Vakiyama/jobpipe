//! `RawPosting` -> `NormalizedPosting`: strip HTML from the description and
//! canonicalize the location into a coarse remote flag.

use crate::sources::RawPosting;

// `clean_html` decodes entities via the html-escape crate.

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
    // Trust the source's explicit remote flag when it gave one; otherwise infer
    // from the location string.
    let remote = raw
        .remote
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| infer_remote(location.as_deref()));
    NormalizedPosting {
        external_id: raw.external_id,
        title: single_line(&raw.title),
        location,
        remote,
        description: clean_html(&raw.description),
        apply_url: raw.apply_url,
    }
}

/// Collapse every run of whitespace (newlines included) into a single space so a
/// title always renders on one line. HN "who is hiring" comments — and the odd
/// ATS — carry embedded line breaks that otherwise wrap the digest, `track`, and
/// apply output across several rows.
fn single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
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

/// A rendered block: a paragraph or a list item. Tracked so the join step can
/// keep bullets tight while giving paragraphs a blank line to breathe.
#[derive(Clone, Copy, PartialEq)]
enum Block {
    Para,
    Bullet,
}

/// Decode HTML entities, strip tags, and normalize whitespace into readable
/// plain text with real structure. Entities are decoded first so entity-escaped
/// markup (Greenhouse serves `&lt;p&gt;`) and real markup (the other boards)
/// both reduce to tags that the stripper then removes.
///
/// Structure is preserved rather than flattened: `<li>` items are prefixed `• `
/// and kept tight (one newline between consecutive bullets); other block-level
/// tags (`<p>`, `<div>`, headings, …) are separated by a blank line; `<br>` is a
/// soft line break within its block; inline tags become spaces. Within a line,
/// whitespace runs collapse to a single space.
pub(crate) fn clean_html(input: &str) -> String {
    let decoded = html_escape::decode_html_entities(input);
    let mut segments: Vec<(Block, String)> = Vec::new();
    let mut buf = String::new();
    // The kind of the text currently accumulating in `buf`, set by the block tag
    // that opened it. Defaults to a paragraph for any leading bare text.
    let mut kind = Block::Para;
    let mut tag = String::new();
    let mut in_tag = false;

    for ch in decoded.chars() {
        match ch {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' => {
                in_tag = false;
                if !is_break_tag(&tag) {
                    buf.push(' '); // inline tag -> word boundary
                } else if tag_name(&tag) == "br" {
                    buf.push('\n'); // soft line break, same block
                } else {
                    // Block boundary: the text so far belongs to the block that
                    // was open; flush it, then let the new tag pick the next kind.
                    push_block(&mut segments, kind, &mut buf);
                    let opening = !tag.trim_start().starts_with('/');
                    kind = if opening && tag_name(&tag) == "li" {
                        Block::Bullet
                    } else {
                        Block::Para
                    };
                }
            }
            _ if in_tag => tag.push(ch),
            c => buf.push(c),
        }
    }
    push_block(&mut segments, kind, &mut buf);

    // Join blocks: consecutive bullets stay tight; every other adjacency gets a
    // blank line. Bullets are marked with `• ` (only their first line, so a `<br>`
    // inside a bullet reads as a continuation, not a new item).
    let mut result = String::new();
    for (i, (block, text)) in segments.iter().enumerate() {
        if i > 0 {
            let tight = *block == Block::Bullet && segments[i - 1].0 == Block::Bullet;
            result.push_str(if tight { "\n" } else { "\n\n" });
        }
        match block {
            Block::Bullet => {
                let mut lines = text.split('\n');
                if let Some(first) = lines.next() {
                    result.push_str("• ");
                    result.push_str(first);
                }
                for l in lines {
                    result.push('\n');
                    result.push_str(l);
                }
            }
            Block::Para => result.push_str(text),
        }
    }
    result
}

/// Clean `buf` into a block and append it to `segments`, then clear `buf`.
/// Collapses whitespace within each line and drops blank lines, but keeps the
/// `<br>` newlines so a block can span multiple lines. An empty block is dropped.
fn push_block(segments: &mut Vec<(Block, String)>, kind: Block, buf: &mut String) {
    let cleaned = buf
        .split('\n')
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    buf.clear();
    if !cleaned.is_empty() {
        segments.push((kind, cleaned));
    }
}

#[cfg(test)]
mod tests {
    use super::{clean_html, normalize};
    use crate::sources::RawPosting;

    #[test]
    fn title_is_flattened_to_a_single_line() {
        let raw = RawPosting {
            external_id: "1".into(),
            title: "Acme Corp\n\nHiring Rust\nengineers".into(),
            location: None,
            description: String::new(),
            apply_url: "https://example.com".into(),
            remote: None,
        };
        assert_eq!(normalize(raw).title, "Acme Corp Hiring Rust engineers");
    }

    #[test]
    fn paragraphs_get_a_blank_line_and_br_stays_within_the_block() {
        let html = "<p>First para.</p><p>Second para.<br>Same para new line.</p>";
        assert_eq!(
            clean_html(html),
            "First para.\n\nSecond para.\nSame para new line."
        );
    }

    #[test]
    fn list_items_are_bulleted_and_kept_tight() {
        let html = "<ul><li>One</li><li>Two</li></ul>";
        assert_eq!(clean_html(html), "• One\n• Two");
    }

    #[test]
    fn a_paragraph_before_a_list_is_separated_by_a_blank_line() {
        let html = "<p>Requirements:</p><ul><li>One</li><li>Two</li></ul>";
        assert_eq!(clean_html(html), "Requirements:\n\n• One\n• Two");
    }

    #[test]
    fn inline_tags_stay_on_one_line_and_entities_decode() {
        let html = "Ship <b>fast</b> &amp; <i>safe</i>";
        assert_eq!(clean_html(html), "Ship fast & safe");
    }

    #[test]
    fn entity_escaped_markup_is_handled() {
        // Greenhouse serves entity-escaped tags like &lt;p&gt;.
        assert_eq!(clean_html("&lt;p&gt;Hello&lt;/p&gt;"), "Hello");
    }

    #[test]
    fn empty_blocks_are_dropped() {
        let html = "<div>A</div><div></div><div></div><div>B</div>";
        assert_eq!(clean_html(html), "A\n\nB");
    }
}

/// The lowercased element name from a tag's inner text (no angle brackets),
/// ignoring a leading `/` (close tags) and any attributes. `p class="x"` → `p`.
fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Whether a tag (its inner text, without the angle brackets) marks a
/// block-level boundary that should render as a line break.
fn is_break_tag(tag: &str) -> bool {
    matches!(
        tag_name(tag).as_str(),
        "br" | "p"
            | "div"
            | "li"
            | "ul"
            | "ol"
            | "tr"
            | "table"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "blockquote"
            | "hr"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
    )
}
