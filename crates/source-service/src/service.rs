//! `BibSourceService`: the six fixed verbs (S-D3) over the registry of
//! adapters. Every verb that reaches the `network` is `external` (the plan's
//! "read-only, external": nothing is written to the store, and
//! plan-koine-trust TR-D11 makes the reach the class); `explain` reads the
//! linked inventory and reaches nothing, so it alone is `read_only`.
//! Importing is imbib's own verb (S-D5).
//!
//! Federation is [`crate::federation`] (S-D7): every selected source asked
//! concurrently under a per-source timeout (default
//! [`federation::DEFAULT_TIMEOUT_MS`], `timeout_ms` per call), rows merged
//! by identifier only, every source's outcome named in `diagnostics`.
//! Etiquette is enforced once at the [`crate::etiquette::Gate`] before any
//! adapter is asked (S-D8). The scored cascade (S-D6) is a later package;
//! `match` today is "identifier hit = 1.0, else unscored".

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use impress_service_core::async_trait;
#[allow(unused_imports)]
use impress_service_macros::impress_method;
use impress_service_macros::{impress_service, impress_service_impl};

use koine_sources::{Diagnostic, Query, Status, Work};

use crate::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use crate::dto::{
    Candidate, ExplainAnswer, ExplainResult, ExportAnswer, ExportResult, Federation, FetchAnswer,
    FetchResult, MatchAnswer, MatchInput, MatchResult, RelatedResult, RowsAnswer, SearchResult,
};
use crate::etiquette::Gate;
use crate::federation::{self, merge_rows, merge_works, Outcome};
use crate::ids;
use crate::query::{is_empty, native_text, supports};
use crate::registry::{self, Registry};

/// `limit`'s default and ceiling, per source (S-D3).
pub const DEFAULT_LIMIT: u32 = 10;
pub const MAX_LIMIT: u32 = 100;

/// The source name on a diagnostic the service itself raises (an id no
/// adapter claims), as against one a source answered.
pub const SERVICE_SOURCE: &str = "bib-source-service";

/// The source answered and has no record for `id` (`not_found`), which is
/// not an answer: fetch-across-sources keeps asking.
pub fn not_found(source: &str, id: &str) -> Diagnostic {
    Diagnostic::not_found(source, id)
}

/// A transport, HTTP or parse failure at `source`.
pub fn failed(source: &str, error: impl std::fmt::Display) -> Diagnostic {
    Diagnostic::error(source, error.to_string())
}

/// Whether a diagnostic is the "no record" answer of [`not_found`].
fn is_miss(d: &Diagnostic) -> bool {
    d.status == Status::NotFound
}

