//! Etiquette, enforced once (S-D8): the contact string set by the host and
//! never per file, a per-source token bucket seeded from the declaration
//! and refilled from response headers, and a per-source **daily budget**
//! counted in-process. A 429 backs the source off; while it is backed off
//! every call answers `rate-limited` at once and nothing is retried.
//!
//! Two layers share one state. The adapters take the per-second tokens
//! ([`Limiter::acquire`]) around their own HTTP calls and record the 429s
//! and the quota headers they see. The service asks the [`Gate`] *before*
//! dispatching to an adapter — [`Gate::admit`] — which refuses a backed-off
//! source and a spent daily budget with `rate_limited` and "nothing was
//! sent", counts the request against the budget, and warns once per source
//! when a polite-pool source is asked with no contact set. A stub adapter in
//! a test goes through the same gate as Crossref does.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

use koine_sources::explain::Budget;
use koine_sources::{Diagnostic, ExplainRecord, Status};

/// The environment variable a host or a bare CLI sets the contact in.
pub const CONTACT_ENV: &str = "KOINE_SOURCE_CONTACT";

static CONTACT: RwLock<Option<String>> = RwLock::new(None);

/// Set the contact the adapters send (`mailto:` address or an email) — the
/// host's identity, once.
pub fn set_contact(contact: impl Into<String>) {
    let value = contact.into();
    let mut slot = CONTACT.write().unwrap_or_else(|e| e.into_inner());
    *slot = if value.trim().is_empty() {
        None
    } else {
        Some(value.trim().to_string())
    };
}

/// The contact: what [`set_contact`] installed, else [`CONTACT_ENV`], else
/// none (the adapter then calls without one and says so in its diagnostics).
pub fn contact() -> Option<String> {
    if let Some(c) = CONTACT.read().unwrap_or_else(|e| e.into_inner()).clone() {
        return Some(c);
    }
    std::env::var(CONTACT_ENV)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The contact as a bare address: `mailto:a@b` and `a@b` both give `a@b`.
pub fn contact_address() -> Option<String> {
    contact().map(|c| c.strip_prefix("mailto:").unwrap_or(&c).to_string())
}

/// One source's bucket.
#[derive(Debug, Clone)]
struct Bucket {
    /// Requests allowed per `interval`.
    limit: u32,
    interval: Duration,
    tokens: f64,
    refilled_at: Instant,
    /// Set by a 429: no request until then.
    backoff_until: Option<Instant>,
}

impl Bucket {
    fn new(limit: u32, interval: Duration) -> Self {
        Bucket {
            limit,
            interval,
            tokens: limit as f64,
            refilled_at: Instant::now(),
            backoff_until: None,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.refilled_at);
        let rate = self.limit as f64 / self.interval.as_secs_f64().max(1e-3);
        self.tokens = (self.tokens + elapsed.as_secs_f64() * rate).min(self.limit as f64);
        self.refilled_at = now;
    }
}

/// What [`Limiter::check`] says about a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// Go.
    Now,
    /// Wait this long, then go.
    After(Duration),
    /// The source is backed off for this long; answer `rate-limited`.
    BackedOff(Duration),
}

/// The per-source token buckets.
#[derive(Default)]
pub struct Limiter {
    buckets: Mutex<BTreeMap<String, Bucket>>,
}

/// The default a source gets before its headers say otherwise: Crossref's
/// documented polite-pool default is 50 per second; we start below it.
pub const DEFAULT_LIMIT: u32 = 10;
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

/// The longest a caller waits for a token before giving up with
/// `rate-limited` instead of queueing — at least this, and for a slow
/// source three of its intervals (`max_wait`): arXiv's one-per-3-s bucket
/// made the third of a burst of feed refreshes give up at 5 s and answer
/// no rows, where the old limiter would have queued it (imbib S6b).
pub const MAX_WAIT: Duration = Duration::from_secs(5);

