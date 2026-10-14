//! What the page reads off the browser's click and pointer: the one place
//! `web-sys` is touched. Each function answers `None` when the DOM does not
//! say, and the mapping then sends no event; nothing here decides what a
//! click or a drop means.

use dioxus::prelude::{MouseEvent, PointerEvent};
use web_sys::wasm_bindgen::JsCast;
use web_sys::{Element, MouseEvent as WebMouseEvent, PointerEvent as WebPointerEvent};

use crate::outline::{self, DropAt};

fn web_event(e: &MouseEvent) -> Option<WebMouseEvent> {
    e.data().downcast::<WebMouseEvent>().cloned()
}

/// The nearest element matching `selector`, from the click's target upward.
/// Not `current_target`: Dioxus listens once at the root and walks the click
/// down to the handler itself, so the DOM event's current target is the root.
fn owner(event: &WebMouseEvent, selector: &str) -> Option<Element> {
    event
        .target()?
        .dyn_into::<Element>()
        .ok()?
        .closest(selector)
        .ok()?
}

/// A click on a plot's `div` (`.koine-plot`): the pointer's offset from the top-left corner
/// of the drawn `<svg>` inside it, and the width that `<svg>` is drawn at, all
/// in CSS pixels. The `<svg>` is measured (not the `div`, which is as wide as
/// the column) so a plot CSS has shrunk to fit still maps back correctly.
pub(crate) fn plot_pixel(e: &MouseEvent) -> Option<(f64, f64, f64)> {
    let event = web_event(e)?;
    let svg = owner(&event, ".koine-plot")?.query_selector("svg").ok()??;
    let rect = svg.get_bounding_client_rect();
    Some((
        f64::from(event.client_x()) - rect.left(),
        f64::from(event.client_y()) - rect.top(),
        rect.width(),
    ))
}

/// A click on an `<img>` (`.koine-image`): the pointer's offset from its top-left corner and
/// the size it is displayed at, in CSS pixels.
pub(crate) fn image_pixel(e: &MouseEvent) -> Option<(f64, f64, f64, f64)> {
    let event = web_event(e)?;
    let rect = owner(&event, ".koine-image")?.get_bounding_client_rect();
    Some((
        f64::from(event.client_x()) - rect.left(),
        f64::from(event.client_y()) - rect.top(),
        rect.width(),
        rect.height(),
    ))
}

/// The `data-id` of the nearest element from the click's target upward.
pub(crate) fn data_id(e: &MouseEvent) -> Option<String> {
    let event = web_event(e)?;
    let target = event.target()?.dyn_into::<Element>().ok()?;
    target.closest("[data-id]").ok()??.get_attribute("data-id")
}

/// Keep the pointer's stream on the row grip it went down on
/// (`setPointerCapture`), so `pointermove` and `pointerup` reach the grip's
/// handlers wherever the pointer goes. A finger's pointer is captured to its
/// target by the browser already; a mouse's is not.
pub(crate) fn capture(e: &PointerEvent) {
    let Some(event) = e.data().downcast::<WebPointerEvent>().cloned() else {
        return;
    };
    let grip = event
        .target()
        .and_then(|t| t.dyn_into::<Element>().ok())
        .and_then(|t| t.closest(".koine-grip").ok().flatten());
    if let Some(grip) = grip {
        let _ = grip.set_pointer_capture(event.pointer_id());
    }
}

/// The drop target under the viewport point `(x, y)` in the tree `widget`:
/// the element there (`document.elementFromPoint`), climbed to the nearest
/// row or gap, kept only when it is inside this widget's `ul.koine-tree`.
pub(crate) fn drop_at_point(widget: &str, x: f64, y: f64) -> Option<DropAt> {
    let document = web_sys::window()?.document()?;
    let under = document.element_from_point(x as f32, y as f32)?;
    let hit = under.closest("[data-koine-row], [data-koine-gap]").ok()??;
    let tree = hit.closest("ul.koine-tree").ok()??;
    if tree.get_attribute("data-koine-id").as_deref() != Some(widget) {
        return None;
    }
    outline::drop_at(
        hit.get_attribute("data-koine-row").as_deref(),
        hit.get_attribute("data-koine-gap").as_deref(),
    )
}
