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
pub fn open_in_browser(url: &str) -> Result<Opened, WindowError> {
    webbrowser::open(url).map_err(|e| WindowError::Browser(e.to_string()))?;
    Ok(Opened::Browser)
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
        assert!(matches!(
            open("not a url", "t"),
            Err(WindowError::Browser(_))
        ));
    }

    #[test]
    fn the_errors_say_what_to_do() {
        assert!(WindowError::WebView("x".into())
            .to_string()
            .contains("window-webview"));
    }
}
