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

/// Whether the tab is in the background. The layout page's poll skips a tick
/// then. A page that cannot tell is treated as visible.
pub async fn page_hidden() -> bool {
    eval_string("return document.hidden ? 'hidden' : 'visible';")
        .await
        .is_some_and(|state| state == "hidden")
}

/// The on-screen lengths along the container's axis of the two cells the
/// divider `index` of `container` sits between: the child before it and the
/// child after it. The page cannot compute these; the browser laid them out.
pub async fn measure_neighbours(
    container: u64,
    index: usize,
    horizontal: bool,
) -> Option<(f32, f32)> {
    let axis = if horizontal { "width" } else { "height" };
    let script = format!(
        r#"const d = document.querySelector('[data-koine-container="{container}"][data-koine-divider="{index}"]');
        if (!d || !d.previousElementSibling || !d.nextElementSibling) {{ return ''; }}
        const a = d.previousElementSibling.getBoundingClientRect();
        const b = d.nextElementSibling.getBoundingClientRect();
        return a.{axis} + ',' + b.{axis};"#
    );
    parse_lengths(&eval_string(&script).await?)
}

/// `"120.5,80"` as two positive lengths.
pub fn parse_lengths(raw: &str) -> Option<(f32, f32)> {
    let (before, after) = raw.split_once(',')?;
    let before: f32 = before.trim().parse().ok()?;
    let after: f32 = after.trim().parse().ok()?;
    (before > 0.0 && after > 0.0 && before.is_finite() && after.is_finite())
        .then_some((before, after))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_lengths_parse_and_anything_else_is_none() {
        assert_eq!(parse_lengths("120.5,80"), Some((120.5, 80.0)));
        assert_eq!(parse_lengths(" 3 , 4 "), Some((3.0, 4.0)));
        for bad in ["", "5", "0,4", "4,-1", "a,b", "NaN,3", "inf,3"] {
            assert_eq!(parse_lengths(bad), None, "{bad:?}");
        }
    }
}