#[impress_service]
pub trait BibSourceService: Send + Sync + 'static {
    /// The inventory (S-D9): every linked source's explain record — what it
    /// can search (`indexes` and their relations), sort by, look up
    /// (`identifiers`), fill, how it pages and what etiquette it needs — or
    /// one source's by name. Refuses an unknown source (`not-found`).
    #[impress_method]
    #[impress_example(name = "inventory", args = r#"{}"#, expect = r#"{"ok": true}"#)]
    async fn explain(&self, source: Option<String>) -> ExplainResult;

    /// Search the sources with one canonical query (S-D2: `{index, relation,
    /// term}` clauses under `and`/`or`, or a `native` passthrough). Answers
    /// `{rows, next, diagnostics}`: at most `limit` rows per source (default
    /// 10, at most 100), each a `bibliographic-work@1.0.0` with its `ids`
    /// and `provenance` (source, `source_id`); `next` to page; a diagnostic per source
    /// naming its outcome, and `unsupported` with the index for every clause
    /// a source could not map — never a silent fallback. `sources` defaults
    /// to those whose explain record covers every clause. The sources are
    /// asked concurrently, each under `timeout_ms` (default 8000; `explain`
    /// reports it): a slow source is `timed_out` and the others' rows still
    /// return. Rows from different sources that share a DOI, arXiv id, PMID
    /// or bibcode are one row (S-D7); `sort` (`year`, `cited_by`,
    /// `relevance`) orders the merged rows, else the first source's order.
    #[impress_method(returns_kind = "bibliographic-work")]
    #[impress_example(
        name = "an-index-no-source-covers",
        args = r#"{"query": {"index": "similar_to", "relation": "any", "term": "10.1126/science.1063991"}, "limit": 1}"#,
        expect = r#"{"ok": true, "rows": []}"#
    )]
    async fn search(
        &self,
        query: Query,
        sources: Option<Vec<String>>,
        limit: Option<u32>,
        cursor: Option<String>,
        sort: Option<String>,
        timeout_ms: Option<u64>,
    ) -> SearchResult;

    /// One record by any canonical identifier (`10.1126/science.1063991`,
    /// `doi:…`, a doi.org URL, `arXiv:2101.00001`, `pmid:…`, `bibcode:…`),
    /// merged across the sources that claim the id (S-D7). Refuses text
    /// that is not an identifier (`invalid-argument`) and an id no source
    /// has (`not-found`, with each source's diagnostic in `message`).
    #[impress_method]
    #[impress_example(
        name = "not-an-identifier",
        args = r#"{"id": "Abel 2002"}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn fetch(
        &self,
        id: String,
        sources: Option<Vec<String>>,
        timeout_ms: Option<u64>,
    ) -> FetchResult;

    /// Resolve a reference — a free string or its structured fields — to
    /// candidates, each with `score` and `evidence` (S-D6). An identifier in
    /// the input is fetched and scores 1 (`via: "identifier"`, accepted);
    /// otherwise the fields become a search and the candidates come back
    /// unscored for a person or an agent to choose (the scorer is a later
    /// package). Nothing auto-merges below threshold. (The plan calls this
    /// verb `match`; `match` is a Rust keyword, so the method is
    /// `match_citation`.)
    #[impress_method]
    #[impress_example(
        name = "nothing-to-match",
        args = r#"{"citation": {"text": "   "}}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn match_citation(
        &self,
        citation: MatchInput,
        sources: Option<Vec<String>>,
    ) -> MatchResult;

    /// Works related to `id` by `relation` (`cites`, `cited_by`, `similar`,
    /// `coreads`), `{rows, next, diagnostics}`, from the sources that claim
    /// the id; a relation a source cannot walk is an `unsupported`
    /// diagnostic. Refuses text that is not an identifier.
    #[impress_method(returns_kind = "bibliographic-work")]
    #[impress_example(
        name = "not-an-identifier",
        args = r#"{"id": "Abel 2002", "relation": "cites"}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn related(
        &self,
        id: String,
        relation: Relation,
        limit: Option<u32>,
        cursor: Option<String>,
        timeout_ms: Option<u64>,
    ) -> RelatedResult;

    /// The records `ids` name as `bibtex`, `ris` or `csl-json` text, each
    /// id from the source that claims it. Refuses an empty list
    /// (`invalid-argument`); an id no source could export is a diagnostic.
    #[impress_method]
    #[impress_example(
        name = "no-ids",
        args = r#"{"ids": [], "format": "bibtex"}"#,
        expect = r#"{"ok": false, "code": "invalid-argument"}"#
    )]
    async fn export(&self, ids: Vec<String>, format: ExportFormat) -> ExportResult;
}

/// The service over a registry and an etiquette gate: the process-wide
/// ones by default, a test's own with [`DefaultBibSourceService::with_registry`]
/// and [`DefaultBibSourceService::with_gate`].
#[derive(Clone, Default)]
pub struct DefaultBibSourceService {
    registry: Option<Registry>,
    gate: Option<Arc<Gate>>,
}

