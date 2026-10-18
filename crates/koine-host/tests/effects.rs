//! koine checks its own verbs' declared effects (ADR-0036 D1;
//! docs/plan-koine-kit-moves.md KM-D14): every Tier A example of every verb
//! this host links runs once, through the pipeline, on a fresh memory store
//! wrapped in a `SpyStore`, and every record kind it read or wrote must be one
//! its descriptor declares. An example that refuses still counts: what it
//! touched before refusing is still a touch.
//!
//! Each example runs twice and the second run is the one measured: a fresh
//! store has no layout, no shipped presets and no kit yet, and a read verb's
//! first call materializes those defaults once (impress's effects test
//! measures the same steady state by preparing fixtures first).

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use impress_item::ItemStore;
use impress_service_core::descriptor::{ExampleTier, Kind};
use impress_service_core::pipeline::{self, Call};
use impress_service_core::VerbDescriptor;
use koine_store_memory::MemoryStore;
use koine_store_spy::SpyStore;
use serde_json::Value;

/// The pipeline's own audit record: every non-read-only call writes one, by
/// construction. It is the pipeline's write, not the verb's effect.
const PIPELINE_AUDIT_KIND: &str = "core/verb-call@1.0.0";

/// How long one example may run before it is reported as a hang.
const EXAMPLE_TIMEOUT: Duration = Duration::from_secs(20);

/// The kinds of the records an argument names (`Target`) or the records
/// parented under them (`Children`), on `store`.
fn resolve(kind: &Kind, args: &Value, store: &dyn ItemStore) -> Option<Vec<String>> {
    let (arg, children) = match kind {
        Kind::Target(a) => (*a, false),
        Kind::Children(a) => (*a, true),
        _ => return None,
    };
    let ids: Vec<String> = match args.get(arg) {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(str::to_string))
            .collect(),
        _ => return None,
    };
    let mut kinds = Vec::new();
    for id in ids {
        let Ok(id) = id.parse() else { continue };
        if children {
            let q = impress_item::ItemQuery {
                predicates: vec![impress_item::Predicate::HasParent(id)],
                ..Default::default()
            };
            if let Ok(rows) = store.query(&q) {
                kinds.extend(rows.into_iter().map(|r| r.schema.to_string()));
            }
        } else if let Ok(Some(row)) = store.get(id) {
            kinds.push(row.schema.to_string());
        }
    }
    (!kinds.is_empty()).then_some(kinds)
}

fn covered(declared: &[Kind], observed: &str, args: &Value, store: &dyn ItemStore) -> bool {
    declared
        .iter()
        .any(|kind| kind.covers(observed, resolve(kind, args, store).as_deref()))
}

#[tokio::test(flavor = "multi_thread")]
async fn every_linked_verb_touches_only_what_it_declares() {
    koine_host::ensure_linked();
    koine_store::process::accept_override_of::<SpyStore<MemoryStore>>();
    // Verbs that keep files (the AI preferences, settings) write them under
    // the process workspace; keep that out of the checkout.
    let workspace = tempfile::tempdir().unwrap();
    koine_store::process::set_workspace(workspace.path());
    let mut problems = Vec::new();
    let mut ran = 0usize;
    let mut verbs: Vec<&'static VerbDescriptor> = VerbDescriptor::iter().collect();
    verbs.sort_by_key(|v| v.name);
    for verb in verbs {
        for example in verb.examples.iter().filter(|e| e.tier == ExampleTier::A) {
            let store = Arc::new(SpyStore::new(Arc::new(MemoryStore::new())));
            let args = example.args_value();
            let warm = tokio::time::timeout(
                EXAMPLE_TIMEOUT,
                pipeline::invoke_on(store.clone(), verb, Call::agent("effects", args.clone())),
            )
            .await;
            if warm.is_err() {
                problems.push(format!(
                    "`{}` example `{}` did not finish",
                    verb.name, example.name
                ));
                continue;
            }
            store.start();
            let outcome = tokio::time::timeout(
                EXAMPLE_TIMEOUT,
                pipeline::invoke_on(store.clone(), verb, Call::agent("effects", args.clone())),
            )
            .await;
            let mut seen = store.stop();
            if outcome.is_err() {
                problems.push(format!(
                    "`{}` example `{}` did not finish",
                    verb.name, example.name
                ));
                continue;
            }
            ran += 1;
            seen.reads.remove(PIPELINE_AUDIT_KIND);
            seen.writes.remove(PIPELINE_AUDIT_KIND);
            let inner: &dyn ItemStore = store.inner().as_ref();
            let undeclared = |kinds: &BTreeSet<String>, declared: &[Kind], what: &str| {
                kinds
                    .iter()
                    .filter(|k| !covered(declared, k, &args, inner))
                    .map(|k| format!("`{}` example `{}` {what} `{k}`", verb.name, example.name))
                    .collect::<Vec<_>>()
            };
            problems.extend(undeclared(&seen.reads, verb.effects.reads, "reads"));
            problems.extend(undeclared(&seen.writes, verb.effects.writes, "writes"));
        }
    }
    println!("effects: {ran} Tier A example(s) ran");
    assert!(ran > 0, "no Tier A example ran");
    assert!(
        problems.is_empty(),
        "a verb touched a kind it does not declare:\n- {}",
        problems.join("\n- ")
    );
}

