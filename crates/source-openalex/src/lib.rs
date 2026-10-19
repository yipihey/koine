//! The OpenAlex adapter (docs/plan-source-convention.md S-D9): the REST API
//! at `api.openalex.org/works`, keyless.
//!
//! - [`explain_json`] is the declaration (S-D1).
//! - [`query`] maps a canonical query to `search` and `filter=` (AND only);
//!   a lone `doi =` is a lookup.
//! - [`parse`] turns a `Work` object into `bibliographic-work@1`.
//! - Etiquette (S-D8): the contact the host set goes in `mailto=` (the
//!   polite pool) and the User-Agent, never hardcoded. The bucket is the
//!   documented 10 requests per second; `x-ratelimit-remaining: 0` (the
//!   daily budget) backs the source off until `x-ratelimit-reset`, and a
//!   429 backs it off and answers `rate_limited` — nothing retries. OpenAlex
//!   also sells an optional `api_key`; this adapter does not read one.
//!
//! Paging is a cursor (`cursor=*`, then `meta.next_cursor`), `per-page` ≤ 200
//! on the wire, capped to 100 here. `related` walks `cited_by:W` (the works
//! it cites), `cites:W` (the works that cite it) and `related_to:W`
//! (similar); co-reads are not an OpenAlex notion.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::{HeaderMap, USER_AGENT};
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::{failed, not_found};

pub mod parse;
pub mod query;

pub const SOURCE: &str = "openalex";
pub const API_VERSION: &str = "works-v1";
pub const DEFAULT_BASE_URL: &str = "https://api.openalex.org";
/// OpenAlex allows 200 per page; a page here is at most 100.
pub const MAX_ROWS: u32 = 100;
/// The documented polite rate: 10 requests per second.
pub const RATE: (u32, Duration) = (10, Duration::from_secs(1));
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "openalex", "name": "OpenAlex", "version": "works-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related"],
  "indexes": {
    "any":      {"relations": ["any"]},
    "title":    {"relations": ["any", "adj"]},
    "abstract": {"relations": ["any"]},
    "author":   {"relations": ["any"]},
    "doi":      {"relations": ["="], "exact": true},
    "pmid":     {"relations": ["="], "exact": true},
    "issn":     {"relations": ["="], "exact": true},
    "orcid":    {"relations": ["="], "exact": true},
    "year":     {"relations": ["=", "<", ">", "within"]},
    "cites":    {"relations": ["="], "exact": true},
    "cited_by": {"relations": ["="], "exact": true}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["openalex", "doi", "pmid"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "abstract", "cited_by", "license", "volume", "issue", "page", "language"],
  "page": {"model": "cursor", "max": 100},
  "etiquette": {"contact": "mailto", "limit": "10/s; daily budget in x-ratelimit-* headers"},
  "secrets": []
}"#;

pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the OpenAlex explain record is valid")
}