impl DefaultBibSourceService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_registry(registry: Registry) -> Self {
        DefaultBibSourceService {
            registry: Some(registry),
            gate: None,
        }
    }

    /// Dispatch through `gate` instead of the process-wide one.
    pub fn with_gate(mut self, gate: Arc<Gate>) -> Self {
        self.gate = Some(gate);
        self
    }

    fn registry(&self) -> Registry {
        self.registry
            .clone()
            .unwrap_or_else(|| registry::global().clone())
    }

    fn gate(&self) -> Arc<Gate> {
        self.gate.clone().unwrap_or_else(crate::etiquette::gate)
    }

    /// The adapters `sources` names, or `Err(message)` naming the unknown one.
    fn named(&self, sources: &[String]) -> Result<Vec<Arc<dyn Adapter>>, String> {
        let registry = self.registry();
        let mut out = Vec::new();
        for name in sources {
            match registry.get(name) {
                Some(a) => out.push(a),
                None => {
                    return Err(format!(
                        "unknown source `{name}`; explain lists {}",
                        list(&registry.sources())
                    ))
                }
            }
        }
        Ok(out)
    }

    /// The adapters that declare `operation`.
    fn supporting(&self, operation: &str) -> Vec<Arc<dyn Adapter>> {
        self.registry()
            .adapters()
            .into_iter()
            .filter(|a| supports(&a.explain(), operation))
            .collect()
    }

    /// The adapters that claim `id` and declare `operation`.
    fn claiming(&self, id: &str, operation: &str) -> Vec<Arc<dyn Adapter>> {
        self.supporting(operation)
            .into_iter()
            .filter(|a| a.detect(id))
            .collect()
    }

    /// Run `query` over `adapters` concurrently (S-D7), merged by identifier
    /// and ordered by `sort` or the first source's order.
    async fn run_search(
        &self,
        query: &Query,
        adapters: &[Arc<dyn Adapter>],
        limit: u32,
        cursors: &BTreeMap<String, String>,
        sort: Option<&str>,
        timeout: Duration,
    ) -> RowsAnswer {
        let mut answer = RowsAnswer::default();
        // What each source is asked: the sort only when it declares it.
        let mut requests: Vec<PageRequest> = Vec::with_capacity(adapters.len());
        for adapter in adapters {
            let record = adapter.explain();
            let mut sort_for = None;
            if let Some(sort) = sort {
                if sort == "relevance" || record.sortable.iter().any(|s| s == sort) {
                    sort_for = Some(sort.to_string());
                } else {
                    answer.diagnostics.push(Diagnostic::unsupported(
                        &record.source,
                        format!(
                            "sort {sort}: the sortable fields are {}",
                            list(&record.sortable)
                        ),
                    ));
                }
            }
            requests.push(PageRequest {
                limit,
                cursor: cursors.get(&record.source).cloned(),
                sort: sort_for,
            });
        }
        let query = Arc::new(query.clone());
        let gate = self.gate();
        let outcomes = federation::run_all(adapters, &gate, timeout, |i, adapter| {
            let page = requests[i].clone();
            let query = query.clone();
            Box::pin(async move { adapter.search(&query, page).await })
        })
        .await;
        self.collect(&mut answer, outcomes, sort);
        answer
    }

    /// Fold the outcomes into the answer: rows merged and ordered, the
    /// cursor map, every diagnostic in source order.
    fn collect(&self, answer: &mut RowsAnswer, outcomes: Vec<Outcome>, sort: Option<&str>) {
        answer.rows = merge_rows(&outcomes, sort);
        let mut next: BTreeMap<String, String> = BTreeMap::new();
        for outcome in outcomes {
            if let Some(cursor) = outcome.next {
                next.insert(outcome.source.clone(), cursor);
            }
            answer.diagnostics.extend(outcome.diagnostics);
        }
        answer.next = encode_cursors(&next);
    }

    /// Fetch `id` from every adapter in `adapters` concurrently, merged by
    /// source priority.
    async fn run_fetch(
        &self,
        id: &str,
        adapters: &[Arc<dyn Adapter>],
        timeout: Duration,
    ) -> (Option<Work>, Vec<Diagnostic>) {
        let gate = self.gate();
        let id = Arc::new(id.to_string());
        let outcomes = federation::run_all(adapters, &gate, timeout, |_, adapter| {
            let id = id.clone();
            Box::pin(async move {
                let source = adapter.explain().source;
                match adapter.fetch(&id).await? {
                    Some(work) => Ok(Page {
                        rows: vec![work],
                        ..Default::default()
                    }),
                    None => Ok(Page {
                        diagnostics: vec![not_found(&source, &id)],
                        ..Default::default()
                    }),
                }
            })
        })
        .await;
        let mut found: Vec<Work> = Vec::new();
        let mut diagnostics = Vec::new();
        for outcome in outcomes {
            // A miss is that source's `not_found` page, never `done`
            // (`run_all` marks only a clean page `done`).
            found.extend(outcome.rows);
            diagnostics.extend(outcome.diagnostics);
        }
        let work = (!found.is_empty()).then(|| merge_works(found));
        (work, diagnostics)
    }

    /// `relation` of `id` from every adapter concurrently.
    async fn run_related(
        &self,
        id: &str,
        relation: Relation,
        adapters: &[Arc<dyn Adapter>],
        limit: u32,
        cursors: &BTreeMap<String, String>,
        timeout: Duration,
    ) -> RowsAnswer {
        let mut answer = RowsAnswer::default();
        let gate = self.gate();
        let id = Arc::new(id.to_string());
        let cursors = Arc::new(cursors.clone());
        let outcomes = federation::run_all(adapters, &gate, timeout, |_, adapter| {
            let id = id.clone();
            let cursors = cursors.clone();
            Box::pin(async move {
                let source = adapter.explain().source;
                let page = PageRequest {
                    limit,
                    cursor: cursors.get(&source).cloned(),
                    sort: None,
                };
                adapter.related(&id, relation, page).await
            })
        })
        .await;
        self.collect(&mut answer, outcomes, None);
        answer
    }
}

