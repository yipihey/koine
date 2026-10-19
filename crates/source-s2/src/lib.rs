//! The Semantic Scholar adapter (docs/plan-source-convention.md S-D9), the
//! first of the second batch: written to show that the next source costs only
//! itself.
//!
//! - [`explain_json`] is the declaration (S-D1).
//! - [`query`] maps a canonical query to `/paper/search` (`query` = the
//!   space-joined `any`/`title` terms, `year` = a range), an identifier
//!   lookup (`/paper/DOI:…`) or a relation walk.
//! - [`parse`] turns a paper object into `bibliographic-work@1.0.0`.
//! - `related`: `cites` → `/paper/{id}/references`, `cited_by` →
//!   `/paper/{id}/citations`, `similar` → `/recommendations/v1/papers/forpaper`
//!   (answers keyless, but only from a pool: the adapter sends `from=all-cs`,
//!   so a non-computer-science paper gets few or no recommendations; the
//!   default pool returned an empty list for the Transformer paper on
//!   2026-10-19). Coreads is `unsupported`. The recommendations API wants a
//!   paper id, so any other id costs one extra lookup.
//! - Paging is `offset`/`limit` (≤ 100); S2 serves only the first 1,000
//!   relevance-ranked search hits, past that the page is empty with an
//!   `unsupported` diagnostic.
//! - Etiquette (S-D8): the optional F-D1 secret `s2-api-key` goes out as
//!   `x-api-key`, read at the request and never an argument; it is optional,
//!   so it is not declared on the verb. Unauthenticated callers share one
//!   pool, documented at 1 request per second, and that pool is routinely
//!   over its limit (about nine of ten keyless calls during the recording of
//!   the fixtures answered 429): the limiter is seeded at 1 per second, a 429
//!   backs the source off and answers `rate_limited`, nothing retries. No
//!   daily figure is documented, so no `budget`.
//! - Export: S2 has no citation-format endpoint; `csl-json` is built from the
//!   fetched record, BibTeX and RIS are `unsupported`.
//!
//! **What the convention lacks here.** `corpusId` is S2's stable numeric id
//! and is not a canonical id kind, so it routes in (`corpusid:N`) but cannot
//! come back on a row (`Work::validate` refuses `ids.corpusid`).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::USER_AGENT;
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::ids;
use source_service::service::{failed, not_found};

pub mod parse;
pub mod query;

pub const SOURCE: &str = "s2";
pub const API_VERSION: &str = "graph-v1";
pub const DEFAULT_BASE_URL: &str = "https://api.semanticscholar.org";
/// The optional F-D1 secret: a Semantic Scholar API key.
pub const API_KEY_SECRET: &str = "s2-api-key";
/// The most rows one request may ask for.
pub const MAX_ROWS: u32 = 100;
/// `/paper/search` serves only the first 1,000 hits.
pub const SEARCH_WINDOW: u32 = 1000;
/// The documented keyless rate: one request per second.
pub const RATE: (u32, Duration) = (1, Duration::from_secs(1));
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// Everything [`parse`] reads off a paper (search, lookup).
pub const FIELDS: &str = "paperId,corpusId,externalIds,title,abstract,venue,publicationVenue,year,publicationDate,citationCount,isOpenAccess,openAccessPdf,publicationTypes,journal,authors";
/// The same without the abstract, for lists of related papers.
pub const LIST_FIELDS: &str = "paperId,corpusId,externalIds,title,year,publicationDate,citationCount,authors,venue,isOpenAccess,openAccessPdf,publicationTypes,journal";

