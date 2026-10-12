//! What the page reads off the browser's click: the one place `web-sys` is
//! touched. Each function answers `None` when the DOM does not say, and the
//! mapping then sends no event; nothing here decides what a click means.

use dioxus::prelude::MouseEvent;
use web_sys::wasm_bindgen::JsCast;
use web_sys::{Element, MouseEvent as WebMouseEvent};

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