/// The per-source timeout a call asked for, or the default; zero is refused.
fn check_timeout(timeout_ms: Option<u64>) -> Result<Duration, String> {
    match timeout_ms {
        None => Ok(Duration::from_millis(federation::DEFAULT_TIMEOUT_MS)),
        Some(0) => Err("`timeout_ms` must be at least 1".into()),
        Some(ms) => Ok(Duration::from_millis(ms)),
    }
}

fn status_word(status: Status) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

/// The service's cursor is a JSON map of each source's own, so one string
/// pages every source at once; a bare string is a single source's cursor.
fn encode_cursors(next: &BTreeMap<String, String>) -> Option<String> {
    if next.is_empty() {
        None
    } else {
        serde_json::to_string(next).ok()
    }
}

fn decode_cursors(
    cursor: Option<&str>,
    sources: &[Arc<dyn Adapter>],
) -> Result<BTreeMap<String, String>, String> {
    let Some(cursor) = cursor.map(str::trim).filter(|c| !c.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    if let Ok(map) = serde_json::from_str::<BTreeMap<String, String>>(cursor) {
        return Ok(map);
    }
    match sources {
        [one] => Ok(BTreeMap::from([(one.explain().source, cursor.to_string())])),
        _ => Err("`cursor` must be a `next` value from a previous call".into()),
    }
}

fn check_limit(limit: Option<u32>) -> Result<u32, String> {
    match limit {
        None => Ok(DEFAULT_LIMIT),
        Some(0) => Err("`limit` must be at least 1".into()),
        Some(n) if n > MAX_LIMIT => Err(format!(
            "`limit` is at most {MAX_LIMIT} per source, got {n}"
        )),
        Some(n) => Ok(n),
    }
}

/// The structured query `match` builds from a reference's fields.
fn match_query(citation: &MatchInput) -> Option<Query> {
    let mut parts = Vec::new();
    if let Some(title) = citation
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        parts.push(Query::clause("title", "any", title));
    }
    if let Some(first) = citation
        .authors
        .first()
        .map(|a| a.trim())
        .filter(|a| !a.is_empty())
    {
        parts.push(Query::clause("author", "any", first));
    }
    if let Some(year) = citation.year {
        parts.push(Query::clause("year", "=", year.to_string()));
    }
    if let Some(container) = citation
        .container
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
    {
        parts.push(Query::clause("container", "any", container));
    }
    if parts.is_empty() {
        let text = citation
            .text
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())?;
        return Some(Query::clause("any", "any", text));
    }
    Some(if parts.len() == 1 {
        parts.pop().expect("one part")
    } else {
        Query::And { and: parts }
    })
}

