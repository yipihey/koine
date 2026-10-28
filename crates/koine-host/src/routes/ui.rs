//! `GET /ui`, `/ui/*`: the `koine-web` bundle from `Config::web_dir`.
//!
//! The same rules as `KoineWebRoutes` in impress-apps' `ImpressAutomation`:
//!
//! * a path whose last segment has an extension is a file of the bundle,
//!   served with its content type (404 when it is not there);
//! * anything else (`/ui`, `/ui/`, `/ui/surface/<id>`) is `index.html`, and
//!   the page's own router decides what it shows;
//! * a loopback peer gets `<meta name="koine-token" content="…">` in that
//!   `index.html`, so the page can post; any other peer gets the page
//!   without it. The peer is the connection's, never a header;
//! * no `..` and no dot-files, so the bundle cannot be climbed out of;
//! * 503 naming the build when there is no bundle.

use std::net::SocketAddr;
use std::path::Path as FsPath;

use axum::extract::{ConnectInfo, OriginalUri, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::{json_response, Shared};

/// The content type by extension (`KoineWebRoutes.contentType`).
pub(super) fn content_type_of(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap_or("");
    match extension.to_ascii_lowercase().as_str() {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "txt" | "md" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn missing_bundle() -> Response {
    json_response(
        503,
        &json!({
            "status": "error",
            "reason": "koine-web bundle is not in this host",
            "fix": "run `dx build --platform web --release` in crates/koine-web, then start \
                    the host with --web-dir pointing at the bundle's public/ directory \
                    (the one holding index.html)",
        }),
    )
}

fn not_found() -> Response {
    json_response(404, &json!({ "status": "error", "reason": "not found" }))
}

/// `<meta name="koine-token" content="…">` before `</head>` (or at the start
/// when the page has no head). `None` leaves the page untouched.
fn inject(token: Option<&str>, html: &str) -> String {
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return html.to_string();
    };
    let escaped = token
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;");
    let meta = format!("<meta name=\"koine-token\" content=\"{escaped}\">");
    match html.find("</head>") {
        Some(at) => format!("{}{meta}{}", &html[..at], &html[at..]),
        None => format!("{meta}{html}"),
    }
}

pub(super) async fn serve(
    State(state): State<Shared>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    OriginalUri(uri): OriginalUri,
) -> Response {
    let Some(root) = state.web_dir.as_deref() else {
        return missing_bundle();
    };
    let relative = uri.path().strip_prefix("/ui").unwrap_or("");
    let token = peer
        .ip()
        .to_canonical()
        .is_loopback()
        .then_some(state.token.as_str());
    let root = root.to_path_buf();
    let relative = relative.to_string();
    let token = token.map(str::to_string);
    // File reads off the reactor.
    tokio::task::spawn_blocking(move || read(&root, &relative, token.as_deref()))
        .await
        .unwrap_or_else(|_| not_found())
}

fn read(root: &FsPath, relative: &str, token: Option<&str>) -> Response {
    let components: Vec<&str> = relative.split('/').filter(|s| !s.is_empty()).collect();
    if components.iter().any(|c| *c == ".." || c.starts_with('.')) {
        return not_found();
    }
    let index = root.join("index.html");
    if !index.is_file() {
        return missing_bundle();
    }
    if let Some(last) = components.last().filter(|last| last.contains('.')) {
        let file = components
            .iter()
            .fold(root.to_path_buf(), |path, part| path.join(part));
        return match std::fs::read(&file) {
            Ok(bytes) => (
                StatusCode::OK,
                [
                    (
                        header::CONTENT_TYPE,
                        HeaderValue::from_static(content_type_of(last)),
                    ),
                    (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                ],
                bytes,
            )
                .into_response(),
            Err(_) => not_found(),
        };
    }
    match std::fs::read_to_string(&index) {
        Ok(html) => (
            StatusCode::OK,
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("text/html; charset=utf-8"),
                ),
                (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            ],
            inject(token, &html),
        )
            .into_response(),
        Err(_) => missing_bundle(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_goes_before_head_closes_and_is_escaped() {
        let page = "<html><head><title>x</title></head><body></body></html>";
        assert_eq!(
            inject(Some("a\"b<c&d"), page),
            "<html><head><title>x</title>\
             <meta name=\"koine-token\" content=\"a&quot;b&lt;c&amp;d\"></head>\
             <body></body></html>"
        );
        assert_eq!(inject(None, page), page);
        assert_eq!(inject(Some(""), page), page);
        assert_eq!(
            inject(Some("t"), "<p>no head</p>"),
            "<meta name=\"koine-token\" content=\"t\"><p>no head</p>"
        );
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type_of("x.js"), "text/javascript; charset=utf-8");
        assert_eq!(content_type_of("a/b.WASM"), "application/wasm");
        assert_eq!(content_type_of("noext"), "application/octet-stream");
        assert_eq!(content_type_of("a.txt"), "text/plain; charset=utf-8");
    }
}
