//! The SciX adapter (docs/plan-source-convention.md S-D9): SciX's
//! ADS-compatible API behind the source convention.
//!
//! SciX serves the ADS API at its own host (`https://api.scixplorer.org/v1`
//! answers the same 401 `Missing "Authorization"` as
//! `api.adsabs.harvard.edu`, checked 2026-10-19) with the same bearer-token
//! model. imbib's Swift `SciXSource` actually dials the ADS host with a SciX
//! token and only rewrites the links; this adapter dials SciX itself, so
//! the `scix` source id names the service it asks. The query and response
//! shape (`q`, `fl`, `rows`, `start`, `sort`; `response.docs[]`) are ADS's,
//! so a shared ADS-family base with the `source-ads` crate is a follow-up
//! once both land; this crate stands alone on purpose.
//!
//! - [`query`] maps a canonical query to ADS Lucene; the explain record's
//!   `native: "ads-lucene"` lets a `{native}` clause through unchanged.
//! - Search is `GET /search/query` with `rows` ≤ 100 and `start` offset
//!   paging (the cursor is the next `start`); `fetch` is a one-row search on
//!   `bibcode:`/`doi:`/`identifier:"arXiv:…"`; `related` is the ADS operators
//!   `references()`, `citations()`, `similar()` and `trending()` (coreads).
//!   [`parse`] turns a doc into `bibliographic-work@1.0.0`.
//! - The token is the F-D1 secret `scix-token`, **required**: with none the
//!   adapter answers the `refused` diagnostic (S-D7) and sends nothing; a
//!   401/403 is `refused` too. It goes in `Authorization: Bearer` only.
//! - Etiquette (S-D8): ADS-family quotas are per day; `X-RateLimit-Limit`
//!   refills the bucket (limit per 86400 s), `X-RateLimit-Remaining: 0` backs
//!   the source off until `X-RateLimit-Reset`; a 429 backs off and answers
//!   `rate_limited`, nothing retries.
//!
//! No SciX token was reachable when this was written, so the fixtures are
//! hand-written from the documented ADS response shape, except the one real
//! 401 (fixtures/README.md); the live test is `#[ignore]`.
//!
//! A host links it once at start: [`register`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION, USER_AGENT};
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::failed;

pub mod parse;
pub mod query;

/// The source id: what `sources` and every diagnostic name.
pub const SOURCE: &str = "scix";
/// The source API version this adapter was written against (S-D1).
pub const API_VERSION: &str = "ads-v1";
/// Where SciX's API lives; a test points the adapter at a mock instead.
pub const DEFAULT_BASE_URL: &str = "https://api.scixplorer.org/v1";
/// A paper's page at SciX.
pub const ABS_URL: &str = "https://scixplorer.org/abs";
/// SciX's link gateway: `{GATEWAY_URL}/{bibcode}/{PUB_PDF|EPRINT_HTML|…}`.
pub const GATEWAY_URL: &str = "https://scixplorer.org/link_gateway";
/// The F-D1 secret: a SciX API token (scixplorer.org → user settings → token).
pub const TOKEN_SECRET: &str = "scix-token";
/// The most rows one request may ask for.
pub const MAX_ROWS: u32 = 100;
/// How long a request may take before the source is reported `timed_out`.
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "scix", "name": "SciX", "version": "ads-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related"],
  "indexes": {
    "any":          {"relations": ["any"]},
    "title":        {"relations": ["any", "adj"]},
    "abstract":     {"relations": ["any"]},
    "author":       {"relations": ["any"]},
    "first_author": {"relations": ["any"]},
    "year":         {"relations": ["=", "<", ">", "within"]},
    "bibcode":      {"relations": ["="], "exact": true},
    "doi":          {"relations": ["="], "exact": true},
    "arxiv":        {"relations": ["="], "exact": true},
    "container":    {"relations": ["any"]},
    "keyword":      {"relations": ["any"]}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["bibcode", "doi", "arxiv"],
  "fills": ["title", "authors", "year", "container", "volume", "issue", "page", "abstract", "doi", "bibcode", "arxiv", "issued", "cited_by", "language"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"limit": "headers", "key": "required", "budget": {"per_day": 5000}},
  "secrets": ["scix-token"],
  "native": "ads-lucene"
}"#;

