//! `--window`: serve, open the page at `/ui/`, and stop when the window closes.
//!
//! A host built with `window-webview` shows the page in a native window and
//! this call blocks (on the main thread) until that window closes, returning
//! [`koine_window::Opened::WebView`]: the caller then shuts the host down.
//! Without the feature the system browser is asked and this returns
//! [`koine_window::Opened::Browser`] at once: the caller keeps serving until
//! Ctrl-C.

/// Open `url` (the served `/ui/` page) titled `app_id`.
pub fn open_after_serve(
    url: &str,
    app_id: &str,
) -> Result<koine_window::Opened, koine_window::WindowError> {
    koine_window::open(url, app_id)
}