impl Limiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed or reset `source`'s bucket from a declaration or a header pair.
    pub fn set_limit(&self, source: &str, limit: u32, interval: Duration) {
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let limit = limit.max(1);
        let entry = map
            .entry(source.to_string())
            .or_insert_with(|| Bucket::new(limit, interval));
        if entry.limit != limit || entry.interval != interval {
            let backoff = entry.backoff_until;
            *entry = Bucket::new(limit, interval);
            entry.backoff_until = backoff;
        }
    }

    /// How long `acquire` will queue for `source`: [`MAX_WAIT`], or three of
    /// the source's declared intervals when that is longer.
    pub fn max_wait(&self, source: &str) -> Duration {
        self.limit_of(source)
            .map(|(_, interval)| interval * 3)
            .unwrap_or(Duration::ZERO)
            .max(MAX_WAIT)
    }

    /// The limit a source currently has, `(limit, interval)`.
    pub fn limit_of(&self, source: &str) -> Option<(u32, Duration)> {
        let map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        map.get(source).map(|b| (b.limit, b.interval))
    }

    /// Refill from `X-Rate-Limit-Limit` / `X-Rate-Limit-Interval` style
    /// values (`"50"`, `"1s"`); a pair that does not parse is ignored.
    pub fn update_from_headers(&self, source: &str, limit: Option<&str>, interval: Option<&str>) {
        let Some(limit) = limit.and_then(|l| l.trim().parse::<u32>().ok()) else {
            return;
        };
        let interval = interval
            .and_then(parse_interval)
            .unwrap_or(DEFAULT_INTERVAL);
        self.set_limit(source, limit, interval);
    }

    /// Record a 429: no request to `source` for `retry_after` (default 10 s).
    pub fn back_off(&self, source: &str, retry_after: Option<Duration>) {
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let entry = map
            .entry(source.to_string())
            .or_insert_with(|| Bucket::new(DEFAULT_LIMIT, DEFAULT_INTERVAL));
        entry.backoff_until = Some(Instant::now() + retry_after.unwrap_or(Duration::from_secs(10)));
    }

    /// How long until `source` may be asked again; zero when it may be asked now.
    pub fn backoff_remaining(&self, source: &str) -> Duration {
        let map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        map.get(source)
            .and_then(|b| b.backoff_until)
            .map(|until| until.saturating_duration_since(Instant::now()))
            .unwrap_or_default()
    }

    /// Take a token for `source` if one is available now; else say how long
    /// to wait. Never blocks.
    pub fn check(&self, source: &str) -> Admission {
        let mut map = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let bucket = map
            .entry(source.to_string())
            .or_insert_with(|| Bucket::new(DEFAULT_LIMIT, DEFAULT_INTERVAL));
        let now = Instant::now();
        if let Some(until) = bucket.backoff_until {
            if until > now {
                return Admission::BackedOff(until - now);
            }
            bucket.backoff_until = None;
        }
        bucket.refill(now);
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Admission::Now
        } else {
            let rate = bucket.limit as f64 / bucket.interval.as_secs_f64().max(1e-3);
            let wait = (1.0 - bucket.tokens) / rate;
            Admission::After(Duration::from_secs_f64(wait.max(0.001)))
        }
    }

    /// Wait for a token (bounded by [`Limiter::max_wait`]). `Err` carries how
    /// long the source is still backed off, or the bound when the bucket is
    /// empty for longer than we will queue.
    pub async fn acquire(&self, source: &str) -> Result<(), Duration> {
        let started = Instant::now();
        let bound = self.max_wait(source);
        loop {
            match self.check(source) {
                Admission::Now => return Ok(()),
                Admission::BackedOff(left) => return Err(left),
                Admission::After(wait) => {
                    if started.elapsed() + wait > bound {
                        return Err(bound);
                    }
                    tokio::time::sleep(wait).await;
                }
            }
        }
    }
}

