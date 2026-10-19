//! The PubMed adapter (docs/plan-source-convention.md S-D9): NCBI's
//! E-utilities behind the source convention.
//!
//! - [`explain_json`] is the declaration (S-D1).
//! - [`query`] maps a canonical query to an esearch `term` in PubMed's own
//!   boolean syntax, field-tagged (`"Abel T"[Author]`, `2020:2024[PDAT]`).
//! - A search is two calls: `esearch.fcgi` (ids, `retmax` ≤ 100, `retstart`
//!   offset paging) then `esummary.fcgi` (the records, in id order).
//!   [`parse`] turns an esummary entry into `bibliographic-work@1.0.0`.
//! - `fetch` by `pmid:` is one esummary; by `doi:` an esearch on `[AID]`
//!   first. `related` `cites`/`cited_by` is `elink.fcgi` with
//!   `pubmed_pubmed_refs` / `pubmed_pubmed_citedin`, then esummary of the
//!   slice asked for.
//! - Etiquette (S-D8): 3 requests per second, 10 with an API key. The
//!   optional key is the F-D1 secret `ncbi-api-key`, read at the request
//!   and never an argument (optional, so not declared on the verb); `tool`
//!   and `email` are sent from the contact the host set, never hardcoded;
//!   `x-ratelimit-limit` refills the bucket; a 429 — or the 200
//!   `{"error": "API rate limit exceeded"}` E-utilities sometimes sends —
//!   backs the source off and answers `rate_limited`, nothing retries.
//!
//! A host links it once at start: [`register`].

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::{HeaderMap, USER_AGENT};
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::failed;

pub mod parse;
pub mod query;

/// The source id: what `sources` and every diagnostic name.
pub const SOURCE: &str = "pubmed";
/// The source API version this adapter was written against (S-D1).
pub const API_VERSION: &str = "eutils-2.0";
/// Where E-utilities live; a test points the adapter at a mock instead.
pub const DEFAULT_BASE_URL: &str = "https://eutils.ncbi.nlm.nih.gov/entrez/eutils";
/// The F-D1 secret of an NCBI account (optional: 10 req/s instead of 3).
pub const API_KEY_SECRET: &str = "ncbi-api-key";
/// The most ids one esearch page asks for.
pub const MAX_ROWS: u32 = 100;
/// esearch will not page past this offset (`retstart` + `retmax`).
pub const MAX_DEPTH: u32 = 9_999;
/// Requests per second without and with a key.
pub const LIMIT_ANONYMOUS: u32 = 3;
pub const LIMIT_KEYED: u32 = 10;
/// How long a request may take before the source is reported `timed_out`.
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "pubmed", "name": "PubMed", "version": "eutils-2.0",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "related"],
  "indexes": {
    "any":          {"relations": ["any"]},
    "title":        {"relations": ["any", "adj"]},
    "abstract":     {"relations": ["any"]},
    "author":       {"relations": ["any"]},
    "first_author": {"relations": ["any"]},
    "year":         {"relations": ["=", "<", ">", "within"]},
    "container":    {"relations": ["any"]},
    "pmid":         {"relations": ["="], "exact": true},
    "doi":          {"relations": ["="], "exact": true},
    "keyword":      {"relations": ["any"]}
  },
  "sortable": ["relevance", "year"],
  "identifiers": ["pmid", "doi"],
  "fills": ["title", "authors", "year", "container", "volume", "issue", "page", "doi", "pmid", "pmcid", "issued", "language", "cited_by"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"contact": "tool+email", "limit": "3/s, 10/s with a key", "key": "optional"},
  "secrets": ["ncbi-api-key"]
}"#;

/// The explain record, parsed and validated.
pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the PubMed explain record is valid")
}

