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

use std::sync::{Condvar, LazyLock, Mutex, RwLock};
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
const HOME_URL: &str = "https://music.youtube.com/";

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:130.0) Gecko/20100101 Firefox/130.0";

pub(crate) const HTTP_TIMEOUT: Duration = Duration::from_secs(12);
/// How often the live client version is re-fetched.
const VERSION_TTL: Duration = Duration::from_secs(24 * 3600);

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
        resp.json()
            .map_err(|e| TransportError::Fatal(format!("bad response body: {e}")))
    }
}

/// Executes `body` against `transport` under the global throttle, with
/// jittered exponential backoff on retryable failures.  Fatal errors return
/// immediately; retryable ones consume all [`MAX_ATTEMPTS`] before failing.
///
/// The concurrency slot is held for the whole call **including backoff
/// sleeps** — a failing endpoint keeps its slot so fresh traffic can't pile
/// onto it; worst case a caller waits ~1 s for a slot (2 attempts × backoff).
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
// Public helpers
// ---------------------------------------------------------------------------

pub(crate) fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

/// POSTs to an InnerTube endpoint (retry + throttle applied).
pub(crate) fn innertube_post(url: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let full = format!("{url}?prettyPrint=false&key={INNERTUBE_KEY}");
    post_via(&ReqwestTransport, &full, &body)
}

/// The live InnerTube client version, auto-refreshed from the
/// music.youtube.com homepage (24 h TTL).  Falls back to the last seen value,
/// then to [`INNERTUBE_VERSION_FALLBACK`].
pub(crate) fn client_version() -> String {
    static CACHE: LazyLock<RwLock<Option<(Instant, String)>>> =
        LazyLock::new(|| RwLock::new(None));

    if let Some((at, v)) = CACHE.read().ok().and_then(|g| g.clone()) {
        if at.elapsed() < VERSION_TTL {
            return v;
        }
    }

    let fresh = http_client().ok().and_then(|client| {
        let html = client.get(HOME_URL).send().ok()?.text().ok()?;
        extract_client_version(&html)
    });

    let value = match fresh {
        Some(v) => v,
        None => match CACHE.read().ok().and_then(|g| g.clone()) {
            Some((_, stale)) => stale,
            None => INNERTUBE_VERSION_FALLBACK.to_string(),
        },
    };
    if let Ok(mut g) = CACHE.write() {
        *g = Some((Instant::now(), value.clone()));
    }
    value
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
}
