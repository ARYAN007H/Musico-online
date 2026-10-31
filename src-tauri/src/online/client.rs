//! The InnerTube HTTP choke point: all catalog requests (search, radio, and
//! Phase 2's browse endpoints) go through here.  Owns the client identity,
//! endpoint URLs, timeout policy, **global concurrency throttling** and
//! retry/backoff.  (Thumbnail/stream downloads use their own paths — they are
//! throughput-oriented, not rate-limit-sensitive.
//!
//! Rate-limit safety: at most [`MAX_CONCURRENCY`] InnerTube requests are in
//! flight at once (RAII permits); retryable failures back off exponentially
//! with jitter instead of hammering at a fixed interval.  The [`Transport`]
//! trait keeps all of this unit-testable without touching the network.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, LazyLock, Mutex, RwLock};
use std::time::{Duration, Instant};

use rand::Rng;

/// Public web client key used by music.youtube.com itself (not a secret).
pub(crate) const INNERTUBE_KEY: &str = "AIzaSyC9XL3ZjWddXya6X74dJoCTL-WEYFDNX30";
pub(crate) const INNERTUBE_CLIENT: &str = "WEB_REMIX";
/// Fallback client version — used only when the homepage auto-refresh fails
/// and no previous value is cached.
const INNERTUBE_VERSION_FALLBACK: &str = "1.20261025.12.00";
pub(crate) const SEARCH_URL: &str = "https://music.youtube.com/youtubei/v1/search";
pub(crate) const NEXT_URL: &str = "https://music.youtube.com/youtubei/v1/next";
pub(crate) const BROWSE_URL: &str = "https://music.youtube.com/youtubei/v1/browse";
const HOME_URL: &str = "https://music.youtube.com/";

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

/// Total request budget (connect + body read).  20 s because YouTube's
/// `next` (radio) endpoint has been observed taking >12 s end-to-end under
/// load — a timeout there must be headroom, not a hard failure.
pub(crate) const HTTP_TIMEOUT: Duration = Duration::from_secs(20);
/// Connect budget — a black-holed connection must fail fast instead of
/// eating the whole request budget.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Any InnerTube call slower than this logs at warn (faster ones at debug).
const SLOW_REQUEST: Duration = Duration::from_secs(5);
/// How often the live client version is re-fetched.
const VERSION_TTL: Duration = Duration::from_secs(24 * 3600);
/// Paces background version-refresh retries — a dead homepage must not turn
/// every stale-serve into a refresh attempt.
const VERSION_RETRY: Duration = Duration::from_secs(5 * 60);

/// Global cap on in-flight InnerTube requests (rate-limit hygiene).
const MAX_CONCURRENCY: usize = 2;
/// Retry attempts for retryable failures (first try + one retry, as before).
const MAX_ATTEMPTS: u32 = 2;
/// Backoff base: attempt N waits `BASE * 2^N` ms plus 0–[`BACKOFF_JITTER_MS`] ms.
const BACKOFF_BASE_MS: u64 = 400;
const BACKOFF_JITTER_MS: u64 = 150;

// ---------------------------------------------------------------------------
// Throttle — global RAII permits
// ---------------------------------------------------------------------------

static PERMITS: LazyLock<(Mutex<usize>, Condvar)> =
    LazyLock::new(|| (Mutex::new(MAX_CONCURRENCY), Condvar::new()));

/// A held concurrency slot; released on drop.
pub(crate) struct Permit {
    _private: (),
}

impl Drop for Permit {
    fn drop(&mut self) {
        let (lock, cv) = &*PERMITS;
        let mut available = lock.lock().unwrap_or_else(|e| e.into_inner());
        *available += 1;
        cv.notify_one();
    }
}

/// Blocks until a concurrency slot is free.  Fair enough via Condvar wakeups.
pub(crate) fn acquire_permit() -> Permit {
    let (lock, cv) = &*PERMITS;
    let mut available = lock.lock().unwrap_or_else(|e| e.into_inner());
    while *available == 0 {
        available = cv.wait(available).unwrap_or_else(|e| e.into_inner());
    }
    *available -= 1;
    Permit { _private: () }
}

/// How many slots are currently free (test/debug introspection).
#[cfg(test)]
pub(crate) fn free_permits() -> usize {
    let (lock, _) = &*PERMITS;
    *lock.lock().unwrap_or_else(|e| e.into_inner())
}

