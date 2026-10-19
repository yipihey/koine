//! The NASA ADS / SciX adapter (docs/plan-source-convention.md S-D9): the
//! one ADS implementation of the suite, replacing `impress-sources`'
//! `ads.rs` and `scix-client-ffi`'s search wrappers.
//!
//! - [`explain_json`] is the declaration (S-D1): `native: "ads-lucene"` (a
//!   `{native}` query is passed through), the `bib.*` indexes ADS can
//!   answer, `secrets: ["ads-token"]`.
//! - [`query`] compiles the canonical query tree to Lucene.
//! - [`parse`] turns a `doc` into `bibliographic-work@1`.
//! - `related` is ADS's operators — `references(…)`, `citations(…)`,
//!   `similar(…)` and `trending(…)` (what scix-client calls co-reads) —
//!   run as ordinary searches; `export` is `/export/bibtex` and `/ris`
//!   (POST) and, for CSL-JSON, the work itself.
//! - Etiquette (S-D8): the token is the F-D1 secret `ads-token`, read from
//!   the secret slot at the request and never an argument (it is optional
//!   in the explain sense that the *record* lists it; a call without it is
//!   `refused`, not a panic, so it is not declared on a verb). The daily
//!   quota is in `X-RateLimit-Remaining` / `X-RateLimit-Reset` (an epoch):
//!   an exhausted quota backs the source off until the reset, and a 429
//!   backs it off and answers `rate_limited` — nothing retries.
//!
//! Paging is an offset (`start`, `rows`); ADS allows 2000 rows, a page here
//! is at most 100.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use regex::Regex;
use reqwest::header::{HeaderMap, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde_json::{json, Value};
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::failed;
use std::sync::OnceLock;

pub mod parse;
pub mod query;

pub const SOURCE: &str = "ads";
pub const API_VERSION: &str = "v1";
pub const DEFAULT_BASE_URL: &str = "https://api.adsabs.harvard.edu/v1";
/// The F-D1 secret: an ADS / SciX API token.
pub const TOKEN_SECRET: &str = "ads-token";
/// ADS allows 2000 rows; a page here is at most 100.
pub const MAX_ROWS: u32 = 100;
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "ads", "name": "NASA ADS / SciX", "version": "v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related", "export"],
  "indexes": {
    "any":          {"relations": ["any", "all", "adj"]},
    "title":        {"relations": ["any", "all", "adj"]},
    "abstract":     {"relations": ["any", "all", "adj"]},
    "author":       {"relations": ["any", "adj"]},
    "first_author": {"relations": ["any", "adj"]},
    "year":         {"relations": ["=", "<", ">", "within"]},
    "bibcode":      {"relations": ["="], "exact": true},
    "doi":          {"relations": ["="], "exact": true},
    "arxiv":        {"relations": ["="], "exact": true},
    "container":    {"relations": ["any", "="]},
    "keyword":      {"relations": ["any", "adj"]},
    "cites":        {"relations": ["="], "exact": true},
    "cited_by":     {"relations": ["="], "exact": true},
    "similar_to":   {"relations": ["="], "exact": true}
  },
  "sortable": ["relevance", "year", "cited_by"],
  "identifiers": ["bibcode", "doi", "arxiv"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "abstract", "cited_by", "volume", "issue", "page"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"contact": "user-agent", "limit": "headers", "key": "required", "budget": {"per_day": 5000}},
  "secrets": ["ads-token"],
  "native": "ads-lucene"
}"#;

pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the ADS explain record is valid")
}

fn bibcode_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^\d{4}[A-Za-z0-9&.]{14}[A-Z.]$").expect("valid regex"))
}

fn arxiv_re() -> &'static [Regex; 2] {
    static R: OnceLock<[Regex; 2]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(r"^\d{4}\.\d{4,5}(v\d+)?$").expect("valid regex"),
            Regex::new(r"^[a-z-]+(\.[A-Z]{2})?/\d{7}(v\d+)?$").expect("valid regex"),
        ]
    })
}

/// Whether `s` is a 19-character ADS bibcode.
pub fn is_bibcode(s: &str) -> bool {
    bibcode_re().is_match(s)
}

/// Whether `s` is an arXiv id, old or new.
pub fn is_arxiv(s: &str) -> bool {
    arxiv_re().iter().any(|r| r.is_match(s))
}

