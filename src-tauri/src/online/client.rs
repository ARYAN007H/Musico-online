//! The InnerTube HTTP choke point: every network byte for catalog traffic
//! goes through here.  Owns the client identity, endpoint URLs, timeout
//! policy and retry/backoff.  (ROADMAP C0.3 adds throttling + a mockable
//! transport on top of this base.)

use std::sync::{LazyLock, RwLock};
use std::time::{Duration, Instant};

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

pub(crate) fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(HTTP_TIMEOUT)
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

pub(crate) fn innertube_post(
    client: &reqwest::blocking::Client,
    url: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let full = format!("{url}?prettyPrint=false&key={INNERTUBE_KEY}");
    let mut last_err = String::new();
    // One retry: transient TLS/DNS/5xx blips are common enough to matter.
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(Duration::from_millis(400));
        }
        let resp = match client.post(&full).json(&body).send() {
            Ok(r) => r,
            Err(e) => {
                last_err = format!("request failed: {e}");
                continue;
            }
        };
        let status = resp.status();
        if status.is_server_error() {
            last_err = format!("YouTube Music returned HTTP {status}");
            continue;
        }
        if !status.is_success() {
            return Err(format!("YouTube Music returned HTTP {status}"));
        }
        return resp.json().map_err(|e| format!("bad response body: {e}"));
    }
    Err(last_err)
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

    #[test]
    fn extracts_client_version_from_html() {
        let html = r#"ytcfg.set({"INNERTUBE_CONTEXT":{"client":{"clientName":"WEB_REMIX","clientVersion":"1.20261025.12.00","hl":"en"}}});"#;
        assert_eq!(extract_client_version(html).as_deref(), Some("1.20261025.12.00"));
        assert_eq!(extract_client_version("no version here"), None);
        assert_eq!(extract_client_version(r#""clientVersion":""#), None);
        // Rejects non-numeric junk after the marker.
        assert_eq!(extract_client_version(r#""clientVersion":"abc""#), None);
    }
}