/// Backoff for retry attempt `attempt` (0-based): `BASE * 2^attempt` ms plus
/// uniform jitter, so parallel failures don't retry in lockstep.
pub(crate) fn backoff_delay(attempt: u32) -> Duration {
    let exp = BACKOFF_BASE_MS.saturating_mul(1u64 << attempt.min(6));
    let jitter = rand::thread_rng().gen_range(0..BACKOFF_JITTER_MS);
    Duration::from_millis(exp + jitter)
}

// ---------------------------------------------------------------------------
// Transport — mockable request layer
// ---------------------------------------------------------------------------

/// Why a request failed.
#[derive(Debug, PartialEq)]
pub(crate) enum TransportError {
    /// Transient (network error, HTTP 5xx) — worth retrying.
    Retryable(String),
    /// Permanent (HTTP 4xx etc.) — surface immediately.
    Fatal(String),
}

/// The raw request layer, abstracted so tests can drive retry/throttle logic
/// without network access.
pub(crate) trait Transport {
    fn send(&self, url: &str, body: &str) -> Result<serde_json::Value, TransportError>;
}

/// One client for the process: TLS setup and the internal runtime thread are
/// paid once; reqwest pools connections across attempts and calls.
fn shared_client() -> Option<reqwest::blocking::Client> {
    static CLIENT: LazyLock<Option<reqwest::blocking::Client>> = LazyLock::new(|| http_client().ok());
    CLIENT.clone()
}

struct ReqwestTransport;

impl Transport for ReqwestTransport {
    fn send(&self, url: &str, body: &str) -> Result<serde_json::Value, TransportError> {
        let client = shared_client().ok_or_else(|| {
            TransportError::Retryable("HTTP client: builder failed".into())
        })?;
        let resp = client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body.to_owned())
            .send()
            .map_err(|e| TransportError::Retryable(format!("request failed: {e}")))?;
        let status = resp.status();
        if status.is_server_error() {
            return Err(TransportError::Retryable(format!(
                "YouTube Music returned HTTP {status}"
            )));
        }
        if !status.is_success() {
            return Err(TransportError::Fatal(format!(
                "YouTube Music returned HTTP {status}"
            )));
        }
        resp.json().map_err(|e| {
            // Timeouts AND reset connections mid-body surface as decode
            // errors in blocking reqwest — both are transient (slow or
            // interrupted endpoint), not fatal.
            if e.is_timeout() || e.is_body() {
                TransportError::Retryable(format!("response body interrupted: {e}"))
            } else {
                TransportError::Fatal(format!("bad response body: {e}"))
            }
        })
    }
}

/// Executes `body` against `transport` under the global throttle, with
/// jittered exponential backoff on retryable failures.  Fatal errors return
/// immediately; retryable ones consume all [`MAX_ATTEMPTS`] before failing.
///
/// The concurrency slot is held for the whole call **including backoff
/// sleeps** — a failing endpoint keeps its slot so fresh traffic can't pile
/// onto it; worst case a caller waits ~40 s for a slot (2 attempts × 20 s
/// request budget, when both permits are pinned by a dead endpoint).
pub(crate) fn post_via(
    transport: &dyn Transport,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    let _permit = acquire_permit();
    let payload = body.to_string();
    let mut last_err = String::new();
    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            std::thread::sleep(backoff_delay(attempt - 1));
        }
        match transport.send(url, &payload) {
            Ok(v) => return Ok(v),
            Err(TransportError::Fatal(e)) => return Err(e),
            Err(TransportError::Retryable(e)) => last_err = e,
        }
    }
    Err(last_err)
}

// ---------------------------------------------------------------------------
// Request coalescing — identical in-flight requests share one round-trip
// ---------------------------------------------------------------------------

struct Coalesced {
    done: bool,
    result: Option<Result<serde_json::Value, String>>,
}

static INFLIGHT: LazyLock<Mutex<HashMap<String, Arc<(Mutex<Coalesced>, Condvar)>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
/// How long a waiter stays parked on its leader before declaring it lost.
const COALESCE_WAIT: Duration = Duration::from_secs(60);

