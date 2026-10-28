//! # Musico — Synced Lyrics (LRC)
//!
//! Ported from the Iced app's `lyrics.rs`. Loads sidecar `.lrc` files
//! (same name as the audio file) and exposes them to the frontend as
//! timestamped lines; the webview does the karaoke highlighting.

use std::path::Path;

use serde::Serialize;

/// A single timestamped lyric line.
#[derive(Debug, Clone, Serialize)]
pub struct LrcLine {
    pub time_secs: f32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LyricsDto {
    pub synced: bool,
    pub lines: Vec<LrcLine>,
}

/// Parse an LRC string into timed lines (ignores `[ar:…]` metadata tags).
pub fn parse_lrc(content: &str) -> Vec<LrcLine> {
    let mut lines = Vec::new();
    for raw_line in content.lines() {
        let raw_line = raw_line.trim();
        if raw_line.is_empty() {
            continue;
        }
        // Pattern: [mm:ss.xx] text — multiple leading timestamps are allowed.
        let mut rest = raw_line;
        let mut times: Vec<f32> = Vec::new();
        loop {
            let Some(start) = rest.find('[') else { break };
            let Some(end_off) = rest[start..].find(']') else { break };
            let end = start + end_off;
            let tag = &rest[start + 1..end];
            match parse_timestamp(tag) {
                Some(t) => times.push(t),
                None => {
                    // Not a timestamp — either metadata or plain text.
                    if times.is_empty() {
                        break; // metadata tag line like [ar:Artist]
                    }
                }
            }
            rest = rest[end + 1..].trim();
            if times.is_empty() {
                break;
            }
        }
        let text = rest.trim().to_string();
        if text.is_empty() {
            continue;
        }
        for t in times {
            lines.push(LrcLine { time_secs: t, text: text.clone() });
        }
    }
    lines.sort_by(|a, b| a.time_secs.partial_cmp(&b.time_secs).unwrap_or(std::cmp::Ordering::Equal));
    lines
}

/// Parse "mm:ss.xx" / "m:ss" into seconds. Multi-minute forms not needed.
fn parse_timestamp(s: &str) -> Option<f32> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let minutes: f32 = parts[0].parse().ok()?;
    let seconds: f32 = parts[1].parse().ok()?;
    Some(minutes * 60.0 + seconds)
}

/// Find and parse the sidecar .lrc next to an audio file.
/// Returns None when there is no lyrics file at all.
pub fn load_sidecar(audio_path: &str) -> Option<LyricsDto> {
    let lrc_path = Path::new(audio_path).with_extension("lrc");
    let content = std::fs::read_to_string(lrc_path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    let lines = parse_lrc(&content);
    if !lines.is_empty() {
        return Some(LyricsDto { synced: true, lines });
    }
    // Unsynced plain-text lyrics
    Some(LyricsDto {
        synced: false,
        lines: content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| LrcLine { time_secs: 0.0, text: l.trim().to_string() })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lrc_with_metadata() {
        let content = "[ar:Test Artist]\n[00:12.34] Hello world\n[01:00] Second line\n";
        let lines = parse_lrc(content);
        assert_eq!(lines.len(), 2);
        assert!((lines[0].time_secs - 12.34).abs() < 0.01);
        assert_eq!(lines[0].text, "Hello world");
        assert!((lines[1].time_secs - 60.0).abs() < 0.01);
    }

    #[test]
    fn duplicate_timestamps_expand() {
        let content = "[00:05.00][01:05.00] Chorus\n";
        let lines = parse_lrc(content);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "Chorus");
    }

    #[test]
    fn unsynced_fallback() {
        let tmp = std::env::temp_dir().join("musico-test-unsynced.txt");
        std::fs::write(&tmp, "just some words\non lines\n").unwrap();
        let lrc = tmp.with_extension("lrc");
        std::fs::write(&lrc, "plain line one\nplain line two\n").unwrap();
        let dto = load_sidecar(&tmp.to_string_lossy()).unwrap();
        assert!(!dto.synced);
        assert_eq!(dto.lines.len(), 2);
        let _ = std::fs::remove_file(lrc);
    }
}
