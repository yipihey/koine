//! The arXiv adapter (docs/plan-source-convention.md S-D9): the Atom API at
//! `export.arxiv.org/api/query`, keyless.
//!
//! - [`explain_json`] is the declaration (S-D1).
//! - [`query`] compiles a canonical query to `search_query` (tree kept) and
//!   an `id_list`; a lone `arxiv =` is a lookup by `id_list`.
//! - [`parse`] turns the feed into `bibliographic-work@1`.
//! - Etiquette (S-D8): arXiv's terms of use ask for no more than one request
//!   every three seconds. The adapter seeds the shared limiter with exactly
//!   that (1 token per 3 s) at construction, so a second request waits for
//!   its turn (up to the limiter's bound, else `rate_limited`), and a 429
//!   backs the source off. The contact the host set goes in the
//!   User-Agent; arXiv has no `mailto` parameter. No secrets.
//!
//! Paging is an offset (`start`, `max_results`); the cursor is the next
//! offset, and it ends when `opensearch:totalResults` is reached.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::USER_AGENT;
use reqwest::StatusCode;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::failed;

pub mod parse;
pub mod query;

/// The source id.
pub const SOURCE: &str = "arxiv";
/// The API version this adapter was written against.
pub const API_VERSION: &str = "atom-v1";
/// Where arXiv lives; a test points the adapter at a mock.
pub const DEFAULT_BASE_URL: &str = "https://export.arxiv.org";
/// The most rows one page asks for (the API allows more; 100 is the practical limit).
pub const MAX_ROWS: u32 = 100;
/// arXiv's terms: one request per this long.
pub const MIN_INTERVAL: Duration = Duration::from_secs(3);
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "arxiv", "name": "arXiv", "version": "atom-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match"],
  "indexes": {
    "any":      {"relations": ["any", "all", "adj"]},
    "title":    {"relations": ["any", "all", "adj"]},
    "abstract": {"relations": ["any", "all", "adj"]},
    "author":   {"relations": ["any", "all", "adj"]},
    "subject":  {"relations": ["="], "exact": true},
    "arxiv":    {"relations": ["="], "exact": true},
    "year":     {"relations": ["=", "<", ">", "within"]}
  },
  "sortable": ["relevance", "year"],
  "identifiers": ["arxiv"],
  "fills": ["title", "authors", "year", "abstract", "doi", "issued"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"contact": "user-agent", "limit": "1 per 3s"},
  "secrets": []
}"#;

pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the arXiv explain record is valid")
}

pub struct Arxiv {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for Arxiv {
    fn default() -> Self {
        Self::new()
    }
}

impl Arxiv {
    pub fn new() -> Self {
        let limiter = etiquette::limiter();
        limiter.set_limit(SOURCE, 1, MIN_INTERVAL);
        Arxiv {
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

    /// Use a limiter of this adapter's own; it is seeded with arXiv's one
    /// request per three seconds.
    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        limiter.set_limit(SOURCE, 1, MIN_INTERVAL);
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
            "koine-source-arxiv/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// One GET of `/api/query` under the etiquette: the bucket (one per
    /// three seconds), the contact in the User-Agent, a 429 backing off.
    async fn get(&self, params: &[(&str, String)]) -> Result<String, Diagnostic> {
        if let Err(left) = self.limiter.acquire(SOURCE).await {
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!(
                    "arXiv allows one request per 3s; waited for the turn and backed off for another {}s, nothing was sent",
                    left.as_secs().max(1)
                ),
            ));
        }
        let encoded: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{k}={}", urlencoding::encode(v)))
            .collect();
        let url = format!("{}/api/query?{}", self.base_url, encoded.join("&"));
        tracing::debug!(target: "source.arxiv", %url, "GET");
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
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.limiter.back_off(SOURCE, retry_after);
            let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from arxiv; backing off {wait}s, not retrying"),
            ));
        }
        let body = response.text().await.map_err(|e| failed(SOURCE, e))?;
        if !status.is_success() {
            return Err(failed(SOURCE, format!("http {}", status.as_u16())));
        }
        Ok(body)
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn parse_feed(body: &str) -> Result<parse::Feed, Diagnostic> {
        let feed = parse::feed(body).map_err(|e| failed(SOURCE, e))?;
        if let Some(message) = feed.entries.iter().find_map(parse::error_of) {
            return Err(failed(
                SOURCE,
                format!("arxiv refused the request: {message}"),
            ));
        }
        Ok(feed)
    }

    fn arxiv_id_of(id: &str) -> Option<&str> {
        id.strip_prefix("arxiv:").filter(|v| parse::is_arxiv_id(v))
    }
}

#[async_trait]
impl Adapter for Arxiv {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::arxiv_id_of(id).is_some()
    }

    async fn search(&self, q: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let mut out = Page::default();
        let mapped = query::map(q);
        out.diagnostics = mapped.diagnostics;
        if mapped.search_query.is_empty() && mapped.id_list.is_empty() {
            // Every clause was dropped: say so, send nothing.
            return Ok(out);
        }
        let rows = page.limit.clamp(1, MAX_ROWS);
        let start: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let sort = page
            .sort
            .as_deref()
            .and_then(query::sort_param)
            .unwrap_or("relevance");
        let mut params: Vec<(&str, String)> = Vec::new();
        if !mapped.search_query.is_empty() {
            params.push(("search_query", mapped.search_query));
        }
        if !mapped.id_list.is_empty() {
            params.push(("id_list", mapped.id_list.join(",")));
        }
        params.push(("start", start.to_string()));
        params.push(("max_results", rows.to_string()));
        params.push(("sortBy", sort.into()));
        params.push(("sortOrder", "descending".into()));
        let body = self.get(&params).await?;
        let feed = Self::parse_feed(&body)?;
        let fetched_at = Self::now();
        let got = feed.entries.len() as u32;
        for entry in &feed.entries {
            if let Some(work) = parse::work(entry, &fetched_at, Some(q)) {
                out.rows.push(work);
            }
        }
        let seen = u64::from(start) + u64::from(got);
        if got > 0 && feed.total.is_some_and(|t| seen < t) {
            out.next = Some(seen.to_string());
        }
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(arxiv) = Self::arxiv_id_of(id) else {
            return Ok(None);
        };
        let body = self
            .get(&[
                ("id_list", arxiv.to_string()),
                ("max_results", "1".to_string()),
            ])
            .await?;
        let feed = Self::parse_feed(&body)?;
        let fetched_at = Self::now();
        Ok(feed
            .entries
            .first()
            .and_then(|e| parse::work(e, &fetched_at, None)))
    }

    async fn related(
        &self,
        _id: &str,
        relation: Relation,
        _page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        Err(Diagnostic::unsupported(SOURCE, relation.as_str()))
    }

    async fn export(&self, _ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        Err(Diagnostic::unsupported(SOURCE, format.as_str()))
    }
}

/// Register the adapter in the process-wide registry; idempotent.
pub fn register() {
    source_service::registry::register(Arc::new(Arxiv::new()));
}

#[cfg(test)]
mod tests;