/// Executes `body` under the global throttle, coalescing identical
/// in-flight requests: the first caller leads (permit + retry machinery in
/// [`post_via`]); later callers park on the entry and clone the outcome
/// WITHOUT holding a throttle slot.  Failure semantics: a leader panic is
/// caught and published as an error; a waiter that times out EVICTS the
/// entry so a later caller can lead again — a lost leader never wedges a
/// key forever.
pub(crate) fn post_coalesced(
    transport: &dyn Transport,
    url: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value, String> {
    post_coalesced_wait(transport, url, body, COALESCE_WAIT)
}

pub(crate) fn post_coalesced_wait(
    transport: &dyn Transport,
    url: &str,
    body: &serde_json::Value,
    max_wait: Duration,
) -> Result<serde_json::Value, String> {
    let payload = body.to_string();
    let key = format!("{url}\n{payload}");
    let (leader, entry) = {
        let mut inflight = INFLIGHT.lock().unwrap_or_else(|e| e.into_inner());
        match inflight.get(&key) {
            Some(e) => (false, e.clone()),
            None => {
                let e = Arc::new((
                    Mutex::new(Coalesced { done: false, result: None }),
                    Condvar::new(),
                ));
                inflight.insert(key.clone(), e.clone());
                (true, e)
            }
        }
    };

    if !leader {
        let (lock, cv) = &*entry;
        let deadline = Instant::now() + max_wait;
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !g.done {
            let now = Instant::now();
            if now >= deadline {
                // Lost leader: evict (if the entry is still ours) so the
                // key can't wedge; the caller's caches/retries recover.
                if let Ok(mut inflight) = INFLIGHT.lock() {
                    if inflight.get(&key).is_some_and(|e| Arc::ptr_eq(e, &entry)) {
                        inflight.remove(&key);
                    }
                }
                return Err("coalesced request wait timed out".into());
            }
            let chunk = deadline
                .saturating_duration_since(now)
                .min(Duration::from_secs(5));
            let (g2, _) = cv.wait_timeout(g, chunk).unwrap_or_else(|e| e.into_inner());
            g = g2;
        }
        return g
            .result
            .clone()
            .unwrap_or_else(|| Err("coalesced request lost its result".into()));
    }

    // Leader: run it; publish no matter what (panics included), then drop
    // the entry so the next identical request starts fresh.
    let t0 = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        post_via(transport, url, body)
    }))
    .unwrap_or_else(|_| Err("innertube request panicked".into()));
    let elapsed = t0.elapsed();
    if elapsed >= SLOW_REQUEST {
        log::warn!("innertube {url} took {elapsed:?}");
    } else {
        log::debug!("innertube {url} took {elapsed:?}");
    }

    let (lock, cv) = &*entry;
    {
        let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
        g.done = true;
        g.result = Some(result.clone());
    }
    cv.notify_all();
    if let Ok(mut inflight) = INFLIGHT.lock() {
        if inflight.get(&key).is_some_and(|e| Arc::ptr_eq(e, &entry)) {
            inflight.remove(&key);
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Public helpers
// ---------------------------------------------------------------------------

pub(crate) fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT)
        .tcp_nodelay(true)
        .pool_max_idle_per_host(8)
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

/// POSTs to an InnerTube endpoint (coalescing + retry + throttle applied).
pub(crate) fn innertube_post(url: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let full = format!("{url}?prettyPrint=false&key={INNERTUBE_KEY}");
    post_coalesced(&ReqwestTransport, &full, &body)
}

// ---------------------------------------------------------------------------
// Client version — stale-while-revalidate
// ---------------------------------------------------------------------------

static VERSION_CACHE: LazyLock<RwLock<Option<(Instant, String)>>> =
    LazyLock::new(|| RwLock::new(None));
static VERSION_REFRESHING: AtomicBool = AtomicBool::new(false);
static VERSION_LAST_ATTEMPT: LazyLock<Mutex<Option<Instant>>> = LazyLock::new(|| Mutex::new(None));

/// The live InnerTube client version, auto-refreshed from the
/// music.youtube.com homepage (24 h TTL).  Stale-while-revalidate: once ANY
/// value is known, requests are answered instantly from cache and the
/// refresh runs on a background thread — the homepage fetch never sits on
/// the request path.  Falls back to the last seen value, then to
/// [`INNERTUBE_VERSION_FALLBACK`].
pub(crate) fn client_version() -> String {
    if let Some((at, v)) = VERSION_CACHE.read().ok().and_then(|g| g.clone()) {
        if at.elapsed() < VERSION_TTL {
            return v;
        }
        maybe_refresh_version();
        return v;
    }
    // First use ever: block once, then cache (fallback included).
    let value = fetch_client_version().unwrap_or_else(|| INNERTUBE_VERSION_FALLBACK.to_string());
    if let Ok(mut g) = VERSION_CACHE.write() {
        *g = Some((Instant::now(), value.clone()));
    }
    value
}

fn fetch_client_version() -> Option<String> {
    let html = shared_client()?.get(HOME_URL).send().ok()?.text().ok()?;
    extract_client_version(&html)
}

/// At most one background refresh at a time, retried at most once per
/// [`VERSION_RETRY`] after a failure.
fn maybe_refresh_version() {
    if VERSION_REFRESHING.swap(true, Ordering::Relaxed) {
        return; // a refresh is already in flight
    }
    {
        let mut last = VERSION_LAST_ATTEMPT.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < VERSION_RETRY) {
            VERSION_REFRESHING.store(false, Ordering::Relaxed);
            return;
        }
        *last = Some(Instant::now());
    }
    let spawned = std::thread::Builder::new()
        .name("innertube-version".into())
        .spawn(|| {
            if let Some(v) = fetch_client_version() {
                // Fresh timestamp even if the value is unchanged — the TTL restarts.
                if let Ok(mut g) = VERSION_CACHE.write() {
                    *g = Some((Instant::now(), v));
                }
            }
            VERSION_REFRESHING.store(false, Ordering::Relaxed);
        });
    if spawned.is_err() {
        VERSION_REFRESHING.store(false, Ordering::Relaxed);
    }
}

