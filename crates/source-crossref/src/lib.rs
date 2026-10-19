//! The Crossref adapter (docs/plan-source-convention.md S-D9): the first
//! source package, and the proof of S-D1–S-D3 and S-D8 on one keyless
//! database.
//!
//! - [`explain_json`] is the declaration (S-D1), parsed and validated into
//!   the [`koine_sources::ExplainRecord`] the registry serves.
//! - [`query`] maps a canonical query to `/works` parameters: `any` →
//!   `query`, `author` → `query.author`, `title`/`container` →
//!   `query.bibliographic` (never the deprecated `query.title`), `year` →
//!   `filter=from-pub-date:…,until-pub-date:…`, a lone `doi =` → `GET
//!   /works/{doi}`.
//! - [`parse`] turns a `work` message into `bibliographic-work@1.0.0`.
//! - Etiquette (S-D8), enforced through `source_service::etiquette`: the
//!   contact the host set goes in `mailto=` and the User-Agent (never
//!   hardcoded here), `X-Rate-Limit-Limit`/`-Interval` refill the token
//!   bucket, a 429 backs the source off and answers `rate_limited` — nothing
//!   retries — and the optional Crossref Plus token is the F-D1 secret
//!   `crossref-plus-token`, read at the request and never an argument. It is
//!   optional, so it is not declared on the verb (a declared secret is a
//!   refusal when absent); the explain record lists it.
//!
//! A host links it once at start: [`register`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::{HeaderMap, ACCEPT, USER_AGENT};
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::{failed, not_found};

pub mod parse;
pub mod query;

/// The source id: what `sources` and every diagnostic name.
pub const SOURCE: &str = "crossref";
/// The source API version this adapter was written against (S-D1).
pub const API_VERSION: &str = "rest-v1";
/// Where Crossref lives; a test points the adapter at a mock instead.
pub const DEFAULT_BASE_URL: &str = "https://api.crossref.org";
/// The F-D1 secret of a Crossref Plus subscriber (optional).
pub const PLUS_TOKEN_SECRET: &str = "crossref-plus-token";
/// The most rows one `/works` request may ask for.
pub const MAX_ROWS: u32 = 1000;
/// How long a request may take before the source is reported `timed_out`.
pub const TIMEOUT: Duration = Duration::from_secs(30);

/// The explain record (S-D1), as the plan writes it, with the operations
/// this adapter actually answers.
pub const EXPLAIN_JSON: &str = r#"{
  "source": "crossref", "name": "Crossref", "version": "rest-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related", "export"],
  "indexes": {
    "any":       {"relations": ["any"]},
    "title":     {"relations": ["any", "adj"]},
    "author":    {"relations": ["any"]},
    "container": {"relations": ["any"]},
    "doi":       {"relations": ["="], "exact": true},
    "issn":      {"relations": ["="], "exact": true},
    "orcid":     {"relations": ["="], "exact": true},
    "year":      {"relations": ["=", "<", ">", "within"]}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["doi"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "cited_by", "license"],
  "page": {"model": "cursor", "max": 1000},
  "etiquette": {"contact": "mailto", "limit": "headers", "key": "optional"},
  "secrets": ["crossref-plus-token"]
}"#;

/// The explain record, parsed and validated.
pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the Crossref explain record is valid")
}

/// The adapter: a client, a base URL, the limiter it goes through and an
/// optional contact override (the host's `etiquette::set_contact` otherwise).
pub struct Crossref {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for Crossref {
    fn default() -> Self {
        Self::new()
    }
}

impl Crossref {
    pub fn new() -> Self {
        Crossref {
            base_url: DEFAULT_BASE_URL.into(),
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("a reqwest client"),
            limiter: etiquette::limiter(),
            contact: None,
            explain: explain_json(),
        }
    }

    /// Point at another base URL (a mock server).
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Use a limiter of this adapter's own instead of the process-wide one.
    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        self.limiter = limiter;
        self
    }

    /// Override the contact (tests); `None` means the host's.
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
            "koine-source-crossref/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// One GET under the etiquette: the bucket, the contact, the optional
    /// Plus token, the rate headers read back, a 429 backing off. `Ok` is
    /// the status and body, 404 included; `Err` is a diagnostic.
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
        let mut query: Vec<(String, String)> = params.to_vec();
        if let Some(contact) = self.contact() {
            query.push(("mailto".into(), contact));
        }
        if !query.is_empty() {
            let encoded: Vec<String> = query
                .iter()
                .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
                .collect();
            url.push('?');
            url.push_str(&encoded.join("&"));
        }
        let mut request = self.client.get(&url).header(USER_AGENT, self.user_agent());
        if let Some(accept) = accept {
            request = request.header(ACCEPT, accept);
        }
        if let Some(token) = impress_service_core::secrets::get(PLUS_TOKEN_SECRET) {
            request = request.header(
                "Crossref-Plus-API-Token",
                format!("Bearer {}", token.expose()),
            );
        }
        tracing::debug!(target: "source.crossref", %url, "GET");
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
        self.read_rate_headers(&headers);
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = headers
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.limiter.back_off(SOURCE, retry_after);
            let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
            tracing::warn!(target: "source.crossref", wait, "429: backing off");
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from crossref; backing off {wait}s, not retrying"),
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

