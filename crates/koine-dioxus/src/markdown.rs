//! A `text` node is markdown (plan-koine-affordances A-D7): the web
//! performance draws headings, emphasis, inline code, lists, links, tables
//! and strikethrough instead of printing `# Map of this app` as it was
//! written. The tui prints the source, since markdown reads as text.
//!
//! A spec cannot inject markup through it: raw HTML in the source (a block
//! or inline) is turned into text and escaped, never passed through, and a
//! link or an image whose address is a script (`javascript:`, `vbscript:`,
//! `data:`) loses the address.

use pulldown_cmark::{html, CowStr, Event, Options, Parser, Tag, TagEnd};

/// The HTML of `source` read as CommonMark with tables and strikethrough.
/// Raw HTML is escaped text; an HTML block becomes a paragraph of it.
pub fn markdown_html(source: &str) -> String {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let events = Parser::new_ext(source, options).map(|event| match event {
        Event::Html(raw) | Event::InlineHtml(raw) => Event::Text(raw),
        Event::Start(Tag::HtmlBlock) => Event::Start(Tag::Paragraph),
        Event::End(TagEnd::HtmlBlock) => Event::End(TagEnd::Paragraph),
        Event::Start(Tag::Link {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image {
            link_type,
            dest_url,
            title,
            id,
        }) => Event::Start(Tag::Image {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        other => other,
    });
    let mut out = String::new();
    html::push_html(&mut out, events);
    out.truncate(out.trim_end().len());
    out
}

/// The address, or nothing when it would run a script.
fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    let scheme: String = url
        .trim_start()
        .chars()
        .filter(|c| !c.is_ascii_whitespace() && !c.is_ascii_control())
        .take_while(|c| *c != ':')
        .collect::<String>()
        .to_ascii_lowercase();
    let has_scheme = url.contains(':') && !scheme.contains('/');
    if has_scheme && matches!(scheme.as_str(), "javascript" | "vbscript" | "data") {
        CowStr::Borrowed("")
    } else {
        url
    }
}

#[cfg(test)]
mod tests {
    use super::markdown_html;

    #[test]
    fn headings_emphasis_code_and_lists_are_drawn() {
        assert_eq!(
            markdown_html("# Map of this app\n\nA *map* of `verbs`:\n\n- one\n- two"),
            "<h1>Map of this app</h1>\n<p>A <em>map</em> of <code>verbs</code>:</p>\n\
             <ul>\n<li>one</li>\n<li>two</li>\n</ul>"
        );
        assert_eq!(markdown_html("plain"), "<p>plain</p>");
        assert_eq!(markdown_html(""), "");
    }

    #[test]
    fn tables_and_strikethrough_are_on() {
        let table = markdown_html("| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(table.starts_with("<table>"), "{table}");
        assert!(table.contains("<td>1</td>"), "{table}");
        assert_eq!(markdown_html("~~gone~~"), "<p><del>gone</del></p>");
    }

    #[test]
    fn raw_html_is_escaped_text_never_markup() {
        let inline = markdown_html("hi <script>alert(1)</script> there");
        assert!(!inline.contains("<script"), "{inline}");
        assert!(inline.contains("&lt;script&gt;"), "{inline}");
        let block = markdown_html("<div onclick=\"x()\">boo</div>");
        assert!(!block.contains("<div"), "{block}");
        assert!(block.starts_with("<p>&lt;div"), "{block}");
        let img = markdown_html("<img src=x onerror=alert(1)>");
        assert!(!img.contains("<img"), "{img}");
    }

    #[test]
    fn a_script_address_is_dropped_and_others_kept() {
        let bad = markdown_html("[x](javascript:alert(1)) [y]( JavaScript:alert(2))");
        assert!(!bad.to_lowercase().contains("javascript"), "{bad}");
        let data = markdown_html("![i](data:text/html;base64,AAAA)");
        assert!(!data.contains("data:"), "{data}");
        assert_eq!(
            markdown_html("[docs](https://example.org/a?b=c)"),
            "<p><a href=\"https://example.org/a?b=c\">docs</a></p>"
        );
        assert_eq!(
            markdown_html("[rel](./x:y)"),
            "<p><a href=\"./x:y\">rel</a></p>"
        );
    }
}