pub struct Ads {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    /// A token fixed by the caller (tests); `None` reads the secret slot.
    token: Option<String>,
    explain: ExplainRecord,
}

impl Default for Ads {
    fn default() -> Self {
        Self::new()
    }
}

impl Ads {
    pub fn new() -> Self {
        Ads {
            base_url: DEFAULT_BASE_URL.into(),
            client: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .expect("a reqwest client"),
            limiter: etiquette::limiter(),
            contact: None,
            token: None,
            explain: explain_json(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    pub fn with_limiter(mut self, limiter: Arc<Limiter>) -> Self {
        self.limiter = limiter;
        self
    }

    pub fn with_contact(mut self, contact: Option<String>) -> Self {
        self.contact = contact;
        self
    }

    /// Use this token instead of the `ads-token` secret (tests).
    pub fn with_token(mut self, token: Option<String>) -> Self {
        self.token = token;
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
            "koine-source-ads/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// The token, from the override or the secret slot; `refused` when
    /// absent.
    fn bearer(&self) -> Result<String, Diagnostic> {
        let token = match &self.token {
            Some(t) => Some(t.clone()),
            None => {
                impress_service_core::secrets::get(TOKEN_SECRET).map(|s| s.expose().to_string())
            }
        };
        token.filter(|t| !t.is_empty()).ok_or_else(|| {
            Diagnostic::new(
                SOURCE,
                Status::Refused,
                format!(
                    "ADS needs an API token: {}",
                    impress_service_core::secrets::how_to_set(TOKEN_SECRET)
                ),
            )
        })
    }

    /// One request under the etiquette: the bucket, the bearer, the
    /// contact in the User-Agent, the quota headers read back, a 429
    /// backing off. `Ok` is the status and body (404 included); `Err` is a
    /// diagnostic.
    async fn send(
        &self,
        method: Method,
        path: &str,
        params: &[(String, String)],
        body: Option<Value>,
    ) -> Result<(StatusCode, String), Diagnostic> {
        let token = self.bearer()?;
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
        let mut request = self
            .client
            .request(method, &url)
            .header(USER_AGENT, self.user_agent())
            .bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        tracing::debug!(target: "source.ads", path, "request");
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
        self.read_quota_headers(&headers);
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
                format!("429 from ads; backing off {wait}s, not retrying"),
            ));
        }
        let text = response.text().await.map_err(|e| failed(SOURCE, e))?;
        if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
            return Err(Diagnostic::new(
                SOURCE,
                Status::Refused,
                format!(
                    "ADS refused the token (http {}); check the {TOKEN_SECRET} secret",
                    status.as_u16()
                ),
            ));
        }
        if status.is_server_error() || (status.is_client_error() && status != StatusCode::NOT_FOUND)
        {
            let detail = serde_json::from_str::<Value>(&text)
                .ok()
                .and_then(|v| {
                    let error = v.get("error");
                    error
                        .and_then(|e| e.get("msg"))
                        .or(error)
                        .or_else(|| v.get("error-msg"))
                        .or_else(|| v.get("message"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
                .map(|m| format!(": {}", m.chars().take(200).collect::<String>()))
                .unwrap_or_default();
            return Err(failed(
                SOURCE,
                format!("http {} for {path}{detail}", status.as_u16()),
            ));
        }
        Ok((status, text))
    }

    /// An exhausted quota (`X-RateLimit-Remaining: 0`) backs the source off
    /// until `X-RateLimit-Reset`, an epoch in seconds.
    fn read_quota_headers(&self, headers: &HeaderMap) {
        let num = |name: &str| {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
        };
        if num("x-ratelimit-remaining") == Some(0) {
            let now = chrono::Utc::now().timestamp().max(0) as u64;
            let wait = num("x-ratelimit-reset")
                .map(|reset| Duration::from_secs(reset.saturating_sub(now).max(1)));
            self.limiter.back_off(SOURCE, wait);
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// `GET /search/query` for `q`: the docs as works, and `numFound`.
    async fn search_q(
        &self,
        q: &str,
        rows: u32,
        start: u32,
        sort: Option<&str>,
        query: Option<&Query>,
    ) -> Result<(Vec<Work>, u64), Diagnostic> {
        let mut params = vec![
            ("q".to_string(), q.to_string()),
            ("fl".to_string(), parse::FIELDS.to_string()),
            ("rows".to_string(), rows.to_string()),
            ("start".to_string(), start.to_string()),
        ];
        if let Some(sort) = sort {
            params.push(("sort".into(), sort.into()));
        }
        let (_, body) = self
            .send(Method::GET, "/search/query", &params, None)
            .await?;
        let v: Value =
            serde_json::from_str(&body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        let response = v
            .get("response")
            .ok_or_else(|| failed(SOURCE, "no response object"))?;
        let fetched_at = Self::now();
        let works = response
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
            .unwrap_or(0);
        Ok((works, found))
    }

    async fn paged(
        &self,
        q: &str,
        page: &PageRequest,
        query: Option<&Query>,
    ) -> Result<Page, Diagnostic> {
        let rows = page.limit.clamp(1, MAX_ROWS);
        let start: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let sort = page.sort.as_deref().and_then(query::sort_param);
        let (works, found) = self.search_q(q, rows, start, sort, query).await?;
        let seen = u64::from(start) + works.len() as u64;
        Ok(Page {
            next: (!works.is_empty() && seen < found).then(|| seen.to_string()),
            rows: works,
            diagnostics: Vec::new(),
        })
    }

    /// The `q` that selects one record by a canonical id this adapter claims.
    fn id_query(id: &str) -> Option<String> {
        let (kind, value) = id.split_once(':')?;
        match kind {
            "bibcode" if is_bibcode(value) => Some(format!("bibcode:\"{value}\"")),
            "doi" if value.starts_with("10.") && value.contains('/') => {
                Some(format!("doi:\"{value}\""))
            }
            "arxiv" if is_arxiv(value) => Some(format!("identifier:\"arXiv:{value}\"")),
            _ => None,
        }
    }
}

#[async_trait]
impl Adapter for Ads {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::id_query(id).is_some()
    }

    async fn search(&self, q: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let mapped = query::map(q);
        if mapped.q.is_empty() {
            // Every clause was dropped: say so, send nothing.
            return Ok(Page {
                diagnostics: mapped.diagnostics,
                ..Default::default()
            });
        }
        let mut out = self.paged(&mapped.q, &page, Some(q)).await?;
        out.diagnostics = mapped.diagnostics;
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(q) = Self::id_query(id) else {
            return Ok(None);
        };
        let (works, _) = self.search_q(&q, 1, 0, None, None).await?;
        Ok(works.into_iter().next())
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
        let op = match relation {
            Relation::Cites => "references",
            Relation::CitedBy => "citations",
            Relation::Similar => "similar",
            Relation::Coreads => "trending",
        };
        self.paged(&format!("{op}({inner})"), &page, None).await
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        if format == ExportFormat::CslJson {
            let mut csl: Vec<Value> = Vec::new();
            for id in ids {
                if let Some(work) = self.fetch(id).await? {
                    csl.push(work.to_csl_json());
                }
            }
            return Ok(serde_json::to_string_pretty(&csl).unwrap_or_else(|_| "[]".into()));
        }
        // The export endpoints take bibcodes: resolve a DOI or arXiv id first.
        let mut bibcodes: Vec<String> = Vec::new();
        for id in ids {
            if let Some(b) = id.strip_prefix("bibcode:").filter(|b| is_bibcode(b)) {
                bibcodes.push(b.to_string());
            } else if let Some(work) = self.fetch(id).await? {
                if let Some(b) = work.ids.get("bibcode") {
                    bibcodes.push(b.clone());
                }
            }
        }
        if bibcodes.is_empty() {
            return Ok(String::new());
        }
        let endpoint = match format {
            ExportFormat::Bibtex => "/export/bibtex",
            _ => "/export/ris",
        };
        let (_, body) = self
            .send(
                Method::POST,
                endpoint,
                &[],
                Some(json!({"bibcode": bibcodes})),
            )
            .await?;
        let v: Value =
            serde_json::from_str(&body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        v.get("export")
            .and_then(Value::as_str)
            .map(|s| s.trim().to_string())
            .ok_or_else(|| failed(SOURCE, "export answered no `export` text"))
    }
}

pub fn register() {
    source_service::registry::register(Arc::new(Ads::new()));
}

#[cfg(test)]
mod tests;
