//! `SourceService`: the six fixed verbs (S-D3) over the registry of
//! adapters. Every verb is `read_only` on the store and reaches the
//! `network`; a search writes nothing, importing is imbib's own verb (S-D5).
//!
//! Federation here is the sequential first cut: sources are asked one after
//! another, rows merged by identifier only (S-D7), every source's outcome
//! named in `diagnostics`. The concurrent job with per-source timeouts and
//! the scored cascade (S-D6) are later packages; `match` today is
//! "identifier hit = 1.0, else unscored".

use std::collections::BTreeMap;
use std::sync::Arc;

use impress_service_core::async_trait;
use impress_service_macros::{impress_service, impress_service_impl};
#[allow(unused_imports)]
use impress_service_macros::impress_method;

use crate::adapter::{Adapter, ExportFormat, PageRequest, Relation};
use crate::dto::{
    Candidate, ExplainAnswer, ExplainResult, ExportAnswer, ExportResult, FetchAnswer, FetchResult,
    MatchAnswer, MatchInput, MatchResult, RelatedResult, RowsAnswer, SearchResult,
};
use crate::ids;
use crate::registry::{self, Registry};
use crate::types::{Diagnostic, DiagnosticStatus, Query, Work};

/// `limit`'s default and ceiling, per source (S-D3).
pub const DEFAULT_LIMIT: u32 = 10;
pub const MAX_LIMIT: u32 = 100;

/// The source name on a diagnostic the service itself raises (an id no
/// adapter claims), as against one a source answered.
pub const SERVICE_SOURCE: &str = "source-service";

#[impress_service]
pub trait SourceService: Send + Sync + 'static {
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
    /// 10, at most 100), each a `bibliographic-work@1` with its `ids`,
    /// `source` and `source_id`; `next` to page; a diagnostic per source
    /// naming its outcome, and `unsupported` with the index for every clause
    /// a source could not map — never a silent fallback. `sources` defaults
    /// to those whose explain record covers every clause.
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
    async fn fetch(&self, id: String, sources: Option<Vec<String>>) -> FetchResult;

    /// Resolve a reference — a free string or its structured fields — to
    /// candidates, each with `score` and `evidence` (S-D6). An identifier in
    /// the input is fetched and scores 1 (`via: "identifier"`, accepted);
    /// otherwise the fields become a search and the candidates come back
    /// unscored for a person or an agent to choose (the scorer is a later
    /// package). Nothing auto-merges below threshold.
    #[impress_method(aliases = ["source-service_match"])]
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

    /// Works related to `id` by `relation` (`cites`, `cited-by`, `similar`,
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

/// The service over a registry: the process-wide one by default, a test's
/// own with [`DefaultSourceService::with_registry`].
#[derive(Clone, Default)]
pub struct DefaultSourceService {
    registry: Option<Registry>,
}

impl DefaultSourceService {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_registry(registry: Registry) -> Self {
        DefaultSourceService {
            registry: Some(registry),
        }
    }

    fn registry(&self) -> Registry {
        self.registry
            .clone()
            .unwrap_or_else(|| registry::global().clone())
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
            .filter(|a| a.explain().supports(operation))
            .collect()
    }

    /// The adapters that claim `id` and declare `operation`.
    fn claiming(&self, id: &str, operation: &str) -> Vec<Arc<dyn Adapter>> {
        self.supporting(operation)
            .into_iter()
            .filter(|a| a.detect(id))
            .collect()
    }