    fn read_rate_headers(&self, headers: &HeaderMap) {
        let limit = headers
            .get("x-rate-limit-limit")
            .and_then(|v| v.to_str().ok());
        let interval = headers
            .get("x-rate-limit-interval")
            .and_then(|v| v.to_str().ok());
        self.limiter.update_from_headers(SOURCE, limit, interval);
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    fn message(body: &str) -> Result<Value, Diagnostic> {
        let v: Value =
            serde_json::from_str(body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        if v.get("status").and_then(Value::as_str) != Some("ok") {
            return Err(failed(
                SOURCE,
                format!("status {}", v.get("status").cloned().unwrap_or_default()),
            ));
        }
        v.get("message")
            .cloned()
            .ok_or_else(|| failed(SOURCE, "no message"))
    }

    /// The DOI of a canonical id this adapter claims.
    fn doi_of(id: &str) -> Option<&str> {
        id.strip_prefix("doi:")
            .filter(|d| d.starts_with("10.") && d.contains('/'))
    }

    fn works_path(doi: &str) -> String {
        format!("/works/{}", urlencoding::encode(doi))
    }

    /// `GET /works/{doi}` as a parsed work; `None` on 404.
    async fn fetch_message(&self, doi: &str) -> Result<Option<Value>, Diagnostic> {
        let (status, body) = self.get(&Self::works_path(doi), &[], None).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        Self::message(&body).map(Some)
    }
}

#[async_trait]
impl Adapter for Crossref {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::doi_of(id).is_some()
    }

    async fn search(&self, query: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let mut out = Page::default();
        let (params, diagnostics) = match query::map(query) {
            query::Mapped::Lookup(doi) => {
                if let Some(message) = self.fetch_message(&doi).await? {
                    if let Some(work) = parse::work(&message, &Self::now(), Some(query)) {
                        out.rows.push(work);
                    }
                }
                return Ok(out);
            }
            query::Mapped::List {
                params,
                diagnostics,
            } => (params, diagnostics),
        };
        out.diagnostics = diagnostics;
        if params.is_empty() {
            // Every clause was dropped: say so, send nothing.
            return Ok(out);
        }
        let mut params = params;
        let rows = page.limit.clamp(1, MAX_ROWS);
        params.push(("rows".into(), rows.to_string()));
        params.push((
            "cursor".into(),
            page.cursor.clone().unwrap_or_else(|| "*".into()),
        ));
        // Always explicit: with `cursor=*` Crossref drops relevance ordering
        // unless `sort=relevance&order=desc` is sent (checked live
        // 2026-10-19: the same query answered `score` 12.8 first with the
        // cursor alone and 30.1 first with the sort; fixtures/README.md).
        let sort = page
            .sort
            .as_deref()
            .and_then(query::sort_param)
            .unwrap_or("relevance");
        params.push(("sort".into(), sort.into()));
        params.push(("order".into(), "desc".into()));
        let (_, body) = self.get("/works", &params, None).await?;
        let message = Self::message(&body)?;
        let fetched_at = Self::now();
        let items = message
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let got = items.len() as u32;
        for item in &items {
            if let Some(work) = parse::work(item, &fetched_at, Some(query)) {
                out.rows.push(work);
            }
        }
        // Crossref always sends a next-cursor; it is only worth following
        // while a page comes back full.
        if got >= rows {
            out.next = message
                .get("next-cursor")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(doi) = Self::doi_of(id) else {
            return Ok(None);
        };
        Ok(self
            .fetch_message(doi)
            .await?
            .and_then(|m| parse::work(&m, &Self::now(), None)))
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        let Some(doi) = Self::doi_of(id) else {
            return Ok(Page::default());
        };
        if relation != Relation::Cites {
            return Err(Diagnostic::unsupported(SOURCE, relation.as_str()));
        }
        let Some(message) = self.fetch_message(doi).await? else {
            return Ok(Page {
                diagnostics: vec![not_found(SOURCE, id)],
                ..Default::default()
            });
        };
        let fetched_at = Self::now();
        let refs: Vec<Work> = message
            .get("reference")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|r| parse::reference(r, &fetched_at))
                    .collect()
            })
            .unwrap_or_default();
        let start: usize = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let end = (start + page.limit.max(1) as usize).min(refs.len());
        let mut out = Page::default();
        if start < refs.len() {
            out.rows = refs[start..end].to_vec();
        }
        if end < refs.len() {
            out.next = Some(end.to_string());
        }
        Ok(out)
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        let mut pieces: Vec<String> = Vec::new();
        let mut csl: Vec<Value> = Vec::new();
        for id in ids {
            let Some(doi) = Self::doi_of(id) else {
                continue;
            };
            match format {
                ExportFormat::CslJson => {
                    if let Some(work) = self.fetch(id).await? {
                        csl.push(work.to_csl_json());
                    }
                }
                ExportFormat::Bibtex | ExportFormat::Ris => {
                    let accept = if format == ExportFormat::Bibtex {
                        "application/x-bibtex"
                    } else {
                        "application/x-research-info-systems"
                    };
                    let path = format!("{}/transform", Self::works_path(doi));
                    let (status, body) = self.get(&path, &[], Some(accept)).await?;
                    if status != StatusCode::NOT_FOUND && !body.trim().is_empty() {
                        pieces.push(body.trim().to_string());
                    }
                }
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
    source_service::registry::register(Arc::new(Crossref::new()));
}

#[cfg(test)]
mod tests;
