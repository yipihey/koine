//! The Web of Science adapter (docs/plan-source-convention.md S-D9): the
//! Clarivate **Starter API** behind the source convention.
//!
//! - [`explain_json`] is the declaration (S-D1).
//! - [`query`] maps a canonical query to the `q` parameter in WoS field-tag
//!   syntax (`AU=(Abel T)`, `PY=2020-2024`).
//! - Search is `GET /documents?q=…&db=WOS&limit=…&page=…&sortField=…`
//!   (`limit` ≤ 50, 1-based `page`; the cursor is the next page number);
//!   `fetch` by DOI is a one-row search on `DO=`, by UT it is
//!   `GET /documents/{uid}`. [`parse`] turns a document into
//!   `bibliographic-work@1.0.0`.
//! - The key is the F-D1 secret `wos-api-key`, **required**: with none the
//!   adapter answers the `refused` diagnostic (S-D7) and sends nothing; a
//!   401/403 is `refused` too. It goes in the `X-ApiKey` header and is never
//!   logged or put in an error.
//! - Etiquette (S-D8): the bucket is seeded at 5 req/s (the Swift's figure,
//!   which the tier of the key may change) and refilled from
//!   `X-RateLimit-Limit-Second`; a 429 backs the source off and answers
//!   `rate_limited`, nothing retries.
//!
//! No Starter-API key was reachable when this was written, so the fixtures
//! are hand-written from the documented response shape (fixtures/README.md)
//! and the live test is `#[ignore]`.
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
use source_service::service::failed;

pub mod parse;
pub mod query;

/// The source id: what `sources` and every diagnostic name.
pub const SOURCE: &str = "wos";
/// The source API version this adapter was written against (S-D1).
pub const API_VERSION: &str = "wos-starter-v1";
/// Where the Starter API lives; a test points the adapter at a mock instead.
pub const DEFAULT_BASE_URL: &str = "https://api.clarivate.com/apis/wos-starter/v1";
/// The F-D1 secret: a Clarivate developer-portal key for the Starter API.
pub const API_KEY_SECRET: &str = "wos-api-key";
/// The most documents one request may ask for.
pub const MAX_ROWS: u32 = 50;
/// The seed of the bucket before `X-RateLimit-Limit-Second` says otherwise.
pub const SEED_PER_SECOND: u32 = 5;
/// How long a request may take before the source is reported `timed_out`.
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "wos", "name": "Web of Science", "version": "wos-starter-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match"],
  "indexes": {
    "any":       {"relations": ["any"]},
    "title":     {"relations": ["any", "adj"]},
    "author":    {"relations": ["any"]},
    "year":      {"relations": ["=", "<", ">", "within"]},
    "doi":       {"relations": ["="], "exact": true},
    "container": {"relations": ["any"]},
    "keyword":   {"relations": ["any"]}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["wos", "doi"],
  "fills": ["title", "authors", "year", "container", "volume", "issue", "page", "doi", "pmid", "wos", "issued", "cited_by"],
  "page": {"model": "offset", "max": 50},
  "etiquette": {"limit": "headers", "key": "required"},
  "secrets": ["wos-api-key"]
}"#;

/// The explain record, parsed and validated.
pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the Web of Science explain record is valid")
}

