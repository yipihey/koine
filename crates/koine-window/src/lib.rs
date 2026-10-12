//! Opens a koine host's page: a native WebView when built with `webview`, the
//! system browser otherwise (docs/plan-koine-live.md L-D9).

#[cfg(feature = "webview")]
mod webview;

/// Whether this build can open a native window.
pub const HAS_WEBVIEW: bool = cfg!(feature = "webview");

/// How the page was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opened {
    /// The system browser was asked to open the page; this returned at once.
    Browser,
    /// A native window showed the page and has been closed.
    WebView,
}

#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    #[error("could not open the system browser: {0}; open the URL by hand")]
    Browser(String),
    #[error(
        "the native window failed: {0}; on Linux install webkit2gtk and gtk3 and build with \
         `--features window-webview`"
    )]
    WebView(String),
}

/// Ask the system browser to open `url`; returns once it has been asked.
///
/// The URL is checked here, before the browser sees it: a browser given
/// "not a url" may refuse it, search for it, or open it as a file, and which
/// one is the browser's choice, not this crate's (the CI runner's Chrome
/// opened it, a machine with no browser refused it).
pub fn open_in_browser(url: &str) -> Result<Opened, WindowError> {
    check_url(url)?;
    webbrowser::open(url).map_err(|e| WindowError::Browser(e.to_string()))?;
    Ok(Opened::Browser)
}

/// An `http://` or `https://` URL with no whitespace; anything else is
/// refused as [`WindowError::Browser`] naming it.
fn check_url(url: &str) -> Result<(), WindowError> {
    let trimmed = url.trim();
    let scheme_ok = trimmed.starts_with("http://") || trimmed.starts_with("https://");
    if trimmed.is_empty() || !scheme_ok || trimmed.contains(char::is_whitespace) {
        return Err(WindowError::Browser(format!(
            "'{url}' is not an http(s) URL"
        )));
    }
    Ok(())
}

/// Show `url` in a native window titled `title` when built with `webview`
/// (blocking until the window closes, returning [`Opened::WebView`]); else open
/// the system browser and return at once.
///
/// The `webview` path must run on the main thread: the platform event loops
/// (AppKit, GTK, Win32) refuse any other.
pub fn open(url: &str, title: &str) -> Result<Opened, WindowError> {
    #[cfg(feature = "webview")]
    {
        webview::run(url, title)
    }
    #[cfg(not(feature = "webview"))]
    {
        let _ = title;
        open_in_browser(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_build_has_no_webview() {
        assert!(!std::hint::black_box(HAS_WEBVIEW));
    }

    #[test]
    fn a_bad_url_is_an_error_not_a_browser() {
        // Refused before any browser is asked, so the answer is the same on a
        // machine with Chrome and on one with nothing.
        for bad in ["not a url", "", "ftp://x", "http://a b", "localhost:8080"] {
            let Err(WindowError::Browser(message)) = open(bad, "t") else {
                panic!("'{bad}' was not refused");
            };
            assert!(message.contains("not an http(s) URL"), "{message}");
        }
        assert!(check_url("http://127.0.0.1:8080/ui/").is_ok());
        assert!(check_url("https://example.org").is_ok());
    }

    #[test]
    fn the_errors_say_what_to_do() {
        assert!(WindowError::WebView("x".into())
            .to_string()
            .contains("window-webview"));
    }
}
