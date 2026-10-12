//! The layout verbs that persist outside the per-verb path — saving,
//! applying and deleting a layout, and the preset verbs — log under the
//! `layout` target with who asked, at the verb path's levels: `info` when
//! done, `warn` when refused (plan wave 7, T5's finding 3; wave 8 U2).
//!
//! Its own test binary, because the subscriber is process-wide.
//!
//! G7a moved these lines off the `log` facade onto `tracing` (D-P1); this
//! test captures them with a small `tracing_subscriber::Layer` instead of a
//! `log::Log`.

use impress_pane_query_compile::test_support::builtin_manifest;
use std::sync::{Arc, Mutex, OnceLock};

use impress_layout_service::{DefaultLayoutService, LayoutService};
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::{Context, SubscriberExt};
use tracing_subscriber::Layer;

const APP: &str = "imbib";

struct Capture(Mutex<Vec<(Level, String, String)>>);

struct MessageVisitor(String);
impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl<S: Subscriber> Layer<S> for &'static Capture {
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor(String::new());
        event.record(&mut visitor);
        self.0.lock().unwrap().push((
            *event.metadata().level(),
            event.metadata().target().to_string(),
            visitor.0,
        ));
    }
}

fn capture() -> &'static Capture {
    static CAPTURE: OnceLock<&'static Capture> = OnceLock::new();
    CAPTURE.get_or_init(|| {
        let capture: &'static Capture = Box::leak(Box::new(Capture(Mutex::new(Vec::new()))));
        let subscriber = tracing_subscriber::registry().with(capture);
        tracing::subscriber::set_global_default(subscriber)
            .expect("the only subscriber in this binary");
        capture
    })
}

/// The one `layout` line that mentions `needle`, with its level.
fn line_with(needle: &str) -> (Level, String) {
    let lines = capture().0.lock().unwrap();
    let found: Vec<_> = lines
        .iter()
        .filter(|(_, target, message)| target == "layout" && message.contains(needle))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "expected one `layout` line with {needle:?}; have {:#?}",
        *lines
    );
    (found[0].0, found[0].2.clone())
}

fn device() -> Option<String> {
    Some("logging-device".to_string())
}

#[tokio::test]
async fn save_apply_delete_and_the_preset_refusals_are_logged_with_their_actor() {
    capture();
    let svc = DefaultLayoutService::with_store(
        Arc::new(koine_store_memory::MemoryStore::new()),
        builtin_manifest(),
    );
    let human = || Some("human".to_string());
    let agent = || Some("agent".to_string());

    let saved = svc
        .save_layout(APP.into(), device(), "Logged".into(), None, human())
        .await;
    assert!(saved.ok, "{}", saved.message);
    let (level, line) = line_with("save_layout 'Logged'");
    assert_eq!(level, Level::INFO, "{line}");
    assert!(line.starts_with("imbib: human save_layout"), "{line}");

    let applied = svc
        .apply_layout(
            APP.into(),
            device(),
            Some("Logged".into()),
            None,
            agent(),
            None,
        )
        .await;
    assert!(applied.ok, "{}", applied.message);
    let (level, line) = line_with("apply_layout 'Logged'");
    assert_eq!(level, Level::INFO, "{line}");
    assert!(line.starts_with("imbib: agent apply_layout"), "{line}");

    let deleted = svc
        .delete_layout(APP.into(), "Logged".into(), human())
        .await;
    assert!(deleted.ok, "{}", deleted.message);
    let (level, line) = line_with("delete_layout 'Logged'");
    assert_eq!(level, Level::INFO, "{line}");
    assert!(line.starts_with("imbib: human delete_layout"), "{line}");

    // A preset is never deleted: refused, and the refusal is logged.
    let refused = svc
        .delete_layout(APP.into(), "Triage".into(), agent())
        .await;
    assert!(!refused.ok);
    let (level, line) = line_with("delete_layout 'Triage' refused");
    assert_eq!(level, Level::WARN, "{line}");
    assert!(
        line.contains("agent") && line.contains("[preset-not-deletable]"),
        "{line}"
    );

    // `save_preset` refuses anything but the live arrangement before it
    // touches a session; that refusal is logged too.
    let refused = svc
        .save_preset(APP.into(), device(), "Mine".into(), None, false, human())
        .await;
    assert!(!refused.ok);
    let (level, line) = line_with("save_preset 'Mine' refused");
    assert_eq!(level, Level::WARN, "{line}");
    assert!(line.starts_with("imbib: human save_preset"), "{line}");

    // And a commit of a kind nothing can materialize yet.
    let refused = svc
        .commit(
            APP.into(),
            device(),
            "figure".into(),
            "Fig".into(),
            None,
            agent(),
        )
        .await;
    assert!(!refused.ok);
    let (level, line) = line_with("commit 'Fig' as 'figure' refused");
    assert_eq!(level, Level::WARN, "{line}");
    assert!(line.contains("[invalid-argument]"), "{line}");
}