    /// Run `query` over `adapters` (every adapter asked, sequentially),
    /// merged by identifier.
    async fn run_search(
        &self,
        query: &Query,
        adapters: &[Arc<dyn Adapter>],
        limit: u32,
        cursors: &BTreeMap<String, String>,
        sort: Option<&str>,
    ) -> RowsAnswer {
        let mut answer = RowsAnswer::default();
        let mut next: BTreeMap<String, String> = BTreeMap::new();
        for adapter in adapters {
            let record = adapter.explain();
            let source = record.source.clone();
            let mut sort_for = None;
            if let Some(sort) = sort {
                if sort == "relevance" || record.sortable.iter().any(|s| s == sort) {
                    sort_for = Some(sort.to_string());
                } else {
                    answer.diagnostics.push(
                        Diagnostic::new(&source, DiagnosticStatus::Unsupported).with_message(
                            format!("{source} cannot sort by `{sort}`; its sortable fields are {}", list(&record.sortable)),
                        ),
                    );
                }
            }
            let page = PageRequest {
                limit,
                cursor: cursors.get(&source).cloned(),
                sort: sort_for,
            };
            match adapter.search(query, page).await {
                Ok(page) => {
                    let had_problem = page.diagnostics.iter().any(|d| d.status != DiagnosticStatus::Done);
                    answer.diagnostics.extend(page.diagnostics);
                    if !had_problem {
                        answer.diagnostics.push(Diagnostic::done(&source));
                    }
                    if let Some(cursor) = page.next {
                        next.insert(source.clone(), cursor);
                    }
                    for row in page.rows {
                        merge_into(&mut answer.rows, row);
                    }
                }
                Err(diagnostic) => answer.diagnostics.push(diagnostic),
            }
        }
        answer.next = encode_cursors(&next);
        answer
    }

    /// Fetch `id` from every adapter in `adapters`, merged.
    async fn run_fetch(&self, id: &str, adapters: &[Arc<dyn Adapter>]) -> (Option<Work>, Vec<Diagnostic>) {
        let mut merged: Option<Work> = None;
        let mut diagnostics = Vec::new();
        for adapter in adapters {
            let source = adapter.explain().source;
            match adapter.fetch(id).await {
                Ok(Some(work)) => {
                    diagnostics.push(Diagnostic::done(&source));
                    match merged.as_mut() {
                        Some(m) => m.merge(work),
                        None => merged = Some(work),
                    }
                }
                Ok(None) => diagnostics.push(Diagnostic::not_found(&source, id)),
                Err(d) => diagnostics.push(d),
            }
        }
        (merged, diagnostics)
    }
}