/// `2002Sci...295...93A`: a year, 14 of `[A-Za-z0-9.&]`, a last letter or dot.
pub fn is_bibcode(s: &str) -> bool {
    s.len() == 19
        && s[..4].chars().all(|c| c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '&')
        && s.ends_with(|c: char| c.is_ascii_alphabetic() || c == '.')
}

/// The explain record, parsed and validated.
pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the SciX explain record is valid")
}

/// The adapter: a client, a base URL and the limiter it goes through.
pub struct Scix {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for Scix {
    fn default() -> Self {
        Self::new()
    }
}

impl Scix {
    pub fn new() -> Self {
        Scix {
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

    fn user_agent(&self) -> String {
        let base = format!(
            "koine-source-scix/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        let contact = self
            .contact
            .clone()
            .map(|c| c.strip_prefix("mailto:").unwrap_or(&c).to_string())
            .or_else(etiquette::contact_address);
        match contact {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// What a missing or rejected token answers (S-D7).
    fn refused(why: &str) -> Diagnostic {
        Diagnostic::new(
            SOURCE,
            Status::Refused,
            format!(
                "{why}: set the secret `{TOKEN_SECRET}` (KOINE_SECRET_SCIX_TOKEN, or \
                 --secrets-dir / --keychain); a token is free at \
                 https://scixplorer.org/user/settings/token"
            ),
        )
    }

    /// One GET under the etiquette. `Ok` is the status and body, 404
    /// included; `Err` is a diagnostic.
    async fn get(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<(StatusCode, String), Diagnostic> {
        let Some(token) = impress_service_core::secrets::get(TOKEN_SECRET) else {
            return Err(Self::refused("no SciX token"));
        };
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
        let encoded: Vec<String> = params
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect();
        let mut url = format!("{}{}", self.base_url, path);
        if !encoded.is_empty() {
            url.push('?');
            url.push_str(&encoded.join("&"));
        }
        tracing::debug!(target: "source.scix", %url, "GET");
        let response = match self
            .client
            .get(&url)
            .header(USER_AGENT, self.user_agent())
            .header(ACCEPT, "application/json")
            .header(AUTHORIZATION, format!("Bearer {}", token.expose()))
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
            Err(e) => return Err(failed(SOURCE, e.without_url())),
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
            tracing::warn!(target: "source.scix", wait, "429: backing off");
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from scix; backing off {wait}s, not retrying"),
            ));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Self::refused(&format!(
                "SciX rejected the token (http {})",
                status.as_u16()
            )));
        }
        let body = response
            .text()
            .await
            .map_err(|e| failed(SOURCE, e.without_url()))?;
        if status.is_server_error() || (status.is_client_error() && status != StatusCode::NOT_FOUND)
        {
            return Err(failed(
                SOURCE,
                format!("http {} for {path}", status.as_u16()),
            ));
        }
        Ok((status, body))
    }

    /// `X-RateLimit-Limit` is a daily budget (ADS-family: 5000 a day by
    /// default); it refills the bucket per 86400 s. `Remaining: 0` backs the
    /// source off until `Reset` (epoch seconds), so the 429 is never earned.
    fn read_rate_headers(&self, headers: &HeaderMap) {
        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        self.limiter
            .update_from_headers(SOURCE, get("x-ratelimit-limit"), Some("86400s"));
        let remaining = get("x-ratelimit-remaining").and_then(|v| v.trim().parse::<u64>().ok());
        if remaining == Some(0) {
            let wait = get("x-ratelimit-reset")
                .and_then(|v| v.trim().parse::<i64>().ok())
                .map(|at| (at - chrono::Utc::now().timestamp()).max(1) as u64)
                .unwrap_or(60);
            self.limiter
                .back_off(SOURCE, Some(Duration::from_secs(wait)));
            tracing::warn!(target: "source.scix", wait, "daily quota spent: backing off");
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// The ADS query that names `id`, and whether it is claimed.
    fn id_query(id: &str) -> Option<String> {
        let (kind, value) = id.trim().split_once(':')?;
        let value = value.trim();
        let kind = kind.to_ascii_lowercase();
        match kind.as_str() {
            "bibcode" if is_bibcode(value) => query::map(&Query::clause("bibcode", "=", value)).q,
            "doi" if value.starts_with("10.") && value.contains('/') => {
                query::map(&Query::clause("doi", "=", value)).q
            }
            "arxiv" if !value.is_empty() => query::map(&Query::clause("arxiv", "=", value)).q,
            _ => None,
        }
    }

    /// `GET /search/query`: the parsed rows and `numFound`.
    async fn docs(
        &self,
        q: &str,
        rows: u32,
        start: u32,
        sort: Option<&str>,
        query: Option<&Query>,
    ) -> Result<(Vec<Work>, u32), Diagnostic> {
        let mut params = vec![
            ("q", q.to_string()),
            ("fl", parse::FIELDS.to_string()),
            ("rows", rows.to_string()),
            ("start", start.to_string()),
        ];
        if let Some(sort) = sort {
            params.push(("sort", sort.to_string()));
        }
        let (_, body) = self.get("/search/query", &params).await?;
        let v: Value =
            serde_json::from_str(&body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        if let Some(error) = v
            .get("error")
            .and_then(|e| e.get("msg"))
            .and_then(Value::as_str)
        {
            return Err(failed(SOURCE, format!("scix: {error}")));
        }
        let response = v
            .get("response")
            .ok_or_else(|| failed(SOURCE, "no response"))?;
        let fetched_at = Self::now();
        let rows_out = response
            .get("docs")
            .and_then(Value::as_array)
            .map(|docs| {
                docs.iter()
                    .filter_map(|d| parse::work(d, &fetched_at, query))
                    .collect()
            })
            .unwrap_or_default();
        let found = response
            .get("numFound")
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        Ok((rows_out, found))
    }

    async fn paged(
        &self,
        q: &str,
        page: &PageRequest,
        query: Option<&Query>,
        diagnostics: Vec<Diagnostic>,
    ) -> Result<Page, Diagnostic> {
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
            .unwrap_or("score desc");
        let (got, found) = self.docs(q, rows, start, Some(sort), query).await?;
        let seen = start + got.len() as u32;
        Ok(Page {
            next: (!got.is_empty() && seen < found).then(|| seen.to_string()),
            rows: got,
            diagnostics,
        })
    }
}

#[async_trait]
impl Adapter for Scix {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::id_query(id).is_some()
    }

    async fn search(&self, query: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let mapped = query::map(query);
        let Some(q) = mapped.q else {
            // Every clause was dropped: say so, send nothing.
            return Ok(Page {
                diagnostics: mapped.diagnostics,
                ..Default::default()
            });
        };
        self.paged(&q, &page, Some(query), mapped.diagnostics).await
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(q) = Self::id_query(id) else {
            return Ok(None);
        };
        let (rows, _) = self.docs(&q, 1, 0, None, None).await?;
        Ok(rows.into_iter().next())
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        let Some(inner) = Self::id_query(id) else {
            return Ok(Page::default());
        };
        let operator = match relation {
            Relation::Cites => "references",
            Relation::CitedBy => "citations",
            Relation::Similar => "similar",
            Relation::Coreads => "trending",
        };
        self.paged(&format!("{operator}({inner})"), &page, None, Vec::new())
            .await
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
    source_service::registry::register(Arc::new(Scix::new()));
}

#[cfg(test)]
mod tests;
