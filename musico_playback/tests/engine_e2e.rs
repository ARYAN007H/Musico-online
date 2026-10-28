//! End-to-end engine test: decode a real WAV → resample (if needed) → CPAL
//! output → event stream. Runs silently (volume 0) but exercises the full
//! pipeline. Requires a working audio device.

use std::time::{Duration, Instant};

use musico_playback::events::PlaybackEvent;
use musico_playback::state::SongInfo;
use musico_playback::PlaybackEngine;

fn test_song(path: &str, duration_secs: f32) -> SongInfo {
    SongInfo {
        id: "test-1".into(),
        file_path: path.into(),
        title: "Test Tone".into(),
        artist: "Engine Test".into(),
        album: "Testing".into(),
        duration_secs,
        cover_art: None,
    }
}

#[test]
fn engine_plays_file_and_emits_position_and_end() {
    let wav = std::env::var("MUSICO_TEST_WAV").unwrap_or_else(|_| "/tmp/musico-test-tone.wav".into());
    if !std::path::Path::new(&wav).exists() {
        panic!(
            "Test WAV missing — run scripts/gen_test_tone.sh or set MUSICO_TEST_WAV"
        );
    }

    let engine = PlaybackEngine::new().expect("engine init (audio device?)");
    engine.set_volume(0.0).expect("set volume");

    // The WAV we generate is ~3s at 44.1kHz.
    let song = test_song(&wav, 3.0);
    engine.play(song).expect("play");

    let mut saw_position_update = false;
    let mut saw_playing = false;
    let mut ended = false;
    let start = Instant::now();

    while start.elapsed() < Duration::from_secs(15) {
        for ev in engine.poll_events() {
            match ev {
                PlaybackEvent::Playing(_) => saw_playing = true,
                PlaybackEvent::PositionUpdate { position_secs, .. } => {
                    if position_secs > 0.5 {
                        saw_position_update = true;
                    }
                }
                PlaybackEvent::SongEnded { .. } => {
                    ended = true;
                    break;
                }
                PlaybackEvent::Error(msg) => panic!("engine error: {msg}"),
                _ => {}
            }
        }
        if ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(saw_playing, "never received Playing event");
    assert!(saw_position_update, "position never advanced — decode/output stalled");
    assert!(ended, "song never ended within 15s");

    let state = engine.state();
    assert_eq!(state.status, musico_playback::state::PlaybackStatus::Stopped);
}

#[test]
fn engine_seek_updates_position() {
    let wav = std::env::var("MUSICO_TEST_WAV").unwrap_or_else(|_| "/tmp/musico-test-tone.wav".into());
    if !std::path::Path::new(&wav).exists() {
        panic!("Test WAV missing");
    }

    let engine = PlaybackEngine::new().expect("engine init (audio device?)");
    engine.set_volume(0.0).expect("set volume");
    engine.play(test_song(&wav, 3.0)).expect("play");

    // Let it start
    let start = Instant::now();
    loop {
        if engine.poll_events().iter().any(|e| matches!(e, PlaybackEvent::PositionUpdate { .. })) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "no position updates after play");
        std::thread::sleep(Duration::from_millis(50));
    }

    engine.seek(2.0).expect("seek");
    let start = Instant::now();
    let mut saw_seeked = false;
    while start.elapsed() < Duration::from_secs(3) {
        for ev in engine.poll_events() {
            match ev {
                PlaybackEvent::Seeked { position_secs } => {
                    assert!((position_secs - 2.0).abs() < 0.5, "seeked to {position_secs}");
                    saw_seeked = true;
                }
                PlaybackEvent::PositionUpdate { position_secs, .. } if position_secs >= 2.0 => {
                    assert!(saw_seeked, "position advanced to target before Seeked event");
                    // Seek verified end-to-end.
                    return;
                }
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("position never reached seek target");
}

/// Pause must silence the callback immediately (the ring buffer used to keep
/// playing for up to 2s) and freeze the position where the listener paused.
#[test]
fn engine_pause_is_instant_and_holds_position() {
    let wav = std::env::var("MUSICO_TEST_WAV").unwrap_or_else(|_| "/tmp/musico-test-tone.wav".into());
    if !std::path::Path::new(&wav).exists() {
        panic!("Test WAV missing");
    }

    let engine = PlaybackEngine::new().expect("engine init (audio device?)");
    engine.set_volume(0.0).expect("set volume");
    engine.play(test_song(&wav, 6.0)).expect("play");

    // Wait until playback is audibly underway.
    let start = Instant::now();
    loop {
        if engine.poll_events().iter().any(|e| matches!(e, PlaybackEvent::Playing(_))) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(5), "no Playing event");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(400));

    let t0 = Instant::now();
    engine.pause().expect("pause");
    let mut paused_at = None;
    while t0.elapsed() < Duration::from_secs(3) {
        for ev in engine.poll_events() {
            if let PlaybackEvent::Paused { position_secs } = ev {
                paused_at = Some((t0.elapsed(), position_secs));
                break;
            }
        }
        if paused_at.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let (latency, pos_at_pause) = paused_at.expect("no Paused event within 3s");
    assert!(
        latency < Duration::from_millis(300),
        "pause latency {latency:?} — audio kept playing after pause"
    );

    // Position must freeze at the paused spot (gate stops consumption).
    std::thread::sleep(Duration::from_millis(800));
    let st = engine.state();
    assert_eq!(st.status, musico_playback::state::PlaybackStatus::Paused);
    assert!(
        st.position_secs < pos_at_pause + 0.25,
        "position kept advancing after pause: {pos_at_pause} → {}",
        st.position_secs
    );

    // Resume continues from the same spot.
    engine.resume().expect("resume");
    let start = Instant::now();
    loop {
        for ev in engine.poll_events() {
            if let PlaybackEvent::PositionUpdate { position_secs, .. } = ev {
                assert!(
                    position_secs >= pos_at_pause - 0.25,
                    "resume jumped backwards: {pos_at_pause} → {position_secs}"
                );
                return; // resumed and advancing
            }
        }
        assert!(start.elapsed() < Duration::from_secs(3), "no position update after resume");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The buffered tail must play out before SongEnded — the last position
/// reported should be near the track's duration instead of ~2s short.
#[test]
fn engine_plays_out_tail_before_song_ended() {
    let wav = std::env::var("MUSICO_TEST_WAV").unwrap_or_else(|_| "/tmp/musico-test-tone.wav".into());
    if !std::path::Path::new(&wav).exists() {
        panic!("Test WAV missing");
    }

    let engine = PlaybackEngine::new().expect("engine init (audio device?)");
    engine.set_volume(0.0).expect("set volume");
    engine.play(test_song(&wav, 3.0)).expect("play");

    let mut last_position = 0.0f32;
    let mut ended_at = None;
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        for ev in engine.poll_events() {
            match ev {
                PlaybackEvent::PositionUpdate { position_secs, .. } => {
                    last_position = last_position.max(position_secs);
                }
                PlaybackEvent::SongEnded { .. } => {
                    ended_at = Some(Instant::now());
                    break;
                }
                _ => {}
            }
        }
        if ended_at.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(ended_at.is_some(), "song never ended within 15s");
    assert!(
        last_position >= 2.6,
        "SongEnded fired with position only at {last_position}s of 3.0s — tail was cut"
    );
}