pub const EXPLAIN_JSON: &str = r#"{
  "source": "s2", "name": "Semantic Scholar", "version": "graph-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related", "export"],
  "indexes": {
    "any":        {"relations": ["any"]},
    "title":      {"relations": ["any", "adj"]},
    "year":       {"relations": ["=", "<", ">", "within"]},
    "doi":        {"relations": ["="], "exact": true},
    "arxiv":      {"relations": ["="], "exact": true},
    "pmid":       {"relations": ["="], "exact": true},
    "cites":      {"relations": ["="], "exact": true},
    "cited_by":   {"relations": ["="], "exact": true},
    "similar_to": {"relations": ["="], "exact": true}
  },
  "sortable": ["relevance"],
  "identifiers": ["doi", "arxiv", "pmid", "pmcid", "s2"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "abstract", "cited_by", "volume", "issue", "page"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"contact": "user-agent", "limit": "1/s (a shared keyless pool that often answers 429)", "key": "optional"},
  "secrets": ["s2-api-key"]
}"#;

pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the Semantic Scholar explain record is valid")
}

pub struct SemanticScholar {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for SemanticScholar {
    fn default() -> Self {
        Self::new()
    }
}

/// A canonical id this adapter claims, as `kind:value` text → its canonical
/// form, or `None`. `query` uses it for the relation clauses.
pub(crate) fn canonical_of(text: &str) -> Option<String> {
    let id = ids::parse(text)?;
    query::s2_id(&id.kind, &id.value).map(|_| id.canonical())
}

/// An S2 id (`DOI:10.1016/s1384-1076(01)00065-3`) as a path segment: S2 reads
/// the colon and the slashes of a DOI literally.
fn encode_id(raw: &str) -> String {
    urlencoding::encode(raw)
        .replace("%3A", ":")
        .replace("%2F", "/")
}

impl SemanticScholar {
    pub fn new() -> Self {
        let limiter = etiquette::limiter();
        limiter.set_limit(SOURCE, RATE.0, RATE.1);
        SemanticScholar {
            base_url: DEFAULT_BASE_URL.into(),
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("a reqwest client"),
            limiter,
            contact: None,
            explain: explain_json(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        limiter.set_limit(SOURCE, RATE.0, RATE.1);
        self.limiter = limiter;
        self
    }

    pub fn with_contact(mut self, contact: Option<String>) -> Self {
        self.contact = contact;
        self
    }

    pub fn limiter(&self) -> &Limiter {
        &self.limiter
    }

    fn contact(&self) -> Option<String> {
        self.contact
            .clone()
            .map(|c| c.strip_prefix("mailto:").unwrap_or(&c).to_string())
            .or_else(etiquette::contact_address)
    }

    fn user_agent(&self) -> String {
        let base = format!(
            "koine-source-s2/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// One GET under the etiquette: the bucket, the optional key, a 429
    /// backing off. `Ok` is the status and body, 404 included.
    async fn get(
        &self,
        path: &str,
        params: &[(String, String)],
    ) -> Result<(StatusCode, String), Diagnostic> {
        if let Err(left) = self.limiter.acquire(SOURCE).await {
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!(
                    "backed off for another {}s; nothing was sent",
                    left.as_secs().max(1)
                ),
            ));
        }
        let mut url = format!("{}{}", self.base_url, path);
        if !params.is_empty() {
            let encoded: Vec<String> = params
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                .collect();
            url.push('?');
            url.push_str(&encoded.join("&"));
        }
        let mut request = self.client.get(&url).header(USER_AGENT, self.user_agent());
        if let Some(key) = impress_service_core::secrets::get(API_KEY_SECRET) {
            request = request.header("x-api-key", key.expose());
        }
        tracing::debug!(target: "source.s2", %url, "GET");
        let response = match request.send().await {
            Ok(r) => r,
            Err(e) if e.is_timeout() => {
                return Err(Diagnostic::new(
                    SOURCE,
                    Status::TimedOut,
                    format!("no answer within {}s", TIMEOUT.as_secs()),
                ))
            }
            Err(e) => return Err(failed(SOURCE, e)),
        };
        let status = response.status();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.limiter.back_off(SOURCE, retry_after);
            let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
            tracing::warn!(target: "source.s2", wait, "429: backing off");
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from s2; backing off {wait}s, not retrying (a key in `s2-api-key` has a pool of its own)"),
            ));
        }
        let body = response.text().await.map_err(|e| failed(SOURCE, e))?;
        if status.is_server_error() || (status.is_client_error() && status != StatusCode::NOT_FOUND)
        {
            return Err(failed(
                SOURCE,
                format!("http {} for {path}", status.as_u16()),
            ));
        }
        Ok((status, body))
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn json(body: &str) -> Result<Value, Diagnostic> {
        serde_json::from_str(body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))
    }