#[async_trait::async_trait]
impl BibSourceService for DefaultBibSourceService {
    async fn explain(&self, source: Option<String>) -> ExplainResult {
        let registry = self.registry();
        let contact = crate::etiquette::contact();
        let gate = self.gate();
        // The declaration, with today's budget count filled in (S-D8).
        let with_budget = |adapter: &Arc<dyn Adapter>| {
            let mut record = adapter.explain();
            if let Some(budget) = gate.budget_of(&record) {
                record.etiquette.budget = Some(budget);
            }
            record
        };
        match source {
            None => ExplainResult::answer(ExplainAnswer {
                contact,
                federation: Federation::default(),
                sources: registry.adapters().iter().map(with_budget).collect(),
            }),
            Some(name) => match registry.get(&name) {
                Some(adapter) => ExplainResult::answer(ExplainAnswer {
                    contact,
                    federation: Federation::default(),
                    sources: vec![with_budget(&adapter)],
                }),
                None => ExplainResult::refused(
                    "not-found",
                    format!(
                        "no source `{name}`; the linked sources are {}",
                        list(&registry.sources())
                    ),
                ),
            },
        }
    }

    async fn search(
        &self,
        query: Query,
        sources: Option<Vec<String>>,
        limit: Option<u32>,
        cursor: Option<String>,
        sort: Option<String>,
        timeout_ms: Option<u64>,
    ) -> SearchResult {
        if let Err(message) = query.validate() {
            return SearchResult::refused("invalid-argument", message);
        }
        let timeout = match check_timeout(timeout_ms) {
            Ok(t) => t,
            Err(message) => return SearchResult::refused("invalid-argument", message),
        };
        if is_empty(&query) {
            return SearchResult::refused("invalid-argument", "the query has no clause: give `{index, relation, term}`, `and`/`or` of those, or `native`");
        }
        let limit = match check_limit(limit) {
            Ok(n) => n,
            Err(message) => return SearchResult::refused("invalid-argument", message),
        };
        let mut answer = RowsAnswer::default();
        let adapters: Vec<Arc<dyn Adapter>> = match sources {
            Some(names) => {
                if names.is_empty() {
                    return SearchResult::refused("invalid-argument", "`sources` names no source; omit it to search every source that covers the query");
                }
                match self.named(&names) {
                    Ok(a) => a,
                    Err(message) => return SearchResult::refused("invalid-argument", message),
                }
            }
            None => {
                // Default: the sources whose explain record covers the query
                // (S-D3); the others say so, each with the first index it lacks.
                let mut covering = Vec::new();
                for adapter in self.supporting("search") {
                    let record = adapter.explain();
                    let mut missing = query.covered_by(&record);
                    if missing.is_empty() {
                        covering.push(adapter);
                    } else {
                        answer.diagnostics.push(missing.remove(0));
                    }
                }
                let _ = native_text(&query);
                covering
            }
        };
        let cursors = match decode_cursors(cursor.as_deref(), &adapters) {
            Ok(c) => c,
            Err(message) => return SearchResult::refused("invalid-argument", message),
        };
        let ran = self
            .run_search(&query, &adapters, limit, &cursors, sort.as_deref(), timeout)
            .await;
        answer.rows = ran.rows;
        answer.next = ran.next;
        answer.diagnostics.extend(ran.diagnostics);
        SearchResult::answer(answer)
    }