/// plan-koine-trust TR-D3: a dry run changes nothing. Every Tier A example of
/// every linked verb runs again with `tentatively` — rehearsed on an overlay
/// where the verb stays in the process, answered statically otherwise — and
/// the base store sees no write but the pipeline's own call record.
#[tokio::test(flavor = "multi_thread")]
async fn every_linked_verb_rehearsed_leaves_the_store_unchanged() {
    koine_host::ensure_linked();
    koine_store::process::accept_override_of::<SpyStore<MemoryStore>>();
    koine_store::rehearse::install();
    let workspace = tempfile::tempdir().unwrap();
    koine_store::process::set_workspace(workspace.path());
    let mut problems = Vec::new();
    let (mut rehearsed, mut other) = (0usize, 0usize);
    let mut verbs: Vec<&'static VerbDescriptor> = VerbDescriptor::iter().collect();
    verbs.sort_by_key(|v| v.name);
    for verb in verbs {
        for example in verb.examples.iter().filter(|e| e.tier == ExampleTier::A) {
            let store = Arc::new(SpyStore::new(Arc::new(MemoryStore::new())));
            let args = example.args_value();
            // Warm up for real, as above, so defaults exist.
            let _ = tokio::time::timeout(
                EXAMPLE_TIMEOUT,
                pipeline::invoke_on(store.clone(), verb, Call::agent("effects", args.clone())),
            )
            .await;
            let call =
                Call::agent("effects", args.clone()).with_adverbs(impress_service_core::Adverbs {
                    tentatively: true,
                    ..Default::default()
                });
            store.start();
            let outcome = tokio::time::timeout(
                EXAMPLE_TIMEOUT,
                pipeline::invoke_on(store.clone(), verb, call),
            )
            .await;
            let mut seen = store.stop();
            let Ok(Ok(answer)) = outcome else {
                problems.push(format!(
                    "`{}` example `{}` did not answer tentatively",
                    verb.name, example.name
                ));
                continue;
            };
            match answer.get("tentative").and_then(Value::as_str) {
                Some("rehearsed") => rehearsed += 1,
                Some(_) => other += 1,
                None => problems.push(format!(
                    "`{}` example `{}` answered without a tentative tag: {answer}",
                    verb.name, example.name
                )),
            }
            seen.writes.remove(PIPELINE_AUDIT_KIND);
            if !seen.writes.is_empty() || !seen.inserted.is_empty() || !seen.deleted.is_empty() {
                problems.push(format!(
                    "`{}` example `{}` wrote during a dry run: {:?}",
                    verb.name, example.name, seen.writes
                ));
            }
        }
    }
    println!("dry runs: {rehearsed} rehearsed, {other} static or simulated");
    assert!(rehearsed > 0, "nothing was rehearsed");
    assert!(
        problems.is_empty(),
        "a dry run was not dry:\n- {}",
        problems.join("\n- ")
    );
}
