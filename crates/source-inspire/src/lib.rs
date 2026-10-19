//! The INSPIRE-HEP adapter (docs/plan-source-convention.md S-D9): the third
//! source of the second batch, written to show that the next source costs
//! only itself.
//!
//! - [`explain_json`] is the declaration (S-D1). `native: "inspire"` means a
//!   `{"native": "…"}` query is passed to `q` as written (SPIRES / Elasticsearch
//!   syntax: `find a abel and t "first star"`, `refersto:recid:N`).
//! - [`query`] keeps the query tree (`and`/`or`/`not`) and maps each clause
//!   to a SPIRES term (`t`, `a`, `fa`, `date`, `doi`, `eprint`, `j`, `k`,
//!   `refersto:recid:`, `citedby:recid:`).
//! - [`parse`] turns a literature record into `bibliographic-work@1.0.0`.
//! - Paging is `page` + `size` (`size` ≤ 100); the opaque cursor is the next
//!   page number, and INSPIRE's window is the first 10,000 hits (past it the
//!   page is empty with an `unsupported` diagnostic, not an error).
//! - Fetch is by DOI (`/doi/<doi>`), arXiv id (`/arxiv/<id>`) or recid
//!   (`/literature/<recid>`); `export bibtex` is INSPIRE's own
//!   (`?format=bibtex`), and `csl-json` is built from the fetched record;
//!   RIS is `unsupported`.
//! - `related`: `cites` → `citedby:recid:N` (the references), `cited_by` →
//!   `refersto:recid:N`; INSPIRE has no similarity or co-read relation.
//! - Etiquette (S-D8): no key; the documented burst limit is 15 requests per
//!   5 seconds, declared to the limiter; a 429 backs the source off and
//!   answers `rate_limited`, nothing retries.
//!
//! **The recid is an id kind.** INSPIRE's own record number is `ids.inspire`
//! (added to `koine_sources::kind::IDENTIFIER_KINDS` when S7 found it had
//! nowhere to go), and also `provenance.source_id`, `Location.source_id` and
//! the `url` (`https://inspirehep.net/literature/<recid>`). `ids::parse` knows
//! `inspire:`/`recid:` and `detect` claims them.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::{ACCEPT, USER_AGENT};
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::{failed, not_found};

pub mod parse;
pub mod query;

pub const SOURCE: &str = "inspire";
pub const API_VERSION: &str = "literature-v1";
pub const DEFAULT_BASE_URL: &str = "https://inspirehep.net/api";
/// INSPIRE accepts larger sizes (up to 1000); a page here is at most 100.
pub const MAX_ROWS: u32 = 100;
/// INSPIRE serves only the first 10,000 hits of any query.
pub const WINDOW: u32 = 10_000;
/// The documented burst limit: 15 requests per 5 seconds.
pub const RATE: (u32, Duration) = (15, Duration::from_secs(5));
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The fields a request asks for: everything [`parse`] reads and nothing
/// else (the references list of a record is large).
pub const FIELDS: &str = "control_number,titles,authors.full_name,authors.first_name,authors.last_name,authors.ids,earliest_date,preprint_date,publication_info,dois,arxiv_eprints,documents,abstracts,citation_count,document_type,external_system_identifiers,collaborations,languages";

pub const EXPLAIN_JSON: &str = r#"{
  "source": "inspire", "name": "INSPIRE-HEP", "version": "literature-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related", "export"],
  "indexes": {
    "any":          {"relations": ["any"]},
    "title":        {"relations": ["any", "adj"]},
    "author":       {"relations": ["any"]},
    "first_author": {"relations": ["any"]},
    "year":         {"relations": ["=", "<", ">", "within"]},
    "doi":          {"relations": ["="], "exact": true},
    "arxiv":        {"relations": ["="], "exact": true},
    "container":    {"relations": ["any"]},
    "keyword":      {"relations": ["any"]},
    "cites":        {"relations": ["="], "exact": true},
    "cited_by":     {"relations": ["="], "exact": true}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["doi", "arxiv", "inspire"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "abstract", "cited_by", "volume", "issue", "page", "language"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"contact": "user-agent", "limit": "15/5s", "key": "none"},
  "secrets": [],
  "native": "inspire"
}"#;

pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the INSPIRE explain record is valid")
}

pub struct Inspire {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for Inspire {
    fn default() -> Self {
        Self::new()
    }
}

/// Where a canonical id lives in INSPIRE's API.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Doi(String),
    Arxiv(String),
    Recid(String),
}

impl Target {
    fn path(&self) -> String {
        match self {
            Target::Doi(d) => format!("/doi/{}", encode_path(d)),
            Target::Arxiv(a) => format!("/arxiv/{}", encode_path(a)),
            Target::Recid(n) => format!("/literature/{n}"),
        }
    }
}

