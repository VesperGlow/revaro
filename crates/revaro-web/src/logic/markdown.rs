//! Markdown rendering for the built-in document editor.
//!
//! The legacy client used `marked` followed by `DOMPurify`. Keep that same
//! two-stage contract here: pulldown-cmark supplies the CommonMark/GFM
//! structure, and ammonia removes raw HTML and unsafe attributes before the
//! result is inserted into the preview with `inner_html`.

use ammonia::Builder;
use pulldown_cmark::{Options, Parser, html};

/// Render Markdown with the syntax the reference editor exposed.
///
/// `marked`'s default mode is GFM, so tables, strikethrough and task list
/// markers are enabled explicitly. Raw HTML remains available where it is
/// harmless (`<u>`, `<span>`, and similar tags), while scripts, event
/// handlers, unsafe URLs and other active content are removed just as the
/// reference's DOMPurify pass does.
#[must_use]
pub fn render_markdown(source: &str) -> String {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut rendered = String::new();
    html::push_html(&mut rendered, Parser::new_ext(source, options));
    // `marked` emits a break without a whitespace text node after it. The
    // pulldown-cmark renderer pretty-prints the same element as `<br />\n`,
    // which changes `textContent` (and copied text) even though the pixels are
    // identical. Remove only that renderer newline; whitespace in code blocks
    // and ordinary Markdown text remains untouched.
    rendered = rendered
        .replace("<br />\n", "<br />")
        .replace("<br>\n", "<br>")
        .replace("<br/>\n", "<br/>");
    for (style, align) in [
        (" style=\"text-align: left\"", " align=\"left\""),
        (" style=\"text-align: center\"", " align=\"center\""),
        (" style=\"text-align: right\"", " align=\"right\""),
    ] {
        rendered = rendered.replace(style, align);
    }

    let mut sanitizer = Builder::default();
    sanitizer
        .link_rel(None)
        .add_tags(["input"])
        .add_tag_attributes("input", ["checked", "disabled", "type"])
        .add_generic_attributes(["class"]);
    // `ammonia` reparses and serializes the fragment, so apply the same tiny
    // normalization once more after sanitizing as well; its serializer uses a
    // canonical `<br>` spelling and can reintroduce the newline.
    sanitizer
        .clean(&rendered)
        .to_string()
        .replace("<br>\n", "<br>")
        .replace("<br />\n", "<br />")
        .replace("<br/>\n", "<br/>")
}

#[cfg(test)]
mod tests {
    use super::render_markdown;

    #[test]
    fn renders_gfm_blocks_inline_elements_and_media() {
        let html = render_markdown(
            "# Title\n\n#### Deep heading\n\n1. one\n2. two\n\n- [x] done\n- [ ] todo\n\n| a | b |\n| --- | :---: |\n| 1 | 2 |\n\n[link](https://example.com \"T\") and ![alt](cover.png)\n\n~~gone~~ and <u>under</u>",
        );
        assert!(html.contains("<h1>Title</h1>"));
        assert!(html.contains("<h4>Deep heading</h4>"));
        assert!(html.contains("<ol>"));
        assert!(html.contains("<input disabled=\"\" type=\"checkbox\" checked=\"\">"));
        assert!(html.contains("<table>"));
        assert!(html.contains("<th align=\"center\">b</th>"));
        assert!(html.contains("<a href=\"https://example.com\" title=\"T\">link</a>"));
        assert!(html.contains("<img src=\"cover.png\" alt=\"alt\">"));
        assert!(html.contains("<del>gone</del>"));
        assert!(html.contains("<u>under</u>"));
    }

    #[test]
    fn keeps_safe_markup_but_removes_active_html() {
        let html = render_markdown(
            "<span onclick=\"alert(1)\">ok</span>\n\n<script>alert(2)</script>\n\n[x](javascript:alert(3))",
        );

        assert!(html.contains("<span>ok</span>"));
        assert!(!html.contains("onclick"));
        assert!(!html.contains("<script>"));
        assert!(!html.contains("alert(2)"));
        assert!(!html.contains("javascript:"));
    }

    #[test]
    fn does_not_add_copyable_whitespace_after_hard_breaks() {
        let html = render_markdown("first  \nsecond");

        assert!(html.contains("first<br>second"));
        assert!(!html.contains("<br>\n"));
    }
}