/// The adapter: a client, a base URL and the limiter it goes through.
pub struct Wos {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for Wos {
    fn default() -> Self {
        Self::new()
    }
}

impl Wos {
    pub fn new() -> Self {
        Wos {
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
            "koine-source-wos/{} (https://github.com/yipihey/koine",
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

    /// What a missing or rejected key answers (S-D7).
    fn refused(why: &str) -> Diagnostic {
        Diagnostic::new(
            SOURCE,
            Status::Refused,
            format!(
                "{why}: set the secret `{API_KEY_SECRET}` (KOINE_SECRET_WOS_API_KEY, or \
                 --secrets-dir / --keychain); a Starter API key is free at \
                 https://developer.clarivate.com/apis/wos-starter"
            ),
        )
    }

    /// One GET under the etiquette: the key (refused when absent), the
    /// bucket, the rate headers read back, a 429 backing off. `Ok` is the
    /// status and body, 404 included; `Err` is a diagnostic.
    async fn get(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> Result<(StatusCode, String), Diagnostic> {
        let Some(key) = impress_service_core::secrets::get(API_KEY_SECRET) else {
            return Err(Self::refused("no Web of Science API key"));
        };
        self.limiter
            .set_limit(SOURCE, SEED_PER_SECOND, Duration::from_secs(1));
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
        tracing::debug!(target: "source.wos", %url, "GET");
        let response = match self
            .client
            .get(&url)
            .header(USER_AGENT, self.user_agent())
            .header(ACCEPT, "application/json")
            .header("X-ApiKey", key.expose())
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
            tracing::warn!(target: "source.wos", wait, "429: backing off");
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from wos; backing off {wait}s, not retrying"),
            ));
        }
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Self::refused(&format!(
                "Web of Science rejected the key (http {})",
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

    /// The gateway's per-second limit refills the bucket
    /// (`X-RateLimit-Limit-Second`); the remaining counters are logged so a
    /// drained quota is visible in the console before the 429.
    fn read_rate_headers(&self, headers: &HeaderMap) {
        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
        self.limiter
            .update_from_headers(SOURCE, get("x-ratelimit-limit-second"), Some("1s"));
        if let Some(left) = get("x-ratelimit-remaining-year") {
            tracing::debug!(target: "source.wos", left, "yearly quota remaining");
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// The UT or DOI of a canonical id this adapter claims.
    fn key_of(id: &str) -> Option<(&'static str, String)> {
        let id = id.trim();
        let (kind, value) = id.split_once(':')?;
        let value = value.trim();
        match kind.to_ascii_lowercase().as_str() {
            "wos" => {
                let ut = if value.len() > 4 && value[..4].eq_ignore_ascii_case("WOS:") {
                    &value[4..]
                } else {
                    value
                };
                (ut.len() == 15 && ut.chars().all(|c| c.is_ascii_alphanumeric()))
                    .then(|| ("wos", format!("WOS:{ut}")))
            }
            "doi" if value.starts_with("10.") && value.contains('/') => {
                Some(("doi", value.to_string()))
            }
            _ => None,
        }
    }

    /// `GET /documents?…`: the parsed rows and the total.
    async fn documents(
        &self,
        q: &str,
        limit: u32,
        page: u32,
        sort: Option<&str>,
        query: Option<&Query>,
    ) -> Result<(Vec<Work>, u32), Diagnostic> {
        let mut params = vec![
            ("q", q.to_string()),
            ("db", "WOS".to_string()),
            ("limit", limit.to_string()),
            ("page", page.to_string()),
        ];
        if let Some(sort) = sort {
            params.push(("sortField", sort.to_string()));
        }
        let (_, body) = self.get("/documents", &params).await?;
        let v: Value =
            serde_json::from_str(&body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        let fetched_at = Self::now();
        let rows = v
            .get("hits")
            .and_then(Value::as_array)
            .map(|hits| {
                hits.iter()
                    .filter_map(|h| parse::work(h, &fetched_at, query))
                    .collect()
            })
            .unwrap_or_default();
        let total = v
            .get("metadata")
            .and_then(|m| m.get("total"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        Ok((rows, total))
    }
}

#[async_trait]
impl Adapter for Wos {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::key_of(id).is_some()
    }

    async fn search(&self, query: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let mut out = Page::default();
        let mapped = query::map(query);
        out.diagnostics = mapped.diagnostics;
        let Some(q) = mapped.q else {
            // Every clause was dropped: say so, send nothing.
            return Ok(out);
        };
        let limit = page.limit.clamp(1, MAX_ROWS);
        let number: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .filter(|n| *n >= 1)
            .unwrap_or(1);
        let sort = page
            .sort
            .as_deref()
            .and_then(query::sort_param)
            .unwrap_or("RS+D");
        let (rows, total) = self
            .documents(&q, limit, number, Some(sort), Some(query))
            .await?;
        let got = rows.len() as u32;
        out.rows = rows;
        if got > 0 && (number - 1) * limit + got < total {
            out.next = Some((number + 1).to_string());
        }
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        match Self::key_of(id) {
            Some(("wos", ut)) => {
                let path = format!("/documents/{}", urlencoding::encode(&ut));
                let (status, body) = self.get(&path, &[]).await?;
                if status == StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                let v: Value = serde_json::from_str(&body)
                    .map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
                Ok(parse::work(&v, &Self::now(), None))
            }
            Some(("doi", doi)) => {
                let q = query::map(&Query::clause("doi", "=", doi))
                    .q
                    .unwrap_or_default();
                let (rows, _) = self.documents(&q, 1, 1, None, None).await?;
                Ok(rows.into_iter().next())
            }
            _ => Ok(None),
        }
    }

    async fn related(
        &self,
        _id: &str,
        relation: Relation,
        _page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        // The Starter API has no citing/cited/related-by-id endpoint (the
        // Swift's `/citing`, `/references`, `/related` are the Expanded API's).
        Err(Diagnostic::unsupported(SOURCE, relation.as_str()))
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
    source_service::registry::register(Arc::new(Wos::new()));
}

#[cfg(test)]
mod tests;
