//! The few DOM reads and writes the page does through `document::eval`.
//!
//! Each is a small, self-contained script. None of them decides anything: the
//! cursor does, and these show it (move DOM focus to a widget) or read what the
//! browser did (which widget a Tab landed on). The synchronous reads a key
//! handler needs are in [`crate::keys`] and [`crate::api::origin`].

use dioxus::document;

use crate::token::{eval_string, js_string};

/// Move DOM focus to the element carrying `data-koine-id = id`, or to the
/// first focusable thing inside it (a row's own control), so the next key
/// lands where the outline is.
pub async fn focus_widget(id: &str) {
    let script = format!(
        r#"const id = {id};
        const el = Array.from(document.querySelectorAll('[data-koine-id]')).find(e => e.dataset.koineId === id);
        if (el) {{
            const target = el.matches('input,select,textarea,button,a[href],[tabindex]')
                ? el : el.querySelector('input,select,textarea,button,a[href],[tabindex]');
            if (target) {{ target.focus({{ preventScroll: false }}); }}
        }}
        return '';"#,
        id = js_string(id)
    );
    eval_string(&script).await;
}

/// The `data-koine-id` of the widget that now has DOM focus, read after the
/// browser has finished its own Tab handling (one macrotask later).
pub async fn focused_widget() -> Option<String> {
    let script = r#"await new Promise(r => setTimeout(r, 0));
        const el = document.activeElement ? document.activeElement.closest('[data-koine-id]') : null;
        return el ? el.dataset.koineId : '';"#;
    eval_string(script).await.filter(|id| !id.is_empty())
}

/// Wait `ms` milliseconds without blocking the page.
pub async fn sleep(ms: u32) {
    let _ = document::eval(&format!(
        "await new Promise(r => setTimeout(r, {ms})); return '';"
    ))
    .await;
}