/// The adapter: a client, a base URL, the limiter it goes through and an
/// optional contact override (the host's `etiquette::set_contact` otherwise).
pub struct PubMed {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for PubMed {
    fn default() -> Self {
        Self::new()
    }
}

impl PubMed {
    pub fn new() -> Self {
        PubMed {
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
            "koine-source-pubmed/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// One GET under the etiquette: the bucket (3/s, 10/s with a key), the
    /// `tool`/`email`/`api_key` parameters, the rate header read back, a 429
    /// backing off. `Ok` is the status and body; `Err` is a diagnostic.
    async fn get(
        &self,
        endpoint: &str,
        params: &[(&str, String)],
    ) -> Result<(StatusCode, String), Diagnostic> {
        let key = impress_service_core::secrets::get(API_KEY_SECRET);
        self.smooth(if key.is_some() {
            LIMIT_KEYED
        } else {
            LIMIT_ANONYMOUS
        });
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
        let mut query: Vec<(&str, String)> = params.to_vec();
        query.push(("tool", "koine-source-pubmed".into()));
        if let Some(contact) = self.contact() {
            query.push(("email", contact));
        }
        if let Some(key) = &key {
            query.push(("api_key", key.expose().to_string()));
        }
        let encoded: Vec<String> = query
            .iter()
            .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
            .collect();
        let url = format!("{}/{endpoint}?{}", self.base_url, encoded.join("&"));
        // The key is a query parameter: never log the URL itself.
        tracing::debug!(target: "source.pubmed", endpoint, "GET");
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
            // The error text can carry the URL, and with it the key.
            Err(e) => return Err(failed(SOURCE, e.without_url())),
        };
        let status = response.status();
        let headers = response.headers().clone();
        self.read_rate_headers(&headers);
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(self.back_off(&headers));
        }
        let body = response
            .text()
            .await
            .map_err(|e| failed(SOURCE, e.without_url()))?;
        if status.is_server_error() || status.is_client_error() {
            return Err(failed(
                SOURCE,
                format!("http {} for {endpoint}", status.as_u16()),
            ));
        }
        Ok((status, body))
    }

    fn back_off(&self, headers: &HeaderMap) -> Diagnostic {
        let retry_after = headers
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u64>().ok())
            .map(Duration::from_secs);
        self.limiter.back_off(SOURCE, retry_after);
        let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
        tracing::warn!(target: "source.pubmed", wait, "rate limited: backing off");
        Diagnostic::new(
            SOURCE,
            Status::RateLimited,
            format!("rate limited by pubmed; backing off {wait}s, not retrying"),
        )
    }

    /// NCBI counts requests per second and answers a 4th within one with a
    /// 429, so the bucket holds one token and refills every `1.4 s / n`: `n`
    /// per second with no burst and a margin (checked live 2026-10-19: runs with
    /// a 1.1 s / n spacing were refused intermittently, 1.4 s / n was not).
    fn smooth(&self, per_second: u32) {
        let n = per_second.max(1);
        self.limiter
            .set_limit(SOURCE, 1, Duration::from_millis(1400 / n as u64));
    }