fn list(items: &[String]) -> String {
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

/// Add `row` to `rows`, folding it into an existing row that shares an id.
fn merge_into(rows: &mut Vec<Work>, row: Work) {
    if let Some(existing) = rows.iter_mut().find(|r| r.shares_id(&row)) {
        existing.merge(row);
    } else {
        rows.push(row);
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

fn decode_cursors(cursor: Option<&str>, sources: &[Arc<dyn Adapter>]) -> Result<BTreeMap<String, String>, String> {
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
        Some(n) if n > MAX_LIMIT => Err(format!("`limit` is at most {MAX_LIMIT} per source, got {n}")),
        Some(n) => Ok(n),
    }
}

/// The structured query `match` builds from a reference's fields.
fn match_query(citation: &MatchInput) -> Option<Query> {
    let mut parts = Vec::new();
    if let Some(title) = citation.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        parts.push(Query::clause("title", "any", title));
    }
    if let Some(first) = citation.authors.first().map(|a| a.trim()).filter(|a| !a.is_empty()) {
        parts.push(Query::clause("author", "any", first));
    }
    if let Some(year) = citation.year {
        parts.push(Query::clause("year", "=", &year.to_string()));
    }
    if let Some(container) = citation.container.as_deref().map(str::trim).filter(|c| !c.is_empty()) {
        parts.push(Query::clause("container", "any", container));
    }
    if parts.is_empty() {
        let text = citation.text.as_deref().map(str::trim).filter(|t| !t.is_empty())?;
        return Some(Query::clause("any", "any", text));
    }
    Some(if parts.len() == 1 {
        parts.pop().expect("one part")
    } else {
        Query::all(parts)
    })
}

#[async_trait::async_trait]
impl SourceService for DefaultSourceService {
    async fn explain(&self, source: Option<String>) -> ExplainResult {
        let registry = self.registry();
        let contact = crate::etiquette::contact();
        match source {
            None => ExplainResult::answer(ExplainAnswer {
                contact,
                sources: registry.adapters().iter().map(|a| a.explain()).collect(),
            }),
            Some(name) => match registry.get(&name) {
                Some(adapter) => ExplainResult::answer(ExplainAnswer {
                    contact,
                    sources: vec![adapter.explain()],
                }),
                None => ExplainResult::refused(
                    "not-found",
                    format!("no source `{name}`; the linked sources are {}", list(&registry.sources())),
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
    ) -> SearchResult {
        if let Err(message) = query.validate() {
            return SearchResult::refused("invalid-argument", message);
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
                    let missing = if let Some(native) = query.native_text() {
                        record.native.is_none().then(|| format!("native:{}", native.chars().take(20).collect::<String>()))
                    } else {
                        query
                            .clauses()
                            .into_iter()
                            .find(|c| !record.covers(c.index, Some(c.relation)))
                            .map(|c| c.index.to_string())
                    };
                    match missing {
                        None => covering.push(adapter),
                        Some(index) => answer.diagnostics.push(Diagnostic::unsupported(&record.source, &index)),
                    }
                }
                covering
            }
        };
        let cursors = match decode_cursors(cursor.as_deref(), &adapters) {
            Ok(c) => c,
            Err(message) => return SearchResult::refused("invalid-argument", message),
        };
        let ran = self.run_search(&query, &adapters, limit, &cursors, sort.as_deref()).await;
        answer.rows = ran.rows;
        answer.next = ran.next;
        answer.diagnostics.extend(ran.diagnostics);
        SearchResult::answer(answer)
    }

    async fn fetch(&self, id: String, sources: Option<Vec<String>>) -> FetchResult {
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
                format!("no linked source claims {canonical}; the sources are {}", list(&self.registry().sources())),
            );
        }
        let (work, diagnostics) = self.run_fetch(&canonical, &adapters).await;
        match work {
            Some(work) => FetchResult::answer(FetchAnswer { work, diagnostics }),
            None => FetchResult::refused(
                "not-found",
                format!(
                    "{canonical}: {}",
                    diagnostics
                        .iter()
                        .map(|d| format!("{} {}{}", d.source, serde_json::to_value(d.status).ok().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default(), d.message.as_deref().map(|m| format!(" ({m})")).unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
        }
    }

    async fn match_citation(&self, citation: MatchInput, sources: Option<Vec<String>>) -> MatchResult {
        let named = match &sources {
            Some(names) => match self.named(names) {
                Ok(a) => Some(a),
                Err(message) => return MatchResult::refused("invalid-argument", message),
            },
            None => None,
        };
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
            let (work, diagnostics) = self.run_fetch(&canonical, &adapters).await;
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
                .filter(|a| {
                    let record = a.explain();
                    query.clauses().iter().all(|c| record.covers(c.index, Some(c.relation)))
                })
                .collect(),
        };
        let ran = self.run_search(&query, &adapters, 5, &BTreeMap::new(), None).await;
        answer.diagnostics.extend(ran.diagnostics);
        answer.candidates.extend(ran.rows.into_iter().map(|work| Candidate {
            work,
            score: None,
            evidence: BTreeMap::new(),
            via: "search".into(),
        }));
        answer.accepted = false;
        MatchResult::answer(answer)
    }

    async fn related(&self, id: String, relation: Relation, limit: Option<u32>, cursor: Option<String>) -> RelatedResult {
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
                format!("no linked source walks relations of {canonical}; the sources are {}", list(&self.registry().sources())),
            );
        }
        let cursors = match decode_cursors(cursor.as_deref(), &adapters) {
            Ok(c) => c,
            Err(message) => return RelatedResult::refused("invalid-argument", message),
        };
        let mut answer = RowsAnswer::default();
        let mut next = BTreeMap::new();
        for adapter in &adapters {
            let source = adapter.explain().source;
            let page = PageRequest {
                limit,
                cursor: cursors.get(&source).cloned(),
                sort: None,
            };
            match adapter.related(&canonical, relation, page).await {
                Ok(page) => {
                    answer.diagnostics.extend(page.diagnostics);
                    answer.diagnostics.push(Diagnostic::done(&source));
                    if let Some(c) = page.next {
                        next.insert(source.clone(), c);
                    }
                    for row in page.rows {
                        merge_into(&mut answer.rows, row);
                    }
                }
                Err(d) => answer.diagnostics.push(d),
            }
        }
        answer.next = encode_cursors(&next);
        RelatedResult::answer(answer)
    }

    async fn export(&self, ids: Vec<String>, format: ExportFormat) -> ExportResult {
        if ids.is_empty() {
            return ExportResult::refused("invalid-argument", "`ids` is empty; name at least one identifier");
        }
        let mut answer = ExportAnswer {
            format,
            ..Default::default()
        };
        // Each id goes to the first adapter that claims it.
        let mut by_adapter: Vec<(Arc<dyn Adapter>, Vec<String>)> = Vec::new();
        for raw in &ids {
            let Some(identifier) = ids::parse(raw) else {
                answer.diagnostics.push(Diagnostic::new(SERVICE_SOURCE, DiagnosticStatus::NotFound).with_message(format!("`{raw}` is not a canonical identifier")));
                continue;
            };
            let canonical = identifier.canonical();
            match self.claiming(&canonical, "export").into_iter().next() {
                Some(adapter) => {
                    let source = adapter.explain().source;
                    match by_adapter.iter_mut().find(|(a, _)| a.explain().source == source) {
                        Some((_, list)) => list.push(canonical),
                        None => by_adapter.push((adapter, vec![canonical])),
                    }
                }
                None => answer.diagnostics.push(Diagnostic::new(SERVICE_SOURCE, DiagnosticStatus::NotFound).with_message(format!("no linked source exports {canonical}"))),
            }
        }
        let mut pieces: Vec<String> = Vec::new();
        let mut csl: Vec<serde_json::Value> = Vec::new();
        for (adapter, list) in &by_adapter {
            let source = adapter.explain().source;
            match adapter.export(list, format).await {
                Ok(text) => {
                    answer.diagnostics.push(Diagnostic::done(&source));
                    match format {
                        ExportFormat::CslJson => match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(serde_json::Value::Array(items)) => {
                                answer.count += items.len() as u32;
                                csl.extend(items);
                            }
                            Ok(one) => {
                                answer.count += 1;
                                csl.push(one);
                            }
                            Err(e) => answer.diagnostics.push(Diagnostic::error(&source, format!("csl-json did not parse: {e}"))),
                        },
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
            ExportFormat::CslJson => serde_json::to_string_pretty(&csl).unwrap_or_else(|_| "[]".into()),
            _ => pieces.join("\n\n"),
        };
        ExportResult::answer(answer)
    }
}

impress_service_impl! {
    service = SourceService,
    safety = read_only,
    since = "0.1.0",
    effects = { reads: [], writes: [], reach: [network] },
    impl = DefaultSourceService,
    instance = DefaultSourceService::new,
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
            sort: Option<String>
        ) -> SearchResult,
        /// One record by a canonical identifier, merged across sources.
        fetch(
            /// A DOI, an arXiv id, `pmid:…`, `bibcode:…`, or a doi.org / arxiv.org URL.
            id: String,
            /// Source ids to ask; absent means every source that claims the id.
            sources: Option<Vec<String>>
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
            /// `cites`, `cited-by`, `similar` or `coreads`.
            relation: Relation,
            /// Rows per source: default 10, at most 100.
            limit: Option<u32>,
            /// The `next` of a previous call, to page.
            cursor: Option<String>
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