fn encode_path(p: &str) -> String {
    p.split('/')
        .map(|s| urlencoding::encode(s).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

impl Inspire {
    pub fn new() -> Self {
        let limiter = etiquette::limiter();
        limiter.set_limit(SOURCE, RATE.0, RATE.1);
        Inspire {
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
            "koine-source-inspire/{} (https://github.com/yipihey/koine",
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
        accept: Option<&str>,
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
        tracing::debug!(target: "source.inspire", %url, "GET");
        let mut request = self.client.get(&url).header(USER_AGENT, self.user_agent());
        if let Some(accept) = accept {
            request = request.header(ACCEPT, accept);
        }
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
        let headers = response.headers().clone();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.limiter.back_off(SOURCE, retry_after);
            let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
            tracing::warn!(target: "source.inspire", wait, "429: backing off");
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from inspire; backing off {wait}s, not retrying"),
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

    /// The target of a canonical id this adapter claims.
    fn target_of(id: &str) -> Option<Target> {
        if let Some(d) = id
            .strip_prefix("doi:")
            .filter(|d| d.starts_with("10.") && d.contains('/'))
        {
            return Some(Target::Doi(d.to_string()));
        }
        if let Some(a) = id.strip_prefix("arxiv:").filter(|a| !a.is_empty()) {
            return Some(Target::Arxiv(a.to_string()));
        }
        let n = id
            .strip_prefix("inspire:")
            .or_else(|| id.strip_prefix("recid:"))?;
        (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| Target::Recid(n.into()))
    }

    /// One record's JSON, or `None` on 404.
    async fn record(&self, target: &Target) -> Result<Option<Value>, Diagnostic> {
        let (status, body) = self
            .get(&target.path(), &[("fields".into(), FIELDS.into())], None)
            .await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Self::json(&body).map(Some)
    }

    /// The recid of any claimed id (one request unless it already is one).
    async fn recid_of(&self, id: &str) -> Result<Option<String>, Diagnostic> {
        let Some(target) = Self::target_of(id) else {
            return Ok(None);
        };
        if let Target::Recid(n) = target {
            return Ok(Some(n));
        }
        Ok(self.record(&target).await?.and_then(|v| {
            v.get("metadata")?
                .get("control_number")?
                .as_u64()
                .map(|n| n.to_string())
        }))
    }

    /// `GET /literature?q=…` as a page.
    async fn list(
        &self,
        q: &str,
        page: &PageRequest,
        sort: Option<&str>,
        asked: Option<&Query>,
    ) -> Result<Page, Diagnostic> {
        let size = page.limit.clamp(1, MAX_ROWS);
        let number: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(1);
        let mut out = Page::default();
        if number.saturating_mul(size) > WINDOW {
            out.diagnostics.push(Diagnostic::unsupported(
                SOURCE,
                format!("page {number} of {size}: INSPIRE serves only the first {WINDOW} hits"),
            ));
            return Ok(out);
        }
        let mut params = vec![
            ("q".to_string(), q.to_string()),
            ("size".into(), size.to_string()),
            ("page".into(), number.to_string()),
            ("fields".into(), FIELDS.into()),
        ];
        if let Some(sort) = sort.and_then(query::sort_param) {
            params.push(("sort".into(), sort.into()));
        }
        let (_, body) = self.get("/literature", &params, None).await?;
        let v = Self::json(&body)?;
        let hits = v.get("hits").cloned().unwrap_or_default();
        let total = hits.get("total").and_then(Value::as_u64).unwrap_or(0);
        let list = hits
            .get("hits")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let fetched_at = Self::now();
        for item in &list {
            if let Some(work) = parse::work(item, &fetched_at, asked) {
                out.rows.push(work);
            }
        }
        let seen = u64::from(number) * u64::from(size);
        if list.len() as u32 >= size && seen < total && seen + u64::from(size) <= u64::from(WINDOW)
        {
            out.next = Some((number + 1).to_string());
        }
        Ok(out)
    }
}

#[async_trait]
impl Adapter for Inspire {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::target_of(id).is_some()
    }

    async fn search(&self, q: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let mapped = query::map(q);
        let Some(text) = mapped.q else {
            return Ok(Page {
                diagnostics: mapped.diagnostics,
                ..Default::default()
            });
        };
        let mut out = self
            .list(&text, &page, page.sort.as_deref(), Some(q))
            .await?;
        let mut diagnostics = mapped.diagnostics;
        diagnostics.append(&mut out.diagnostics);
        out.diagnostics = diagnostics;
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(target) = Self::target_of(id) else {
            return Ok(None);
        };
        Ok(self
            .record(&target)
            .await?
            .and_then(|v| parse::work(&v, &Self::now(), None)))
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        if Self::target_of(id).is_none() {
            return Ok(Page::default());
        }
        let walk = match relation {
            Relation::Cites => "citedby",
            Relation::CitedBy => "refersto",
            Relation::Similar | Relation::Coreads => {
                return Err(Diagnostic::unsupported(SOURCE, relation.as_str()))
            }
        };
        let Some(recid) = self.recid_of(id).await? else {
            return Ok(Page {
                diagnostics: vec![not_found(SOURCE, id)],
                ..Default::default()
            });
        };
        self.list(
            &format!("{walk}:recid:{recid}"),
            &page,
            page.sort.as_deref(),
            None,
        )
        .await
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        if format == ExportFormat::Ris {
            return Err(Diagnostic::unsupported(SOURCE, format.as_str()));
        }
        let mut pieces: Vec<String> = Vec::new();
        let mut csl: Vec<Value> = Vec::new();
        for id in ids {
            let Some(target) = Self::target_of(id) else {
                continue;
            };
            if format == ExportFormat::CslJson {
                if let Some(work) = self.fetch(id).await? {
                    csl.push(work.to_csl_json());
                }
                continue;
            }
            let (status, body) = self
                .get(
                    &target.path(),
                    &[("format".into(), "bibtex".into())],
                    Some("application/x-bibtex"),
                )
                .await?;
            if status != StatusCode::NOT_FOUND && !body.trim().is_empty() {
                pieces.push(body.trim().to_string());
            }
        }
        Ok(match format {
            ExportFormat::CslJson => {
                serde_json::to_string_pretty(&csl).unwrap_or_else(|_| "[]".into())
            }
            _ => pieces.join("\n\n"),
        })
    }
}

/// Register the adapter in the process-wide registry; a host calls this
/// once at start. Idempotent.
pub fn register() {
    source_service::registry::register(Arc::new(Inspire::new()));
}

#[cfg(test)]
mod tests;