/// `"1s"`, `"500ms"`, `"2m"`, or a bare number of seconds.
pub fn parse_interval(text: &str) -> Option<Duration> {
    let text = text.trim();
    if let Some(ms) = text.strip_suffix("ms") {
        return ms.trim().parse::<u64>().ok().map(Duration::from_millis);
    }
    if let Some(m) = text.strip_suffix('m') {
        return m
            .trim()
            .parse::<u64>()
            .ok()
            .map(|m| Duration::from_secs(m * 60));
    }
    let secs = text.strip_suffix('s').unwrap_or(text).trim();
    secs.parse::<f64>()
        .ok()
        .filter(|s| *s > 0.0)
        .map(Duration::from_secs_f64)
}

static LIMITER: OnceLock<Arc<Limiter>> = OnceLock::new();

/// The process-wide limiter every adapter goes through by default; a test
/// gives its adapter a `Limiter::new()` of its own so a recorded 429 backs
/// off nothing but that test.
pub fn limiter() -> Arc<Limiter> {
    LIMITER.get_or_init(|| Arc::new(Limiter::new())).clone()
}

/// One source's daily count.
#[derive(Debug, Clone)]
struct DayCount {
    per_day: u64,
    used: u64,
    /// The UTC day (days since the epoch) the count is for; a new day
    /// starts the count over.
    day: i64,
}

fn utc_day(now: chrono::DateTime<chrono::Utc>) -> i64 {
    now.timestamp().div_euclid(86_400)
}