    /// The S2 path segment of a canonical id this adapter claims.
    fn segment_of(id: &str) -> Option<String> {
        let (kind, value) = id.split_once(':')?;
        query::s2_id(kind, value).map(|s| encode_id(&s))
    }

    /// `GET /paper/{id}` as JSON, `None` on 404.
    async fn paper(&self, segment: &str, fields: &str) -> Result<Option<Value>, Diagnostic> {
        let (status, body) = self
            .get(
                &format!("/graph/v1/paper/{segment}"),
                &[("fields".into(), fields.into())],
            )
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Self::json(&body).map(Some)
    }

    async fn walk(
        &self,
        relation: query::Walk,
        id: &str,
        page: &PageRequest,
    ) -> Result<Page, Diagnostic> {
        let Some(segment) = Self::segment_of(id) else {
            return Ok(Page::default());
        };
        let limit = page.limit.clamp(1, MAX_ROWS);
        let offset: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let fetched_at = Self::now();
        let mut out = Page::default();
        match relation {
            query::Walk::Similar => {
                // The recommendations API takes a paper id, not DOI:/ARXIV:.
                let paper_id = match query::is_paper_id(&segment) {
                    true => segment.clone(),
                    false => match self.paper(&segment, "paperId").await? {
                        Some(v) => match v.get("paperId").and_then(Value::as_str) {
                            Some(p) => p.to_string(),
                            None => return Ok(out),
                        },
                        None => {
                            out.diagnostics.push(not_found(SOURCE, id));
                            return Ok(out);
                        }
                    },
                };
                let (status, body) = self
                    .get(
                        &format!("/recommendations/v1/papers/forpaper/{paper_id}"),
                        &[
                            ("fields".into(), LIST_FIELDS.into()),
                            ("limit".into(), limit.to_string()),
                            ("from".into(), "all-cs".into()),
                        ],
                    )
                    .await?;
                if status == StatusCode::NOT_FOUND {
                    out.diagnostics.push(not_found(SOURCE, id));
                    return Ok(out);
                }
                let v = Self::json(&body)?;
                for item in v
                    .get("recommendedPapers")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    out.rows.extend(parse::work(item, &fetched_at, None));
                }
                // The recommendations API does not page.
                Ok(out)
            }
            walk => {
                let (edge, key) = match walk {
                    query::Walk::Citations => ("citations", "citingPaper"),
                    _ => ("references", "citedPaper"),
                };
                let (status, body) = self
                    .get(
                        &format!("/graph/v1/paper/{segment}/{edge}"),
                        &[
                            ("fields".into(), LIST_FIELDS.into()),
                            ("offset".into(), offset.to_string()),
                            ("limit".into(), limit.to_string()),
                        ],
                    )
                    .await?;
                if status == StatusCode::NOT_FOUND {
                    out.diagnostics.push(not_found(SOURCE, id));
                    return Ok(out);
                }
                let v = Self::json(&body)?;
                for item in v
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(work) = item
                        .get(key)
                        .and_then(|p| parse::work(p, &fetched_at, None))
                    {
                        out.rows.push(work);
                    }
                }
                out.next = v.get("next").and_then(Value::as_u64).map(|n| n.to_string());
                Ok(out)
            }
        }
    }
}

