//! `plan(&spec, &state, &params, &SourceCache) -> Vec<SourceRequest>`: which
//! sources need fetching, in dependency order, skipping what a cache already has
//! fresh (ADR-0033 "Defaults": "sources are cached by resolved arguments and
//! re-run only when an argument changes, a store invalidation names a query, or
//! an action refreshes them explicitly").
//!
//! This function is pure and synchronous: it never runs a verb or a query itself
//! (that is `impress-surface-service`'s `Executor`, S4) — it only decides *which*
//! ones the runtime should, this round, by resolving each source's templated
//! `args` against `state`/`params`/already-cached source values and hashing the
//! result.
//!
//! # A source blocked on an unfetched dependency is simply omitted this round
//!
//! `docs/plan-agent-surfaces.md` leaves the exact shape of "not ready yet" open
//! ("deferred (returned with `blocked_on: Vec<String>`, or simply omitted until
//! the next plan — choose and document)"). This module omits it, for two
//! reasons: first, the task's own [`SourceRequest`] shape has no `blocked_on`
//! field to put it in; second, the caller already re-plans after every fetch
//! lands in the cache (S4's runtime loop), so a source's absence from one
//! `plan()` call is corrected by the very next one rather than needing its own
//! signal. A source is "ready" the moment its name is a key in the cache — even
//! if that entry's `args_hash` no longer matches (it is about to be refetched):
//! its last known value is the best available approximation for a downstream
//! source's args in the same round, and using it (rather than blocking on the
//! *freshest* value) is what lets an unrelated part of the source graph settle
//! without waiting on a slow one.

use std::collections::hash_map::DefaultHasher;
use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use serde_json::Value;

use crate::spec::{PaneQuery, Source, SurfaceSpec};
use crate::template::{resolve_value, source_refs_in, Context};

/// What to fetch for one source: either call a verb with resolved arguments, or
/// run a pane query.
#[derive(Debug, Clone, PartialEq)]
pub enum SourceRequestKind {
    Verb { verb: String, args: Value },
    Query { query: PaneQuery },
}

/// One source that needs (re)fetching this round.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRequest {
    pub name: String,
    pub kind: SourceRequestKind,
    /// A hash of the resolved request (args, or the query plus the params that
    /// feed its bindings) — the same value the caller should store back into the
    /// [`SourceCache`] alongside the fetched result, so the *next* `plan()` call
    /// can recognize this exact request as already satisfied.
    pub args_hash: u64,
}

/// One source's last known fetch: what it was fetched *with* (`args_hash`) and
/// what it returned (`value`).
#[derive(Debug, Clone, PartialEq)]
pub struct CachedSource {
    pub args_hash: u64,
    pub value: Value,
}

/// Per-surface-instance cache, keyed by source name. Owned by the caller (S4's
/// `SurfaceRuntime`) — this crate never holds state across calls.
pub type SourceCache = BTreeMap<String, CachedSource>;

/// See the module docs.
pub fn plan(
    spec: &SurfaceSpec,
    state: &Value,
    params: &Value,
    cache: &SourceCache,
) -> Vec<SourceRequest> {
    let available: Value = Value::Object(
        cache
            .iter()
            .map(|(name, cached)| (name.clone(), cached.value.clone()))
            .collect(),
    );
    let null = Value::Null;
    let ctx = Context::new(state, params, &available, &null);

    let order = topological_order(spec);
    let mut requests = Vec::new();

    for name in order {
        let Some(source) = spec.sources.get(&name) else {
            continue;
        };
        match source {
            Source::Value { .. } => {
                // A fixed value never needs fetching — it has no request kind to
                // give the runtime. `resolve.rs` reads it straight out of the
                // spec.
            }
            Source::Verb { verb, args } => {
                let deps = source_refs_in(args);
                if !deps.iter().all(|d| cache.contains_key(d)) {
                    // A dependency has never been fetched: defer to the next
                    // `plan()` call (see the module docs).
                    continue;
                }
                let Ok(resolved_args) = resolve_value(args, &ctx) else {
                    // A `state`/`param` reference the args need is not present
                    // yet either — same deferral, for the same reason.
                    continue;
                };
                let args_hash = hash_json(&resolved_args);
                if cache.get(&name).map(|c| c.args_hash) == Some(args_hash) {
                    continue;
                }
                requests.push(SourceRequest {
                    name: name.clone(),
                    kind: SourceRequestKind::Verb {
                        verb: verb.clone(),
                        args: resolved_args,
                    },
                    args_hash,
                });
            }
            Source::Query { query } => {
                // A `PaneQuery`'s `$param` bindings are filled by the pane
                // executor from `params`, not by this crate's template system
                // (see `crate::spec::Source::Query`), so the request hashes the
                // query together with the current params: a query's re-fetch
                // trigger is "the query text changed" or "a bound param
                // changed", never a `state`/`source` template (it has none).
                let args_hash = hash_json(&serde_json::json!({
                    "query": query,
                    "params": params,
                }));
                if cache.get(&name).map(|c| c.args_hash) == Some(args_hash) {
                    continue;
                }
                requests.push(SourceRequest {
                    name: name.clone(),
                    kind: SourceRequestKind::Query {
                        query: query.clone(),
                    },
                    args_hash,
                });
            }
        }
    }

    requests
}