    fn read_rate_headers(&self, headers: &HeaderMap) {
        if let Some(n) = headers
            .get("x-ratelimit-limit")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<u32>().ok())
        {
            self.smooth(n);
        }
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    }

    /// The JSON body, or the diagnostic E-utilities' own `error` field is.
    fn json(&self, body: &str) -> Result<Value, Diagnostic> {
        let v: Value =
            serde_json::from_str(body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        if let Some(error) = v.get("error").and_then(Value::as_str) {
            if error.to_ascii_lowercase().contains("rate limit") {
                return Err(self.back_off(&HeaderMap::new()));
            }
            return Err(failed(SOURCE, format!("eutils: {error}")));
        }
        Ok(v)
    }

    /// The PMID or DOI of a canonical id this adapter claims.
    fn key_of(id: &str) -> Option<(&'static str, &str)> {
        let id = id.trim();
        let (kind, value) = id.split_once(':')?;
        let value = value.trim();
        match kind.to_ascii_lowercase().as_str() {
            "pmid" if !value.is_empty() && value.chars().all(|c| c.is_ascii_digit()) => {
                Some(("pmid", value))
            }
            "doi" if value.starts_with("10.") && value.contains('/') => Some(("doi", value)),
            _ => None,
        }
    }

    /// esearch: the ids of one page, and the total count.
    async fn esearch(
        &self,
        term: &str,
        retmax: u32,
        retstart: u32,
        sort: Option<&str>,
    ) -> Result<(Vec<String>, u32), Diagnostic> {
        let mut params = vec![
            ("db", "pubmed".to_string()),
            ("retmode", "json".to_string()),
            ("term", term.to_string()),
            ("retmax", retmax.to_string()),
            ("retstart", retstart.to_string()),
        ];
        if let Some(sort) = sort {
            params.push(("sort", sort.to_string()));
        }
        let (_, body) = self.get("esearch.fcgi", &params).await?;
        let v = self.json(&body)?;
        let result = v
            .get("esearchresult")
            .ok_or_else(|| failed(SOURCE, "no esearchresult"))?;
        let ids = result
            .get("idlist")
            .and_then(Value::as_array)
            .map(|l| {
                l.iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let count = result
            .get("count")
            .and_then(Value::as_str)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        Ok((ids, count))
    }

    /// esummary of `pmids`, parsed, in the order asked; unknown ids drop out.
    async fn esummary(
        &self,
        pmids: &[String],
        query: Option<&Query>,
    ) -> Result<Vec<Work>, Diagnostic> {
        if pmids.is_empty() {
            return Ok(Vec::new());
        }
        let params = [
            ("db", "pubmed".to_string()),
            ("retmode", "json".to_string()),
            ("id", pmids.join(",")),
        ];
        let (_, body) = self.get("esummary.fcgi", &params).await?;
        let v = self.json(&body)?;
        let result = v.get("result").ok_or_else(|| failed(SOURCE, "no result"))?;
        let fetched_at = Self::now();
        Ok(pmids
            .iter()
            .filter_map(|id| result.get(id))
            .filter_map(|item| parse::work(item, &fetched_at, query))
            .collect())
    }

    /// The PMID a canonical id names: itself, or the DOI's through esearch.
    async fn pmid_of(&self, id: &str) -> Result<Option<String>, Diagnostic> {
        match Self::key_of(id) {
            Some(("pmid", pmid)) => Ok(Some(pmid.to_string())),
            Some(("doi", doi)) => {
                let term = query::map(&Query::clause("doi", "=", doi))
                    .term
                    .unwrap_or_default();
                let (ids, _) = self.esearch(&term, 1, 0, None).await?;
                Ok(ids.into_iter().next())
            }
            _ => Ok(None),
        }
    }
}

#[async_trait]
impl Adapter for PubMed {
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
        let Some(term) = mapped.term else {
            // Every clause was dropped: say so, send nothing.
            return Ok(out);
        };
        let retmax = page.limit.clamp(1, MAX_ROWS);
        let retstart: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        if retstart >= MAX_DEPTH {
            out.diagnostics.push(Diagnostic::new(
                SOURCE,
                Status::Unsupported,
                format!("pubmed does not page past {MAX_DEPTH} results; narrow the query"),
            ));
            return Ok(out);
        }
        let sort = page
            .sort
            .as_deref()
            .and_then(query::sort_param)
            .unwrap_or("relevance");
        let (ids, count) = self.esearch(&term, retmax, retstart, Some(sort)).await?;
        out.rows = self.esummary(&ids, Some(query)).await?;
        let seen = retstart + ids.len() as u32;
        if seen < count.min(MAX_DEPTH) && !ids.is_empty() {
            out.next = Some(seen.to_string());
        }
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        if Self::key_of(id).is_none() {
            return Ok(None);
        }
        let Some(pmid) = self.pmid_of(id).await? else {
            return Ok(None);
        };
        Ok(self.esummary(&[pmid], None).await?.into_iter().next())
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        if Self::key_of(id).is_none() {
            return Ok(Page::default());
        }
        let linkname = match relation {
            Relation::Cites => "pubmed_pubmed_refs",
            Relation::CitedBy => "pubmed_pubmed_citedin",
            other => return Err(Diagnostic::unsupported(SOURCE, other.as_str())),
        };
        let Some(pmid) = self.pmid_of(id).await? else {
            return Ok(Page {
                diagnostics: vec![source_service::service::not_found(SOURCE, id)],
                ..Default::default()
            });
        };
        let params = [
            ("dbfrom", "pubmed".to_string()),
            ("db", "pubmed".to_string()),
            ("linkname", linkname.to_string()),
            ("retmode", "json".to_string()),
            ("id", pmid),
        ];
        let (_, body) = self.get("elink.fcgi", &params).await?;
        let v = self.json(&body)?;
        let links: Vec<String> = v
            .get("linksets")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|set| {
                set.get("linksetdbs")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter(|db| db.get("linkname").and_then(Value::as_str) == Some(linkname))
            .flat_map(|db| {
                db.get("links")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
            })
            .filter_map(|l| l.as_str().map(str::to_string))
            .collect();
        let start: usize = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let end = (start + page.limit.clamp(1, MAX_ROWS) as usize).min(links.len());
        let mut out = Page::default();
        if start < links.len() {
            out.rows = self.esummary(&links[start..end], None).await?;
        }
        if end < links.len() {
            out.next = Some(end.to_string());
        }
        Ok(out)
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        // PubMed's own citation exports are not wired; csl-json is the
        // convention's canonical form and comes from the parsed record.
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
    source_service::registry::register(Arc::new(PubMed::new()));
}

#[cfg(test)]
mod tests;