#[async_trait]
impl Adapter for SemanticScholar {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::segment_of(id).is_some()
    }

    async fn search(&self, q: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        match query::map(q) {
            query::Mapped::Lookup { id, diagnostics } => {
                let mut out = Page {
                    diagnostics,
                    ..Default::default()
                };
                if let Some(v) = self.paper(&encode_id(&id), FIELDS).await? {
                    out.rows.extend(parse::work(&v, &Self::now(), Some(q)));
                }
                Ok(out)
            }
            query::Mapped::Walk {
                relation,
                id,
                diagnostics,
            } => {
                let mut out = self.walk(relation, &id, &page).await?;
                let mut all = diagnostics;
                all.append(&mut out.diagnostics);
                out.diagnostics = all;
                Ok(out)
            }
            query::Mapped::Search {
                text,
                year,
                diagnostics,
            } => {
                let mut out = Page {
                    diagnostics,
                    ..Default::default()
                };
                if text.trim().is_empty() {
                    // Every clause was dropped (or there was only a year,
                    // which /paper/search cannot answer alone): say so.
                    if year.is_some() {
                        out.diagnostics.push(Diagnostic::unsupported(
                            SOURCE,
                            "bib.year alone: /paper/search needs words to search for",
                        ));
                    }
                    return Ok(out);
                }
                let limit = page.limit.clamp(1, MAX_ROWS);
                let offset: u32 = page
                    .cursor
                    .as_deref()
                    .and_then(|c| c.parse().ok())
                    .unwrap_or(0);
                if offset.saturating_add(limit) > SEARCH_WINDOW {
                    out.diagnostics.push(Diagnostic::unsupported(
                        SOURCE,
                        format!(
                            "offset {offset} + {limit}: Semantic Scholar serves only the first {SEARCH_WINDOW} search hits"
                        ),
                    ));
                    return Ok(out);
                }
                let mut params = vec![
                    ("query".to_string(), text),
                    ("fields".into(), FIELDS.into()),
                    ("offset".into(), offset.to_string()),
                    ("limit".into(), limit.to_string()),
                ];
                if let Some(y) = year {
                    params.push(("year".into(), y));
                }
                let (_, body) = self.get("/graph/v1/paper/search", &params).await?;
                let v = Self::json(&body)?;
                let fetched_at = Self::now();
                for item in v
                    .get("data")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    out.rows.extend(parse::work(item, &fetched_at, Some(q)));
                }
                // `next` is absent on the last page.
                out.next = v
                    .get("next")
                    .and_then(Value::as_u64)
                    .filter(|n| *n < u64::from(SEARCH_WINDOW))
                    .map(|n| n.to_string());
                Ok(out)
            }
        }
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(segment) = Self::segment_of(id) else {
            return Ok(None);
        };
        Ok(self
            .paper(&segment, FIELDS)
            .await?
            .and_then(|v| parse::work(&v, &Self::now(), None)))
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        if Self::segment_of(id).is_none() {
            return Ok(Page::default());
        }
        let walk = match relation {
            Relation::Cites => query::Walk::References,
            Relation::CitedBy => query::Walk::Citations,
            Relation::Similar => query::Walk::Similar,
            Relation::Coreads => return Err(Diagnostic::unsupported(SOURCE, relation.as_str())),
        };
        self.walk(walk, id, &page).await
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        if format != ExportFormat::CslJson {
            return Err(Diagnostic::unsupported(SOURCE, format.as_str()));
        }
        let mut csl: Vec<Value> = Vec::new();
        for id in ids {
            if let Some(work) = self.fetch(id).await? {
                csl.push(work.to_csl_json());
            }
        }
        Ok(serde_json::to_string_pretty(&csl).unwrap_or_else(|_| "[]".into()))
    }
}

/// Register the adapter in the process-wide registry; a host calls this
/// once at start. Idempotent.
pub fn register() {
    source_service::registry::register(Arc::new(SemanticScholar::new()));
}

#[cfg(test)]
mod tests;
