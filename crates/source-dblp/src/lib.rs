//! The DBLP adapter (docs/plan-source-convention.md S-D9), the second of the
//! second batch: written to show that the next source costs only itself.
//!
//! - [`explain_json`] is the declaration (S-D1).
//! - [`query`] joins clauses into DBLP's free-text `q` and reports, per
//!   clause, the restriction the public API cannot make (`unsupported`; see
//!   its docs). There is no `dblp` search index: the `bib` index set
//!   (`koine_sources::query::INDEXES`) has none, so a DBLP key is reached
//!   through `fetch dblp:<key>` only (`identifiers` lists `dblp`).
//! - [`parse`] turns a search hit or a `rec/<key>.xml` into
//!   `bibliographic-work@1.0.0`.
//! - Paging: `h` (≤ 100) and `f` (offset); the opaque cursor is the next
//!   offset. No sorting (DBLP ranks); `related` is `unsupported`.
//! - Fetch is by DBLP key through `https://dblp.org/rec/<key>.xml`;
//!   **`export bibtex` is DBLP's own** (`rec/<key>.bib`, with its
//!   `DBLP:<key>` citation key and `bibsource`), which is the source's BibTeX
//!   as it prints it. `csl-json` is built from the fetched record; RIS is
//!   `unsupported`.
//! - Etiquette (S-D8): no key; DBLP asks for at most one request per second
//!   sustained, which the limiter is seeded with; a 429 backs the source off.
//!
//! **Bot check.** On 2026-10-19 dblp.org answered *every* API request, from
//! every client string tried, with HTTP 202 and an Anubis proof-of-work page
//! (`fixtures/bot-challenge.html`) instead of data. This adapter does not
//! solve it: a 202, or any HTML body where JSON/XML/BibTeX was asked for, is
//! an `error` diagnostic that says so. The recorded fixtures for the happy
//! paths are therefore hand-written from DBLP's documented shapes
//! (`fixtures/README.md`), and the live test is `#[ignore]`d.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use koine_sources::{Diagnostic, ExplainRecord, Query, Status, Work};
use reqwest::header::USER_AGENT;
use reqwest::StatusCode;
use serde_json::Value;
use source_service::adapter::{Adapter, ExportFormat, Page, PageRequest, Relation};
use source_service::etiquette::{self, Limiter};
use source_service::service::failed;

pub mod parse;
pub mod query;

pub const SOURCE: &str = "dblp";
pub const API_VERSION: &str = "search-publ-v1";
pub const DEFAULT_BASE_URL: &str = "https://dblp.org";
/// The most hits one request may ask for here (DBLP allows more).
pub const MAX_ROWS: u32 = 100;
/// DBLP's request: one per second, sustained.
pub const RATE: (u32, Duration) = (1, Duration::from_secs(1));
pub const TIMEOUT: Duration = Duration::from_secs(30);

pub const EXPLAIN_JSON: &str = r#"{
  "source": "dblp", "name": "DBLP", "version": "search-publ-v1",
  "kind": "bibliographic-work@1",
  "operations": ["search", "fetch", "match", "export"],
  "indexes": {
    "any":       {"relations": ["any"]},
    "title":     {"relations": ["any", "adj"]},
    "author":    {"relations": ["any"]},
    "container": {"relations": ["any"]},
    "year":      {"relations": ["="]}
  },
  "sortable": [],
  "identifiers": ["dblp"],
  "fills": ["title", "authors", "year", "container", "doi", "issued", "volume", "issue", "page"],
  "page": {"model": "offset", "max": 100},
  "etiquette": {"contact": "user-agent", "limit": "1/s sustained", "key": "none"},
  "secrets": []
}"#;

pub fn explain_json() -> ExplainRecord {
    ExplainRecord::from_json(EXPLAIN_JSON).expect("the DBLP explain record is valid")
}

pub struct Dblp {
    base_url: String,
    client: reqwest::Client,
    limiter: Arc<Limiter>,
    contact: Option<String>,
    explain: ExplainRecord,
}

impl Default for Dblp {
    fn default() -> Self {
        Self::new()
    }
}

/// `conf|journals|books|phd|series/<venue>/<name>…` — DBLP's record keys.
pub fn is_key(key: &str) -> bool {
    let mut parts = key.split('/');
    let Some(first) = parts.next() else {
        return false;
    };
    if !matches!(first, "conf" | "journals" | "books" | "phd" | "series") {
        return false;
    }
    let rest: Vec<&str> = parts.collect();
    rest.len() >= 2
        && rest.iter().all(|p| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '='))
        })
        && rest[0].chars().all(|c| c.is_ascii_alphanumeric())
}

/// A key as pasted: `dblp:` or a `dblp.org/rec/` URL are already stripped by
/// the id parser; this drops a `.html`/`.xml`/`.bib`/`.ris` suffix and a
/// `#fragment`.
pub fn key_of(text: &str) -> Option<String> {
    let t = text.trim();
    let t = t.strip_prefix("dblp:").unwrap_or(t);
    let t = t.split(['#', '?']).next().unwrap_or(t);
    let t = ["html", "xml", "bib", "ris"]
        .iter()
        .find_map(|ext| t.strip_suffix(&format!(".{ext}")))
        .unwrap_or(t);
    is_key(t).then(|| t.to_string())
}

/// Whether a body is the bot-check page instead of data.
fn is_challenge(status: StatusCode, content_type: &str, body: &str) -> bool {
    status == StatusCode::ACCEPTED
        || content_type.contains("text/html")
        || body
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("<!doctype html")
}

