//! The loopback token (plan D8).
//!
//! A `GET` needs no token; a dispatch and a layout verb need the launch's
//! loopback bearer, which a page cannot read from the file. The page takes it
//! from the first of three places and never guesses:
//!
//! 1. `<meta name="koine-token" content="…">`, which the app injects into the
//!    `index.html` it serves on loopback;
//! 2. `sessionStorage["koine-token"]`, where an earlier load or the field
//!    below left it;
//! 3. the value the person pastes into the status line's field, which is then
//!    stored in `sessionStorage` (this tab only; never in the URL).
//!
//! Without one the page is read-only and says so. What is pure here
//! ([`Token::new`], [`meta_token`], [`first`]) is tested natively; the DOM
//! reads go through `document::eval`.

use std::fmt;

use dioxus::document;

/// The meta element's name and the storage key: one spelling for both.
pub const NAME: &str = "koine-token";

/// A bearer token. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A token from raw text: trimmed, and refused when empty or when it holds
    /// whitespace or a control character (it becomes a header value).
    pub fn new(raw: &str) -> Option<Token> {
        let raw = raw.trim();
        let valid = !raw.is_empty() && raw.chars().all(|c| !c.is_whitespace() && !c.is_control());
        valid.then(|| Token(raw.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

/// The `content` of the first `<meta name="koine-token" …>` in `html`. The
/// two attributes may come in either order, in single or double quotes.
pub fn meta_token(html: &str) -> Option<Token> {
    let mut rest = html;
    while let Some(start) = rest.find("<meta") {
        let tag_and_after = &rest[start + "<meta".len()..];
        let end = tag_and_after.find('>')?;
        let tag = &tag_and_after[..end];
        if attribute(tag, "name").as_deref() == Some(NAME) {
            if let Some(token) = attribute(tag, "content").as_deref().and_then(Token::new) {
                return Some(token);
            }
        }
        rest = &tag_and_after[end..];
    }
    None
}

/// The value of `name="…"` (or `name='…'`) inside one tag's attributes.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let mut from = 0;
    while let Some(found) = tag[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let boundary_before = tag[..at]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace());
        let after = tag[from..].trim_start();
        if !boundary_before || !after.starts_with('=') {
            continue;
        }
        let value = after[1..].trim_start();
        let quote = value.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let inner = &value[1..];
        let close = inner.find(quote)?;
        return Some(inner[..close].to_string());
    }
    None
}

/// The first token held: the meta element's, else storage's.
pub fn first(meta: Option<Token>, stored: Option<Token>) -> Option<Token> {
    meta.or(stored)
}

/// What the page holds at load: the meta element, else `sessionStorage`.
pub async fn load() -> Option<Token> {
    // `outerHTML` of <head> is the text `meta_token` reads; the page's head is
    // small and the read happens once.
    let head = eval_string("return document.head ? document.head.outerHTML : '';").await;
    let stored = eval_string(&format!(
        "try {{ return sessionStorage.getItem({}) || ''; }} catch (e) {{ return ''; }}",
        js_string(NAME)
    ))
    .await;
    first(
        head.as_deref().and_then(meta_token),
        stored.as_deref().and_then(Token::new),
    )
}

/// Keep a pasted token for this tab.
pub async fn store(token: &Token) {
    let script = format!(
        "try {{ sessionStorage.setItem({}, {}); }} catch (e) {{}} return '';",
        js_string(NAME),
        js_string(token.as_str())
    );
    eval_string(&script).await;
}

/// Forget the stored token (the meta element, if any, is the app's to change).
pub async fn forget() {
    let script = format!(
        "try {{ sessionStorage.removeItem({}); }} catch (e) {{}} return '';",
        js_string(NAME)
    );
    eval_string(&script).await;
}

/// A JavaScript string literal for `raw`: JSON is one.
pub(crate) fn js_string(raw: &str) -> String {
    serde_json::to_string(raw).unwrap_or_else(|_| "\"\"".to_string())
}

pub(crate) async fn eval_string(script: &str) -> Option<String> {
    let value = document::eval(script).await.ok()?;
    value.as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(token: Option<Token>) -> Option<String> {
        token.map(|t| t.as_str().to_string())
    }

    #[test]
    fn the_meta_element_gives_the_token_in_either_attribute_order() {
        let html =
            r#"<head><meta charset="utf-8"><meta name="koine-token" content="abc123"></head>"#;
        assert_eq!(text(meta_token(html)), Some("abc123".into()));
        let reversed = r#"<meta content='abc123' name='koine-token'>"#;
        assert_eq!(text(meta_token(reversed)), Some("abc123".into()));
        let self_closed = r#"<meta  name = "koine-token"   content = "t0k" />"#;
        assert_eq!(text(meta_token(self_closed)), Some("t0k".into()));
    }

    #[test]
    fn other_meta_elements_and_an_empty_content_give_nothing() {
        assert!(meta_token(r#"<meta name="viewport" content="width=device-width">"#).is_none());
        assert!(meta_token(r#"<meta name="koine-token" content="">"#).is_none());
        assert!(meta_token(r#"<meta name="koine-token">"#).is_none());
        assert!(meta_token(r#"<meta name="x-koine-token" content="no">"#).is_none());
        assert!(meta_token("").is_none());
        assert!(meta_token("<meta").is_none());
    }

    #[test]
    fn the_first_meta_with_a_token_wins_over_later_ones() {
        let html = r#"<meta name="koine-token" content="first"><meta name="koine-token" content="second">"#;
        assert_eq!(text(meta_token(html)), Some("first".into()));
    }

    #[test]
    fn a_token_is_trimmed_and_refuses_anything_a_header_cannot_hold() {
        assert_eq!(text(Token::new("  abc  ")), Some("abc".into()));
        assert!(Token::new("").is_none());
        assert!(Token::new("   ").is_none());
        assert!(Token::new("a b").is_none());
        assert!(Token::new("a\r\nX-Evil: 1").is_none());
    }

    #[test]
    fn the_meta_element_outranks_storage_and_neither_is_read_only() {
        let a = Token::new("meta");
        let b = Token::new("stored");
        assert_eq!(text(first(a.clone(), b.clone())), Some("meta".into()));
        assert_eq!(text(first(None, b)), Some("stored".into()));
        assert!(first(None, None).is_none());
    }

    #[test]
    fn debug_never_prints_the_token() {
        let token = Token::new("secret").unwrap();
        assert!(!format!("{token:?}").contains("secret"));
    }

    #[test]
    fn a_js_string_is_quoted_and_escaped() {
        assert_eq!(js_string("a\"b"), r#""a\"b""#);
    }
}