pub struct OpenAlex {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for OpenAlex {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenAlex {
    pub fn new() -> Self {
        let limiter = etiquette::limiter();
        limiter.set_limit(SOURCE, RATE.0, RATE.1);
        OpenAlex {
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
            "koine-source-openalex/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// One GET under the etiquette. `Ok` is the status and body, 404
    /// included; `Err` is a diagnostic. `path` is already encoded.
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
        let mut query: Vec<(String, String)> = params.to_vec();
        if let Some(contact) = self.contact() {
            query.push(("mailto".into(), contact));
        }
        let mut url = format!("{}{}", self.base_url, path);
        if !query.is_empty() {
            let encoded: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                .collect();
            url.push('?');
            url.push_str(&encoded.join("&"));
        }
        tracing::debug!(target: "source.openalex", %url, "GET");
        let response = match self
            .client
            .get(&url)
            .header(USER_AGENT, self.user_agent())
            .send()
            .await
        {
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
        let headers = response.headers().clone();
        self.read_rate_headers(&headers);
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.limiter.back_off(SOURCE, retry_after);
            let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from openalex; backing off {wait}s, not retrying"),
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

    /// `x-ratelimit-remaining: 0` is the daily budget spent: back off until
    /// `x-ratelimit-reset` (seconds).
    fn read_rate_headers(&self, headers: &HeaderMap) {
        let num = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        if num("x-ratelimit-remaining") == Some(0) {
            self.limiter
                .back_off(SOURCE, num("x-ratelimit-reset").map(Duration::from_secs));
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn json(body: &str) -> Result<Value, Diagnostic> {
        serde_json::from_str(body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))
    }

    /// The path of a canonical id this adapter claims.
    fn path_of(id: &str) -> Option<String> {
        if let Some(w) = id.strip_prefix("openalex:").and_then(query::work_id) {
            return Some(format!("/works/{w}"));
        }
        if let Some(doi) = id
            .strip_prefix("doi:")
            .filter(|d| d.starts_with("10.") && d.contains('/'))
        {
            let encoded: Vec<String> = doi
                .split('/')
                .map(|s| urlencoding::encode(s).into_owned())
                .collect();
            return Some(format!("/works/https://doi.org/{}", encoded.join("/")));
        }
        if let Some(pmid) = id
            .strip_prefix("pmid:")
            .filter(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        {
            return Some(format!("/works/pmid:{pmid}"));
        }
        None
    }

    async fn fetch_json(&self, path: &str) -> Result<Option<Value>, Diagnostic> {
        let (status, body) = self.get(path, &[]).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Self::json(&body).map(Some)
    }

    /// The `W…` id of any id this adapter claims (one request for a DOI or PMID).
    async fn work_id_of(&self, id: &str) -> Result<Option<String>, Diagnostic> {
        if let Some(w) = id.strip_prefix("openalex:").and_then(query::work_id) {
            return Ok(Some(w));
        }
        let Some(path) = Self::path_of(id) else {
            return Ok(None);
        };
        Ok(self.fetch_json(&path).await?.and_then(|v| {
            v.get("id")
                .and_then(Value::as_str)
                .map(|s| parse::short_id(s).to_string())
        }))
    }

    async fn list(
        &self,
        mut params: Vec<(String, String)>,
        page: &PageRequest,
        query: Option<&Query>,
    ) -> Result<Page, Diagnostic> {
        let per = page.limit.clamp(1, MAX_ROWS);
        let has_search = params.iter().any(|(k, _)| k == "search");
        params.push(("per-page".into(), per.to_string()));
        params.push((
            "cursor".into(),
            page.cursor.clone().unwrap_or_else(|| "*".into()),
        ));
        let sort = page
            .sort
            .as_deref()
            .and_then(|s| query::sort_param(s, has_search))
            .unwrap_or(if has_search {
                "relevance_score:desc"
            } else {
                "cited_by_count:desc"
            });
        params.push(("sort".into(), sort.into()));
        let (_, body) = self.get("/works", &params).await?;
        let v = Self::json(&body)?;
        let fetched_at = Self::now();
        let mut out = Page::default();
        let results = v
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for item in &results {
            if let Some(work) = parse::work(item, &fetched_at, query) {
                out.rows.push(work);
            }
        }
        if results.len() as u32 >= per {
            out.next = v
                .get("meta")
                .and_then(|m| m.get("next_cursor"))
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        Ok(out)
    }
}

#[async_trait]
impl Adapter for OpenAlex {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::path_of(id).is_some()
    }

    async fn search(&self, q: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        match query::map(q) {
            query::Mapped::Lookup(doi) => {
                let mut out = Page::default();
                if let Some(path) = Self::path_of(&format!("doi:{doi}")) {
                    if let Some(item) = self.fetch_json(&path).await? {
                        out.rows.extend(parse::work(&item, &Self::now(), Some(q)));
                    }
                }
                Ok(out)
            }
            query::Mapped::List {
                params,
                diagnostics,
            } => {
                if params.is_empty() {
                    return Ok(Page {
                        diagnostics,
                        ..Default::default()
                    });
                }
                let mut out = self.list(params, &page, Some(q)).await?;
                out.diagnostics = diagnostics;
                Ok(out)
            }
        }
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(path) = Self::path_of(id) else {
            return Ok(None);
        };
        Ok(self
            .fetch_json(&path)
            .await?
            .and_then(|v| parse::work(&v, &Self::now(), None)))
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        if Self::path_of(id).is_none() {
            return Ok(Page::default());
        }
        let filter = match relation {
            Relation::Cites => "cited_by",
            Relation::CitedBy => "cites",
            Relation::Similar => "related_to",
            Relation::Coreads => return Err(Diagnostic::unsupported(SOURCE, relation.as_str())),
        };
        let Some(w) = self.work_id_of(id).await? else {
            return Ok(Page {
                diagnostics: vec![not_found(SOURCE, id)],
                ..Default::default()
            });
        };
        self.list(
            vec![("filter".into(), format!("{filter}:{w}"))],
            &page,
            None,
        )
        .await
    }

    async fn export(&self, _ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        Err(Diagnostic::unsupported(SOURCE, format.as_str()))
    }
}

pub fn register() {
    source_service::registry::register(Arc::new(OpenAlex::new()));
}

#[cfg(test)]
mod tests;