    async fn fetch(
        &self,
        id: String,
        sources: Option<Vec<String>>,
        timeout_ms: Option<u64>,
    ) -> FetchResult {
        let timeout = match check_timeout(timeout_ms) {
            Ok(t) => t,
            Err(message) => return FetchResult::refused("invalid-argument", message),
        };
        let Some(identifier) = ids::parse(&id) else {
            return FetchResult::refused(
                "invalid-argument",
                format!("`{id}` is not a canonical identifier (a DOI, an arXiv id, `pmid:`, `bibcode:`, …); use search for text"),
            );
        };
        let canonical = identifier.canonical();
        let adapters = match sources {
            Some(names) => match self.named(&names) {
                Ok(a) => a,
                Err(message) => return FetchResult::refused("invalid-argument", message),
            },
            None => self.claiming(&canonical, "fetch"),
        };
        if adapters.is_empty() {
            return FetchResult::refused(
                "not-found",
                format!(
                    "no linked source claims {canonical}; the sources are {}",
                    list(&self.registry().sources())
                ),
            );
        }
        let (work, diagnostics) = self.run_fetch(&canonical, &adapters, timeout).await;
        match work {
            Some(work) => FetchResult::answer(FetchAnswer { work, diagnostics }),
            None => FetchResult::refused(
                "not-found",
                format!(
                    "{canonical}: {}",
                    diagnostics
                        .iter()
                        .map(|d| format!(
                            "{} {}{}",
                            d.source,
                            if is_miss(d) {
                                "not-found".to_string()
                            } else {
                                status_word(d.status)
                            },
                            if d.detail.is_empty() {
                                String::new()
                            } else {
                                format!(" ({})", d.detail)
                            }
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
        }
    }

    async fn match_citation(
        &self,
        citation: MatchInput,
        sources: Option<Vec<String>>,
    ) -> MatchResult {
        let named = match &sources {
            Some(names) => match self.named(names) {
                Ok(a) => Some(a),
                Err(message) => return MatchResult::refused("invalid-argument", message),
            },
            None => None,
        };
        let timeout = Duration::from_millis(federation::DEFAULT_TIMEOUT_MS);
        // 1. Identifier first (S-D6): a hit is `via: identifier`, score 1.
        let mut found: Vec<ids::Identifier> = Vec::new();
        for (kind, value) in [("doi", &citation.doi), ("arxiv", &citation.arxiv)] {
            if let Some(v) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
                let id = ids::parse(v).unwrap_or_else(|| ids::Identifier::new(kind, v));
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
        if let Some(text) = &citation.text {
            for id in ids::scan(text) {
                if !found.contains(&id) {
                    found.push(id);
                }
            }
        }
        let mut answer = MatchAnswer::default();
        for identifier in &found {
            let canonical = identifier.canonical();
            let adapters = match &named {
                Some(a) => a.clone(),
                None => self.claiming(&canonical, "fetch"),
            };
            let (work, diagnostics) = self.run_fetch(&canonical, &adapters, timeout).await;
            answer.diagnostics.extend(diagnostics);
            if let Some(work) = work {
                answer.candidates.push(Candidate {
                    work,
                    score: Some(1.0),
                    evidence: BTreeMap::from([("identifier".to_string(), 1.0)]),
                    via: "identifier".into(),
                });
                answer.best = Some(0);
                answer.accepted = true;
                answer.rule = Some("identifier".into());
                return MatchResult::answer(answer);
            }
        }
        // 2. The local store: a later package (the service has no store port yet).
        // 3. Search-based matching; the validation scorer is S4, so candidates
        //    come back unscored.
        let Some(query) = match_query(&citation) else {
            return MatchResult::refused(
                "invalid-argument",
                "nothing to match: give `text`, or `title`/`authors`/`year`, or `doi`/`arxiv`",
            );
        };
        let adapters = match named {
            Some(a) => a,
            None => self
                .supporting("match")
                .into_iter()
                .filter(|a| query.covered_by(&a.explain()).is_empty())
                .collect(),
        };
        let ran = self
            .run_search(&query, &adapters, 5, &BTreeMap::new(), None, timeout)
            .await;
        answer.diagnostics.extend(ran.diagnostics);
        answer
            .candidates
            .extend(ran.rows.into_iter().map(|work| Candidate {
                work,
                score: None,
                evidence: BTreeMap::new(),
                via: "search".into(),
            }));
        answer.accepted = false;
        MatchResult::answer(answer)
    }

    async fn related(
        &self,
        id: String,
        relation: Relation,
        limit: Option<u32>,
        cursor: Option<String>,
        timeout_ms: Option<u64>,
    ) -> RelatedResult {
        let timeout = match check_timeout(timeout_ms) {
            Ok(t) => t,
            Err(message) => return RelatedResult::refused("invalid-argument", message),
        };
        let Some(identifier) = ids::parse(&id) else {
            return RelatedResult::refused(
                "invalid-argument",
                format!("`{id}` is not a canonical identifier (a DOI, an arXiv id, `pmid:`, `bibcode:`, …)"),
            );
        };
        let limit = match check_limit(limit) {
            Ok(n) => n,
            Err(message) => return RelatedResult::refused("invalid-argument", message),
        };
        let canonical = identifier.canonical();
        let adapters = self.claiming(&canonical, "related");
        if adapters.is_empty() {
            return RelatedResult::refused(
                "not-found",
                format!(
                    "no linked source walks relations of {canonical}; the sources are {}",
                    list(&self.registry().sources())
                ),
            );
        }
        let cursors = match decode_cursors(cursor.as_deref(), &adapters) {
            Ok(c) => c,
            Err(message) => return RelatedResult::refused("invalid-argument", message),
        };
        let answer = self
            .run_related(&canonical, relation, &adapters, limit, &cursors, timeout)
            .await;
        RelatedResult::answer(answer)
    }

    async fn export(&self, ids: Vec<String>, format: ExportFormat) -> ExportResult {
        if ids.is_empty() {
            return ExportResult::refused(
                "invalid-argument",
                "`ids` is empty; name at least one identifier",
            );
        }
        let mut answer = ExportAnswer {
            format,
            ..Default::default()
        };
        // Each id goes to the first adapter that claims it.
        let mut by_adapter: Vec<(Arc<dyn Adapter>, Vec<String>)> = Vec::new();
        for raw in &ids {
            let Some(identifier) = ids::parse(raw) else {
                answer.diagnostics.push(Diagnostic::new(
                    SERVICE_SOURCE,
                    Status::Unsupported,
                    format!("`{raw}` is not a canonical identifier"),
                ));
                continue;
            };
            let canonical = identifier.canonical();
            match self.claiming(&canonical, "export").into_iter().next() {
                Some(adapter) => {
                    let source = adapter.explain().source;
                    match by_adapter
                        .iter_mut()
                        .find(|(a, _)| a.explain().source == source)
                    {
                        Some((_, list)) => list.push(canonical),
                        None => by_adapter.push((adapter, vec![canonical])),
                    }
                }
                None => answer.diagnostics.push(Diagnostic::new(
                    SERVICE_SOURCE,
                    Status::Unsupported,
                    format!("no linked source exports {canonical}"),
                )),
            }
        }
        let mut pieces: Vec<String> = Vec::new();
        let mut csl: Vec<serde_json::Value> = Vec::new();
        let gate = self.gate();
        for (adapter, list) in &by_adapter {
            let record = adapter.explain();
            let source = record.source.clone();
            match gate.admit(&record) {
                Ok(warnings) => answer.diagnostics.extend(warnings),
                Err(refused) => {
                    answer.diagnostics.push(refused);
                    continue;
                }
            }
            match adapter.export(list, format).await {
                Ok(text) => {
                    answer.diagnostics.push(Diagnostic::done(&source));
                    match format {
                        ExportFormat::CslJson => {
                            match serde_json::from_str::<serde_json::Value>(&text) {
                                Ok(serde_json::Value::Array(items)) => {
                                    answer.count += items.len() as u32;
                                    csl.extend(items);
                                }
                                Ok(one) => {
                                    answer.count += 1;
                                    csl.push(one);
                                }
                                Err(e) => answer
                                    .diagnostics
                                    .push(failed(&source, format!("csl-json did not parse: {e}"))),
                            }
                        }
                        _ => {
                            answer.count += list.len() as u32;
                            pieces.push(text.trim().to_string());
                        }
                    }
                }
                Err(d) => answer.diagnostics.push(d),
            }
        }
        answer.text = match format {
            ExportFormat::CslJson => {
                serde_json::to_string_pretty(&csl).unwrap_or_else(|_| "[]".into())
            }
            _ => pieces.join("\n\n"),
        };
        ExportResult::answer(answer)
    }
}

impress_service_impl! {
    service = BibSourceService,
    safety = external,
    since = "0.1.0",
    effects = { reads: [], writes: [], reach: [network] },
    impl = DefaultBibSourceService,
    instance = DefaultBibSourceService::new,
    methods = [
        /// The explain records of every linked source, or of one.
        explain(
            /// A source id from the inventory (`crossref`); absent for all.
            source: Option<String>
        ) -> ExplainResult,
        /// Search the sources with one canonical query; `{rows, next, diagnostics}`.
        search(
            /// The query: `{"index": "author", "relation": "any", "term": "Abel"}`, or `{"and": [...]}`, `{"or": [...]}`, or `{"native": "..."}`. Indexes: any, title, abstract, author, first_author, year, container, doi, arxiv, pmid, bibcode, isbn, issn, orcid, keyword, subject, cites, cited_by, similar_to.
            query: Query,
            /// Source ids to ask; absent means every source whose explain record covers the query.
            sources: Option<Vec<String>>,
            /// Rows per source: default 10, at most 100.
            limit: Option<u32>,
            /// The `next` of a previous call, to page.
            cursor: Option<String>,
            /// One of the source's `sortable` fields (`relevance`, `year`, `cited_by`).
            sort: Option<String>,
            /// Per-source timeout in milliseconds; default 8000 (`explain` reports it).
            timeout_ms: Option<u64>
        ) -> SearchResult,
        /// One record by a canonical identifier, merged across sources.
        fetch(
            /// A DOI, an arXiv id, `pmid:…`, `bibcode:…`, or a doi.org / arxiv.org URL.
            id: String,
            /// Source ids to ask; absent means every source that claims the id.
            sources: Option<Vec<String>>,
            /// Per-source timeout in milliseconds; default 8000.
            timeout_ms: Option<u64>
        ) -> FetchResult,
        /// Resolve a reference to scored candidates.
        match_citation(
            /// The reference: `{"text": "..."}` as written, or `{"title", "authors", "year", "container", "doi", "arxiv"}`.
            citation: MatchInput,
            /// Source ids to ask; absent means every source that can.
            sources: Option<Vec<String>>
        ) -> MatchResult,
        /// Works related to an id by a relation; `{rows, next, diagnostics}`.
        related(
            /// A canonical identifier.
            id: String,
            /// `cites`, `cited_by`, `similar` or `coreads`.
            relation: Relation,
            /// Rows per source: default 10, at most 100.
            limit: Option<u32>,
            /// The `next` of a previous call, to page.
            cursor: Option<String>,
            /// Per-source timeout in milliseconds; default 8000.
            timeout_ms: Option<u64>
        ) -> RelatedResult,
        /// The records as `bibtex`, `ris` or `csl-json` text.
        export(
            /// Canonical identifiers, at least one.
            ids: Vec<String>,
            /// `bibtex`, `ris` or `csl-json`.
            format: ExportFormat
        ) -> ExportResult,
    ],
}