/// A deterministic hash of a JSON value's canonical (key-sorted —
/// `serde_json::Value`'s object map is a `BTreeMap` in this workspace) text
/// form. `u64` per the task's `SourceRequest::args_hash` field, using the
/// standard library's hasher rather than adding a hashing dependency this
/// crate's Cargo.toml does not list.
fn hash_json(value: &Value) -> u64 {
    let mut hasher = DefaultHasher::new();
    serde_json::to_string(value)
        .unwrap_or_default()
        .hash(&mut hasher);
    hasher.finish()
}

/// `spec.sources` in dependency order (a source before anything that names it
/// in `source.<name>`), ties broken by name for determinism. Falls back to name
/// order for any source `find_cycle` (in `validate.rs`) would have flagged —
/// `plan()` assumes a validated spec, so a cyclic one gets *some* order rather
/// than a special error path here.
fn topological_order(spec: &SurfaceSpec) -> Vec<String> {
    let mut deps: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for (name, source) in &spec.sources {
        let refs = match source {
            Source::Verb { args, .. } => source_refs_in(args)
                .into_iter()
                .filter(|r| spec.sources.contains_key(r))
                .collect(),
            _ => Vec::new(),
        };
        deps.insert(name.as_str(), refs);
    }

    let mut visited: BTreeSet<&str> = BTreeSet::new();
    let mut visiting: BTreeSet<&str> = BTreeSet::new();
    let mut out: Vec<String> = Vec::new();

    fn visit<'a>(
        name: &'a str,
        deps: &'a BTreeMap<&'a str, Vec<String>>,
        visited: &mut BTreeSet<&'a str>,
        visiting: &mut BTreeSet<&'a str>,
        out: &mut Vec<String>,
    ) {
        if visited.contains(name) || visiting.contains(name) {
            return; // already placed, or a cycle back-edge — stop here.
        }
        visiting.insert(name);
        if let Some(refs) = deps.get(name) {
            for r in refs {
                visit(r.as_str(), deps, visited, visiting, out);
            }
        }
        visiting.remove(name);
        visited.insert(name);
        out.push(name.to_string());
    }

    for name in spec.sources.keys() {
        visit(name.as_str(), &deps, &mut visited, &mut visiting, &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{Node, NodeKind};

    fn spec_with_series_and_hist() -> SurfaceSpec {
        let mut sources = BTreeMap::new();
        sources.insert(
            "series".to_string(),
            Source::Verb {
                verb: "surface-demo-service_series".to_string(),
                args: serde_json::json!({"freq": "{{state.freq}}", "n": 512}),
            },
        );
        sources.insert(
            "hist".to_string(),
            Source::Verb {
                verb: "surface-demo-service_histogram".to_string(),
                args: serde_json::json!({"values": "{{source.series.values}}", "bins": "{{state.bins}}"}),
            },
        );
        SurfaceSpec {
            surface: "1.0".to_string(),
            name: "test".to_string(),
            params: Vec::new(),
            state: serde_json::json!({"freq": 1.0, "bins": 20}),
            sources,
            root: Node::leaf(NodeKind::Spacer),
        }
    }

    #[test]
    fn the_first_plan_only_requests_the_dependency_free_source() {
        let spec = spec_with_series_and_hist();
        let cache = SourceCache::new();
        let requests = plan(&spec, &spec.state, &Value::Null, &cache);
        let names: Vec<&str> = requests.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["series"]);
    }

    #[test]
    fn once_series_is_cached_hist_is_requested_next() {
        let spec = spec_with_series_and_hist();
        let mut cache = SourceCache::new();
        cache.insert(
            "series".to_string(),
            CachedSource {
                args_hash: 0, // deliberately stale, to also prove series re-requests
                value: serde_json::json!({"values": [1, 2, 3]}),
            },
        );
        let requests = plan(&spec, &spec.state, &Value::Null, &cache);
        let names: Vec<&str> = requests.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["series", "hist"]);
    }

    #[test]
    fn an_unchanged_source_is_omitted() {
        let spec = spec_with_series_and_hist();
        let mut cache = SourceCache::new();
        let resolved_args = serde_json::json!({"freq": 1.0, "n": 512});
        cache.insert(
            "series".to_string(),
            CachedSource {
                args_hash: hash_json(&resolved_args),
                value: serde_json::json!({"values": [1, 2, 3]}),
            },
        );
        let requests = plan(&spec, &spec.state, &Value::Null, &cache);
        let names: Vec<&str> = requests.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["hist"]);
    }

    #[test]
    fn a_changed_state_value_reruns_the_source_that_reads_it() {
        let spec = spec_with_series_and_hist();
        let mut cache = SourceCache::new();
        // Cached against the OLD freq (2.0); state now says 1.0.
        let stale_args = serde_json::json!({"freq": 2.0, "n": 512});
        cache.insert(
            "series".to_string(),
            CachedSource {
                args_hash: hash_json(&stale_args),
                value: serde_json::json!({"values": [9, 9, 9]}),
            },
        );
        let requests = plan(&spec, &spec.state, &Value::Null, &cache);
        assert!(requests.iter().any(|r| r.name == "series"));
    }

    #[test]
    fn hash_json_is_stable_regardless_of_object_key_order() {
        let a = serde_json::json!({"a": 1, "b": 2});
        let b = serde_json::json!({"b": 2, "a": 1});
        assert_eq!(hash_json(&a), hash_json(&b));
    }
}