/// Pulls `"clientVersion":"1.20241001.01.00"` out of the homepage HTML.
/// Regex-free: find the marker, read until the closing quote.
fn extract_client_version(html: &str) -> Option<String> {
    const MARKER: &str = "\"clientVersion\":\"";
    let start = html.find(MARKER)? + MARKER.len();
    let rest = &html[start..];
    let end = rest.find('"')?;
    let v = &rest[..end];
    (!v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')).then(|| v.to_string())
}

pub(crate) fn innertube_context() -> serde_json::Value {
    serde_json::json!({
        "client": {
            "clientName": INNERTUBE_CLIENT,
            "clientVersion": client_version(),
            "hl": "en",
            "gl": "US",
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// The throttle is a process-global shared by cargo's parallel test
    /// threads — every test that touches permits (directly or via post_via)
    /// must hold this gate or assertions on `free_permits()` race.
    static TEST_GATE: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn extracts_client_version_from_html() {
        let html = r#"ytcfg.set({"INNERTUBE_CONTEXT":{"client":{"clientName":"WEB_REMIX","clientVersion":"1.20261025.12.00","hl":"en"}}});"#;
        assert_eq!(extract_client_version(html).as_deref(), Some("1.20261025.12.00"));
        assert_eq!(extract_client_version("no version here"), None);
        assert_eq!(extract_client_version(r#""clientVersion":""#), None);
        // Rejects non-numeric junk after the marker.
        assert_eq!(extract_client_version(r#""clientVersion":"abc""#), None);
    }

    #[test]
    fn backoff_is_exponential_with_bounded_jitter() {
        for attempt in 0..4u32 {
            let lo = BACKOFF_BASE_MS * (1u64 << attempt);
            let hi = lo + BACKOFF_JITTER_MS;
            for _ in 0..20 {
                let d = backoff_delay(attempt).as_millis() as u64;
                assert!((lo..=hi).contains(&d), "attempt {attempt}: {d} not in [{lo}, {hi}]");
            }
        }
        // Exponential growth holds across attempts.
        assert!(backoff_delay(3).as_millis() > backoff_delay(0).as_millis() * 4);
    }

    struct ScriptedTransport {
        /// Failure to return per call index; None = succeed.
        script: Vec<Option<TransportError>>,
        calls: AtomicUsize,
    }

    impl Transport for ScriptedTransport {
        fn send(&self, _url: &str, _body: &str) -> Result<serde_json::Value, TransportError> {
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.script.get(i) {
                Some(Some(e)) => Err(match e {
                    TransportError::Retryable(m) => TransportError::Retryable(m.clone()),
                    TransportError::Fatal(m) => TransportError::Fatal(m.clone()),
                }),
                _ => Ok(serde_json::json!({ "ok": i })),
            }
        }
    }

    #[test]
    fn retry_succeeds_after_transient_failure() {
        let _gate = TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let t = ScriptedTransport {
            script: vec![
                Some(TransportError::Retryable("dns blip".into())),
                None,
            ],
            calls: AtomicUsize::new(0),
        };
        let out = post_via(&t, "https://x", &serde_json::json!({})).expect("second attempt lands");
        assert_eq!(out["ok"], 1);
        assert_eq!(t.calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn fatal_errors_are_not_retried() {
        let _gate = TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let t = ScriptedTransport {
            script: vec![Some(TransportError::Fatal("HTTP 400 Bad Request".into()))],
            calls: AtomicUsize::new(0),
        };
        let err = post_via(&t, "https://x", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("400"), "fatal error surfaced: {err}");
        assert_eq!(t.calls.load(Ordering::SeqCst), 1, "no retry after fatal");
    }

    #[test]
    fn retryable_exhaustion_returns_last_error() {
        let _gate = TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let t = ScriptedTransport {
            script: vec![
                Some(TransportError::Retryable("first".into())),
                Some(TransportError::Retryable("second".into())),
            ],
            calls: AtomicUsize::new(0),
        };
        let err = post_via(&t, "https://x", &serde_json::json!({})).unwrap_err();
        assert!(err.contains("second"), "last retryable error surfaced: {err}");
        assert_eq!(t.calls.load(Ordering::SeqCst), MAX_ATTEMPTS as usize);
    }

    #[test]
    fn throttle_caps_concurrency_at_two() {
        let _gate = TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        // Drain all slots, then confirm a third acquisition blocks.
        let p1 = acquire_permit();
        let p2 = acquire_permit();
        assert_eq!(free_permits(), 0, "both slots held");

        let acquired = Arc::new(AtomicUsize::new(0));
        let a2 = acquired.clone();
        let h = std::thread::spawn(move || {
            let _p = acquire_permit();
            a2.fetch_add(1, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(120));
        assert_eq!(acquired.load(Ordering::SeqCst), 0, "third request must wait");

        drop(p1); // free one slot → the waiter proceeds
        h.join().expect("waiter finishes after a slot frees");
        assert_eq!(acquired.load(Ordering::SeqCst), 1);

        drop(p2); // release the last slot

        // Churn: repeated acquire/release must always restore full capacity
        // (regression guard for the Condvar wakeup path).
        for _ in 0..MAX_CONCURRENCY * 3 {
            let _p = acquire_permit();
        }
        assert_eq!(free_permits(), MAX_CONCURRENCY);
    }

    /// Coalescing tests go through post_via (permits) — hold the gate.
    struct CountingTransport {
        delay: Duration,
        calls: AtomicUsize,
    }

    impl Transport for CountingTransport {
        fn send(&self, _url: &str, _body: &str) -> Result<serde_json::Value, TransportError> {
            std::thread::sleep(self.delay);
            let i = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(serde_json::json!({ "call": i }))
        }
    }

    #[test]
    fn identical_inflight_requests_share_one_roundtrip() {
        let _gate = TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let t = Arc::new(CountingTransport {
            delay: Duration::from_millis(150),
            calls: AtomicUsize::new(0),
        });
        let mut handles = Vec::new();
        for _ in 0..4 {
            let t2 = t.clone();
            handles.push(std::thread::spawn(move || {
                post_coalesced_wait(
                    &*t2,
                    "https://coalesce.test",
                    &serde_json::json!({ "q": "same" }),
                    Duration::from_secs(5),
                )
            }));
        }
        for h in handles {
            h.join().unwrap().expect("every waiter gets the leader's result");
        }
        assert_eq!(
            t.calls.load(Ordering::SeqCst),
            1,
            "four identical requests, one round-trip"
        );
    }

    #[test]
    fn timed_out_waiter_evicts_dead_leader() {
        let _gate = TEST_GATE.lock().unwrap_or_else(|e| e.into_inner());
        let t = Arc::new(CountingTransport {
            delay: Duration::from_millis(400),
            calls: AtomicUsize::new(0),
        });
        let t_leader = t.clone();
        let leader = std::thread::spawn(move || {
            post_coalesced_wait(
                &*t_leader,
                "https://evict.test",
                &serde_json::json!({ "q": "stuck" }),
                Duration::from_secs(5),
            )
        });
        std::thread::sleep(Duration::from_millis(60)); // leader registered
        let waiter = post_coalesced_wait(
            &*t,
            "https://evict.test",
            &serde_json::json!({ "q": "stuck" }),
            Duration::from_millis(80),
        );
        assert!(waiter.is_err(), "waiter times out instead of parking forever");
        // The eviction frees the key: a fresh caller leads a NEW round-trip.
        let fresh = post_coalesced_wait(
            &*t,
            "https://evict.test",
            &serde_json::json!({ "q": "stuck" }),
            Duration::from_secs(5),
        );
        assert!(fresh.is_ok(), "evicted entry does not wedge the key");
        leader.join().unwrap().ok();
        assert_eq!(t.calls.load(Ordering::SeqCst), 2, "leader + fresh leader only");
    }
}