/// The next UTC midnight after `now`, RFC 3339: when every daily count resets.
pub fn resets_at(now: chrono::DateTime<chrono::Utc>) -> String {
    let next = (utc_day(now) + 1) * 86_400;
    chrono::DateTime::<chrono::Utc>::from_timestamp(next, 0)
        .unwrap_or(now)
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// The detail of the once-per-source warning a polite-pool source gets when
/// the host set no contact.
pub const NO_CONTACT_DETAIL: &str = "no contact set: polite pool off";

/// Whether a source's declaration asks for a contact beyond the User-Agent
/// (`mailto`: Crossref's and OpenAlex's polite pools; `tool+email`: NCBI's
/// request): the sources that run slower, or not at all, without one.
pub fn wants_contact(record: &ExplainRecord) -> bool {
    matches!(
        record.etiquette.contact.as_deref(),
        Some("mailto") | Some("tool+email")
    )
}

/// The one etiquette layer the service dispatches through: the limiter the
/// adapters share, the daily budgets, and which sources were already warned
/// about the missing contact.
#[derive(Default)]
pub struct Gate {
    limiter: Arc<Limiter>,
    budgets: Mutex<BTreeMap<String, DayCount>>,
    warned: Mutex<BTreeSet<String>>,
    /// A fixed contact instead of [`contact`] (tests, so a sibling test's
    /// `set_contact` cannot race the warning).
    fixed_contact: Option<Option<String>>,
}

impl Gate {
    /// A gate over a limiter of its own (tests).
    pub fn new() -> Self {
        Self::default()
    }

    /// A gate over `limiter` — the process-wide one, so the adapters' 429s
    /// and the service's admission agree.
    pub fn over(limiter: Arc<Limiter>) -> Self {
        Gate {
            limiter,
            ..Default::default()
        }
    }

    /// Fix the contact this gate sees instead of reading [`contact`].
    pub fn with_contact(mut self, contact: Option<String>) -> Self {
        self.fixed_contact = Some(contact);
        self
    }

    fn has_contact(&self) -> bool {
        match &self.fixed_contact {
            Some(fixed) => fixed.is_some(),
            None => contact().is_some(),
        }
    }

    /// The limiter this gate reads; an adapter under test takes the same one.
    pub fn limiter(&self) -> Arc<Limiter> {
        self.limiter.clone()
    }

    /// Declare or change `source`'s daily budget (the explain record's
    /// `budget.per_day` does this on first use; a test sets a small one).
    /// The count so far is kept.
    pub fn set_budget(&self, source: &str, per_day: u64) {
        let mut map = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let day = utc_day(chrono::Utc::now());
        let entry = map.entry(source.to_string()).or_insert(DayCount {
            per_day,
            used: 0,
            day,
        });
        entry.per_day = per_day;
    }

    /// `source`'s budget as `explain` shows it: `per_day` from the
    /// declaration, `remaining` and `resets_at` from the count. `None` when
    /// the record declares none.
    pub fn budget_of(&self, record: &ExplainRecord) -> Option<Budget> {
        let declared = record.etiquette.budget.as_ref()?;
        let now = chrono::Utc::now();
        let map = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
        let used = map
            .get(&record.source)
            .filter(|c| c.day == utc_day(now))
            .map_or(0, |c| c.used);
        let per_day = map
            .get(&record.source)
            .map_or(declared.per_day, |c| c.per_day);
        Some(Budget {
            per_day,
            remaining: Some(per_day.saturating_sub(used)),
            resets_at: Some(resets_at(now)),
        })
    }

    /// Admit one request to the source `record` describes, or refuse it.
    /// `Err` is the `rate_limited` diagnostic (the source is backed off, or
    /// today's budget is spent) and nothing was sent; `Ok` carries the
    /// warnings to append to the result — today only the once-per-source
    /// "no contact set" line — and the request has been counted.
    pub fn admit(&self, record: &ExplainRecord) -> Result<Vec<Diagnostic>, Diagnostic> {
        let source = record.source.as_str();
        let left = self.limiter.backoff_remaining(source);
        if left > Duration::ZERO {
            return Err(Diagnostic::new(
                source,
                Status::RateLimited,
                format!(
                    "backed off for another {}s; nothing was sent",
                    left.as_secs().max(1)
                ),
            ));
        }
        if let Some(declared) = &record.etiquette.budget {
            let now = chrono::Utc::now();
            let today = utc_day(now);
            let mut map = self.budgets.lock().unwrap_or_else(|e| e.into_inner());
            let count = map.entry(source.to_string()).or_insert(DayCount {
                per_day: declared.per_day,
                used: 0,
                day: today,
            });
            if count.day != today {
                count.day = today;
                count.used = 0;
            }
            if count.used >= count.per_day {
                return Err(Diagnostic::new(
                    source,
                    Status::RateLimited,
                    format!(
                        "daily budget of {} spent; resets at {}; nothing was sent",
                        count.per_day,
                        resets_at(now)
                    ),
                ));
            }
            count.used += 1;
        }
        let mut warnings = Vec::new();
        if wants_contact(record) && !self.has_contact() {
            let mut warned = self.warned.lock().unwrap_or_else(|e| e.into_inner());
            if warned.insert(source.to_string()) {
                warnings.push(Diagnostic::new(source, Status::Done, NO_CONTACT_DETAIL));
            }
        }
        Ok(warnings)
    }
}

static GATE: OnceLock<Arc<Gate>> = OnceLock::new();

/// The process-wide gate the linked service dispatches through, over the
/// process-wide [`limiter`]; a test builds a `Gate::new()` of its own.
pub fn gate() -> Arc<Gate> {
    GATE.get_or_init(|| Arc::new(Gate::over(limiter()))).clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_parse() {
        assert_eq!(parse_interval("1s"), Some(Duration::from_secs(1)));
        assert_eq!(parse_interval("500ms"), Some(Duration::from_millis(500)));
        assert_eq!(parse_interval("2m"), Some(Duration::from_secs(120)));
        assert_eq!(parse_interval("3"), Some(Duration::from_secs(3)));
        assert_eq!(parse_interval("soon"), None);
    }

    #[test]
    fn headers_reseed_the_bucket_and_a_429_backs_off() {
        let l = Limiter::new();
        l.update_from_headers("x", Some("3"), Some("1s"));
        assert_eq!(l.limit_of("x"), Some((3, Duration::from_secs(1))));
        assert_eq!(l.check("x"), Admission::Now);
        assert_eq!(l.check("x"), Admission::Now);
        assert_eq!(l.check("x"), Admission::Now);
        assert!(matches!(l.check("x"), Admission::After(_)));
        l.back_off("x", Some(Duration::from_secs(30)));
        assert!(matches!(l.check("x"), Admission::BackedOff(_)));
        assert!(l.backoff_remaining("x") > Duration::from_secs(20));
        // Garbage headers change nothing.
        l.update_from_headers("x", Some("lots"), Some("1s"));
        assert_eq!(l.limit_of("x"), Some((3, Duration::from_secs(1))));
    }

    #[tokio::test]
    async fn acquire_waits_for_a_refill_and_refuses_a_backoff() {
        let l = Limiter::new();
        l.set_limit("y", 2, Duration::from_millis(100));
        assert!(l.acquire("y").await.is_ok());
        assert!(l.acquire("y").await.is_ok());
        let t = Instant::now();
        assert!(l.acquire("y").await.is_ok(), "third waits for a refill");
        assert!(t.elapsed() >= Duration::from_millis(20));
        l.back_off("y", Some(Duration::from_secs(5)));
        assert!(l.acquire("y").await.is_err());
    }

    #[tokio::test]
    async fn a_slow_source_queues_three_of_its_intervals_not_five_seconds() {
        let l = Limiter::new();
        l.set_limit("arxiv-like", 1, Duration::from_secs(3));
        assert_eq!(l.max_wait("arxiv-like"), Duration::from_secs(9));
        assert_eq!(l.max_wait("unseen"), MAX_WAIT);
        assert!(l.acquire("arxiv-like").await.is_ok());
        assert!(l.acquire("arxiv-like").await.is_ok(), "second queues ~3 s");
        let t = Instant::now();
        assert!(
            l.acquire("arxiv-like").await.is_ok(),
            "third queues ~3 s more, past the old 5 s"
        );
        assert!(t.elapsed() >= Duration::from_millis(2500));
    }

    fn record(source: &str, contact: Option<&str>, per_day: Option<u64>) -> ExplainRecord {
        ExplainRecord {
            source: source.into(),
            name: source.into(),
            version: "0".into(),
            kind: "bibliographic-work@1".into(),
            operations: vec!["search".into()],
            indexes: BTreeMap::new(),
            sortable: vec![],
            identifiers: vec![],
            fills: vec![],
            page: koine_sources::explain::Page {
                model: koine_sources::explain::PageModel::Cursor,
                max: None,
            },
            etiquette: koine_sources::explain::Etiquette {
                contact: contact.map(String::from),
                limit: None,
                key: None,
                budget: per_day.map(|per_day| Budget {
                    per_day,
                    remaining: None,
                    resets_at: None,
                }),
            },
            secrets: vec![],
            native: None,
        }
    }

    #[test]
    fn the_gate_counts_a_budget_and_refuses_at_zero() {
        let gate = Gate::new();
        let r = record("z", None, Some(2));
        assert_eq!(gate.budget_of(&r).unwrap().remaining, Some(2));
        assert!(gate.admit(&r).unwrap().is_empty());
        assert!(gate.admit(&r).unwrap().is_empty());
        let refused = gate.admit(&r).unwrap_err();
        assert_eq!(refused.status, Status::RateLimited);
        assert!(
            refused.detail.contains("daily budget of 2 spent"),
            "{refused:?}"
        );
        let shown = gate.budget_of(&r).unwrap();
        assert_eq!(shown.remaining, Some(0));
        assert!(shown.resets_at.unwrap().ends_with("T00:00:00Z"));
        // No declaration, no budget, never refused for one.
        assert!(gate.budget_of(&record("free", None, None)).is_none());
        for _ in 0..5 {
            assert!(gate.admit(&record("free", None, None)).is_ok());
        }
    }

    #[test]
    fn the_gate_refuses_a_backed_off_source_without_sending() {
        let gate = Gate::new();
        gate.limiter().back_off("q", Some(Duration::from_secs(30)));
        let refused = gate.admit(&record("q", None, None)).unwrap_err();
        assert_eq!(refused.status, Status::RateLimited);
        assert!(refused.detail.contains("nothing was sent"));
    }

    #[test]
    fn the_contact_is_the_hosts_or_the_environments() {
        set_contact("mailto:someone@example.org");
        assert_eq!(contact_address().as_deref(), Some("someone@example.org"));
        set_contact("");
        // Falls back to the environment (which the test does not set) or none.
        let _ = contact();
    }
}
