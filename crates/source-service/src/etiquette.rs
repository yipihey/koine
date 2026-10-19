//! Etiquette, enforced once (S-D8): the contact string set by the host and
//! never per file, and a per-source token bucket seeded from the
//! declaration and refilled from response headers. A 429 backs the source
//! off; while it is backed off every call answers `rate-limited` at once
//! and nothing is retried.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

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
/// `rate-limited` instead of queueing.
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

    /// Wait for a token (bounded by [`MAX_WAIT`]). `Err` carries how long the
    /// source is still backed off, or [`MAX_WAIT`] when the bucket is empty
    /// for longer than we will queue.
    pub async fn acquire(&self, source: &str) -> Result<(), Duration> {
        let started = Instant::now();
        loop {
            match self.check(source) {
                Admission::Now => return Ok(()),
                Admission::BackedOff(left) => return Err(left),
                Admission::After(wait) => {
                    if started.elapsed() + wait > MAX_WAIT {
                        return Err(MAX_WAIT);
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

    #[test]
    fn the_contact_is_the_hosts_or_the_environments() {
        set_contact("mailto:someone@example.org");
        assert_eq!(contact_address().as_deref(), Some("someone@example.org"));
        set_contact("");
        // Falls back to the environment (which the test does not set) or none.
        let _ = contact();
    }
}
