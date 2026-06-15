//! HTML → readable markdown for `web_fetch`. Backed by `fast_html2md`'s lol_html
//! rewriter: among the fastest Rust converters with a tiny, input-independent
//! memory footprint and no html5ever DOM tree to babysit.
//!
//! The `distilled` (Readability main-content) extraction mode is intentionally NOT
//! here yet — see `web_fetch::FetchMode::Distilled`. It is reserved for a follow-up
//! using `dom_smoothie` (the one Rust Readability port that reliably finds the main
//! article body) so the small model can ask for boilerplate-free content.

/// Hard safety ceiling on returned bytes, independent of mode, so a huge page can
/// never flood the model context.
pub const MAX_BYTES: usize = 100_000;

/// `truncated` mode preview size — a cheap first look at a page.
pub const TRUNCATE_BYTES: usize = 8_000;

/// Convert an HTML document to markdown, trimmed and with runs of blank lines
/// collapsed. Script/style content is dropped by the converter.
pub fn html_to_markdown(html: &str) -> String {
    let md = md::rewrite_html(html, false);
    collapse_blank_lines(md.trim())
}

/// Cap `text` to at most `limit` bytes, never splitting a UTF-8 char. Returns the
/// (possibly shortened) text and whether truncation occurred.
pub fn cap(text: &str, limit: usize) -> (String, bool) {
    if text.len() <= limit {
        return (text.to_string(), false);
    }
    let mut end = limit;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    (text[..end].to_string(), true)
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0usize;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out.trim_end().to_string()
}

// `fast_html2md`'s lib name is `html2md`.
use html2md as md;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_and_drops_script_style() {
        let html = r#"<html><head><style>.x{color:red}</style>
            <script>alert('x')</script></head>
            <body><h1>Title</h1><p>Hello <b>world</b>.</p></body></html>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("Title"), "{md}");
        assert!(md.contains("Hello"), "{md}");
        assert!(md.contains("world"), "{md}");
        assert!(!md.contains("alert("), "script body must be stripped: {md}");
        assert!(
            !md.contains("color:red"),
            "style body must be stripped: {md}"
        );
    }

    #[test]
    fn renders_links_and_headings_as_markdown() {
        let html = r#"<h2>Docs</h2><p><a href="https://e.com">site</a></p>"#;
        let md = html_to_markdown(html);
        assert!(md.contains("Docs"), "{md}");
        assert!(md.contains("site"), "{md}");
        assert!(md.contains("https://e.com"), "link target preserved: {md}");
    }

    #[test]
    fn collapses_blank_line_runs() {
        let collapsed = collapse_blank_lines("a\n\n\n\nb");
        assert_eq!(collapsed, "a\n\nb");
    }

    #[test]
    fn cap_is_a_noop_under_the_limit() {
        let (out, truncated) = cap("short", 100);
        assert_eq!(out, "short");
        assert!(!truncated);
    }

    #[test]
    fn cap_truncates_and_flags() {
        let (out, truncated) = cap("0123456789", 4);
        assert_eq!(out, "0123");
        assert!(truncated);
    }

    #[test]
    fn cap_respects_utf8_boundaries() {
        // "é" is two bytes; capping at 1 byte must back off to 0, not split it.
        let (out, truncated) = cap("é", 1);
        assert_eq!(out, "");
        assert!(truncated);
    }
}