impl Dblp {
    pub fn new() -> Self {
        let limiter = etiquette::limiter();
        limiter.set_limit(SOURCE, RATE.0, RATE.1);
        Dblp {
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
            "koine-source-dblp/{} (https://github.com/yipihey/koine",
            env!("CARGO_PKG_VERSION")
        );
        match self.contact() {
            Some(c) => format!("{base}; mailto:{c})"),
            None => format!("{base})"),
        }
    }

    /// One GET under the etiquette. `Ok` is the status and body (404
    /// included); a bot-check page, a 429 and every other failure are `Err`.
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
        tracing::debug!(target: "source.dblp", %url, "GET");
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
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        if status == StatusCode::TOO_MANY_REQUESTS {
            let retry_after = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            self.limiter.back_off(SOURCE, retry_after);
            let wait = retry_after.unwrap_or(Duration::from_secs(10)).as_secs();
            tracing::warn!(target: "source.dblp", wait, "429: backing off");
            return Err(Diagnostic::new(
                SOURCE,
                Status::RateLimited,
                format!("429 from dblp; backing off {wait}s, not retrying"),
            ));
        }
        let body = response.text().await.map_err(|e| failed(SOURCE, e))?;
        // DBLP's own 404 page is HTML too: a miss is not the bot check.
        if status != StatusCode::NOT_FOUND && is_challenge(status, &content_type, &body) {
            return Err(failed(
                SOURCE,
                format!(
                    "dblp.org answered {path} with a bot-check page (HTTP {}), not data; this adapter does not solve it",
                    status.as_u16()
                ),
            ));
        }
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

    fn key_path(key: &str, ext: &str) -> String {
        let encoded: Vec<String> = key
            .split('/')
            .map(|s| urlencoding::encode(s).into_owned())
            .collect();
        format!("/rec/{}.{ext}", encoded.join("/"))
    }

    /// The key of a canonical id this adapter claims.
    fn claimed(id: &str) -> Option<String> {
        id.strip_prefix("dblp:").and_then(key_of)
    }

    /// One record by key, `None` on 404.
    async fn record(&self, key: &str, query: Option<&Query>) -> Result<Option<Work>, Diagnostic> {
        let (status, body) = self.get(&Self::key_path(key, "xml"), &[]).await?;
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let record = parse::record_from_xml(&body)
            .ok_or_else(|| failed(SOURCE, "the record XML held no publication"))?;
        Ok(Some(parse::work(&record, &Self::now(), query)))
    }
}

#[async_trait]
impl Adapter for Dblp {
    fn explain(&self) -> ExplainRecord {
        self.explain.clone()
    }

    fn detect(&self, id: &str) -> bool {
        Self::claimed(id).is_some()
    }

    async fn search(&self, q: &Query, page: PageRequest) -> Result<Page, Diagnostic> {
        let query::Mapped {
            q: text,
            diagnostics,
        } = query::map(q);
        let mut out = Page {
            diagnostics,
            ..Default::default()
        };
        let Some(text) = text else {
            return Ok(out);
        };
        let h = page.limit.clamp(1, MAX_ROWS);
        let first: u32 = page
            .cursor
            .as_deref()
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let params = vec![
            ("q".to_string(), text),
            ("format".into(), "json".into()),
            ("h".into(), h.to_string()),
            ("f".into(), first.to_string()),
        ];
        let (_, body) = self.get("/search/publ/api", &params).await?;
        let v: Value =
            serde_json::from_str(&body).map_err(|e| failed(SOURCE, format!("not JSON: {e}")))?;
        let hits = v
            .get("result")
            .and_then(|r| r.get("hits"))
            .cloned()
            .unwrap_or_default();
        let count = |name: &str| {
            hits.get(name)
                .and_then(Value::as_str)
                .and_then(|n| n.parse::<u32>().ok())
                .unwrap_or(0)
        };
        let (total, sent) = (count("@total"), count("@sent"));
        // One hit is an object, several an array, none no key at all.
        let list: Vec<Value> = match hits.get("hit") {
            Some(Value::Array(a)) => a.clone(),
            Some(one @ Value::Object(_)) => vec![one.clone()],
            _ => Vec::new(),
        };
        let fetched_at = Self::now();
        for hit in &list {
            if let Some(record) = hit.get("info").and_then(parse::record_from_hit) {
                out.rows.push(parse::work(&record, &fetched_at, Some(q)));
            }
        }
        if sent > 0 && first + sent < total {
            out.next = Some((first + sent).to_string());
        }
        Ok(out)
    }

    async fn fetch(&self, id: &str) -> Result<Option<Work>, Diagnostic> {
        let Some(key) = Self::claimed(id) else {
            return Ok(None);
        };
        self.record(&key, None).await
    }

    async fn related(
        &self,
        id: &str,
        relation: Relation,
        _page: PageRequest,
    ) -> Result<Page, Diagnostic> {
        if Self::claimed(id).is_none() {
            return Ok(Page::default());
        }
        Err(Diagnostic::unsupported(SOURCE, relation.as_str()))
    }

    async fn export(&self, ids: &[String], format: ExportFormat) -> Result<String, Diagnostic> {
        if format == ExportFormat::Ris {
            return Err(Diagnostic::unsupported(SOURCE, format.as_str()));
        }
        let mut pieces: Vec<String> = Vec::new();
        let mut csl: Vec<Value> = Vec::new();
        for id in ids {
            let Some(key) = Self::claimed(id) else {
                continue;
            };
            if format == ExportFormat::CslJson {
                if let Some(work) = self.record(&key, None).await? {
                    csl.push(work.to_csl_json());
                }
                continue;
            }
            let (status, body) = self.get(&Self::key_path(&key, "bib"), &[]).await?;
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
    source_service::registry::register(Arc::new(Dblp::new()));
}

#[cfg(test)]
mod tests;
