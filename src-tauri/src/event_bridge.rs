//! # Musico — Playback Event Bridge
//!
//! A background thread that drains `PlaybackEngine::poll_events()` and
//! forwards each event to the frontend as a Tauri `playback-event`.
//! On `SongEnded` it auto-advances to the next queued song, so playback
//! never dead-ends.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tauri::{AppHandle, Emitter, Manager};

use musico_playback::events::PlaybackEvent;
use musico_playback::{PlaybackEngine, SongInfo};

use crate::state::{AppState, SongMeta};
const EVENT_NAME: &str = "playback-event";
const QUEUE_EVENT: &str = "queue-changed";
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Mirrors engine state into the MPRIS D-Bus properties (media keys, playerctl).
#[cfg(target_os = "linux")]
fn update_mpris(engine: &Arc<PlaybackEngine>, handle: &AppHandle) {
    use tauri::Manager;
    let state = handle.state::<AppState>();
    let Some(mpris_state) = state
        .mpris_state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
    else {
        return;
    };
    let ps = engine.state();
    use musico_playback::state::PlaybackStatus;
    let status = match ps.status {
        PlaybackStatus::Playing => "Playing",
        PlaybackStatus::Paused | PlaybackStatus::Buffering => "Paused",
        PlaybackStatus::Stopped => "Stopped",
    };
    let mut meta = crate::mpris::MprisMetadata {
        playback_status: status.into(),
        position_us: (ps.position_secs * 1_000_000.0) as i64,
        duration_us: (ps.duration_secs * 1_000_000.0) as i64,
        volume: ps.volume as f64,
        ..Default::default()
    };
    if let Some(song) = ps.current_song {
        meta.track_id = song.id.clone();
        meta.title = song.title.clone();
        meta.artist = song.artist.clone();
        meta.album = song.album.clone();
        let cover = crate::scanner::cover_path(&song.id);
        if cover.exists() {
            meta.art_url = format!("file://{}", cover.display());
        }
    }
    crate::mpris::update_mpris_state(&mpris_state, meta);
}

/// Serializes a PlaybackEvent into a tagged JSON payload for the frontend.
fn to_payload(ev: &PlaybackEvent) -> serde_json::Value {
    match ev {
        PlaybackEvent::Playing(song) => json!({
            "type": "Playing",
            "song": SongMeta::from(song.clone()),
        }),
        PlaybackEvent::Paused { position_secs } => json!({
            "type": "Paused",
            "position_secs": position_secs,
        }),
        PlaybackEvent::Resumed => json!({ "type": "Resumed" }),
        PlaybackEvent::Stopped => json!({ "type": "Stopped" }),
        PlaybackEvent::Seeked { position_secs } => json!({
            "type": "Seeked",
            "position_secs": position_secs,
        }),
        PlaybackEvent::PositionUpdate { position_secs, .. } => json!({
            "type": "PositionUpdate",
            "position_secs": position_secs,
        }),
        PlaybackEvent::SongEnded { song_id, listened_secs, duration_secs } => json!({
            "type": "SongEnded",
            "song_id": song_id,
            "listened_secs": listened_secs,
            "duration_secs": duration_secs,
        }),
        PlaybackEvent::BufferingStarted => json!({ "type": "BufferingStarted" }),
        PlaybackEvent::BufferingComplete => json!({ "type": "BufferingComplete" }),
        PlaybackEvent::Error(msg) => json!({ "type": "Error", "message": msg }),
        PlaybackEvent::VolumeChanged(volume) => json!({
            "type": "VolumeChanged",
            "volume": volume,
        }),
    }
}

/// Auto-advance policy, applied when the current song ends or `next` is hit.
pub(crate) fn advance(handle: &AppHandle) -> bool {
    advance_from(handle, None, true)
}

/// Full playback-advance policy. `after_song` carries the song that just
/// ended (for listen logging + Smart Radio context).  `allow_online_radio`
/// guards the fallback path against recursion.
/// 1. Pop the next queued song.
/// 2. Repeat One → replay the current song.
/// 3. Online song + queue empty → YouTube Music radio (related tracks),
///    fetched and downloaded on a worker thread so the event loop never stalls.
/// 4. Smart Radio (shuffle mode 2 + smart_recs on) → play the top local
///    recommendation.
/// 5. Shuffle / Repeat All → play a random library song.
/// 6. Otherwise stop.
fn advance_from(handle: &AppHandle, ended: Option<&PlaybackEvent>, allow_online_radio: bool) -> bool {
    let state = handle.state::<AppState>();
    let Some(eng) = state.engine.as_ref().cloned() else { return false };

    // Log the listen (skip detection included) so every taste signal learns:
    // song affinity, artist rep, cluster, time-of-day bucket, profile — and
    // only non-skipped ends move the session mood centroid.
    if let Some(PlaybackEvent::SongEnded { song_id, listened_secs, duration_secs }) = ended {
        if let Some(rec) = &state.recommender {
            if let Ok(mut r) = rec.lock() {
                if let Err(e) = r.on_song_ended(song_id, *listened_secs, *duration_secs as u32) {
                    log::warn!("on_song_ended failed: {e}");
                }
            }
        }
    }

    // 1. Explicit queue first
    let next_song = state.queue.lock().unwrap_or_else(|e| e.into_inner()).next();
    if let Some(song) = next_song {
        if play_and_track(&eng, &state, song) {
            emit_queue_snapshot(handle, &state);
            return true;
        }
        return false;
    }

    let (shuffle, repeat, smart_recs, autoplay) = {
        let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        (s.shuffle_mode, s.repeat_mode, s.smart_recs, s.autoplay)
    };

    // 2. Repeat One
    if repeat == 1 {
        if let Some(current) = eng.state().current_song {
            let mut replay = current;
            replay.cover_art = None;
            return play_and_track(&eng, &state, replay);
        }
    }

    // 3. Online radio: a `yt:` song can't be meaningfully followed by local
    //    library picks — continue with YouTube Music related tracks.  The
    //    fetch + download run on a worker thread (seconds of blocking work
    //    must not stall the 50 ms event loop); failure falls back to the
    //    local branches below.
    if allow_online_radio && repeat != 1 {
        if let Some(current) = eng.state().current_song {
            if current.id.starts_with("yt:") {
                let seed = current.id.trim_start_matches("yt:").to_string();
                let h = handle.clone();
                let spawned = std::thread::Builder::new()
                    .name("online-radio".into())
                    .spawn(move || online_radio_advance(&h, seed));
                if spawned.is_ok() {
                    return true;
                }
            }
        }
    }

    // 4. Smart Radio: top recommendation anchored on the current song
    //    (also fires when autoplay is on, regardless of shuffle mode)
    if (shuffle == 2 && smart_recs) || autoplay {
        if let Some(rec) = &state.recommender {
            let current_id = eng
                .state()
                .current_song
                .map(|s| s.id)
                .unwrap_or_default();
            if let Ok(mut r) = rec.lock() {
                if let Ok(recs) = r.get_recommendations(&current_id, 1) {
                    if let Some(top) = recs.into_iter().next() {
                        let song = SongInfo {
                            id: top.record.id,
                            file_path: top.record.file_path,
                            title: top.record.title,
                            artist: top.record.artist,
                            album: top.record.album,
                            duration_secs: top.record.duration_secs as f32,
                            cover_art: None,
                        };
                        if play_and_track(&eng, &state, song) {
                            return true;
                        }
                    }
                }
            }
        }
    }

    // 5. Random pick for shuffle / repeat-all
    if shuffle > 0 || repeat == 2 {
        let library = state.library.lock().unwrap_or_else(|e| e.into_inner());
        if !library.is_empty() {
            use rand::seq::SliceRandom;
            let picked: SongMeta = library.choose(&mut rand::thread_rng()).unwrap().clone();
            drop(library);
            return play_and_track(&eng, &state, picked.into());
        }
    }

    // 6. Nothing to play — ensure the UI resets
    let _ = eng.stop();
    false
}

/// Continues playback after an online track: fetches YouTube Music related
/// tracks for the seed, resolves + downloads the pick, and plays it through
/// the queue path.  Runs on the `online-radio` worker thread — everything in
/// here is allowed to block.  On any failure the local advance policy runs
/// (with online radio disabled, so no recursion).
fn online_radio_advance(handle: &AppHandle, seed_video_id: String) {
    let outcome = (|| -> Result<(), String> {
        let tracks = crate::online::radio_tracks(&seed_video_id)?;
        let Some(track) = tracks.first() else {
            return Err("radio: no related tracks".into());
        };
        crate::online::remember_radio_play(&track.video_id);
        let _ = handle.emit(
            "online-status",
            json!({ "stage": "resolving", "videoId": track.video_id, "title": track.title }),
        );
        let file =
            crate::online::resolve_stream(&track.video_id, std::time::Duration::from_secs(240))?;
        let song = crate::online::to_song_info(track, &file);

        let state = handle.state::<AppState>();
        let Some(eng) = state.engine.as_ref().cloned() else {
            return Err("playback engine unavailable".into());
        };
        {
            let mut q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
            q.push_back(song);
            let next = q.next().ok_or("enqueue failed")?;
            drop(q);
            if !play_and_track(&eng, &state, next) {
                return Err("failed to start radio playback".into());
            }
        }
        Ok(())
    })();

    if let Err(e) = outcome {
        log::warn!("online radio failed: {e}");
        let _ = handle.emit(
            "online-status",
            json!({ "stage": "error", "message": e }),
        );
        advance_from(handle, None, false);
    }
}

/// Plays a song and notifies the recommender's session tracker, then applies
/// ReplayGain normalization when enabled.
pub(crate) fn play_and_track(
    eng: &Arc<PlaybackEngine>,
    state: &tauri::State<AppState>,
    song: SongInfo,
) -> bool {
    if eng.play(song.clone()).is_err() {
        return false;
    }
    if let Some(rec) = &state.recommender {
        if let Ok(mut r) = rec.lock() {
            let _ = r.on_song_changed(&song.id);
        }
    }
    apply_normalization(eng, state, &song.id);
    true
}

/// Track-mode normalization: gain = 10^((target_db − track_db) / 20),
/// target −18 dBFS. SongRecord.replay_gain_db holds the track's measured RMS.
fn apply_normalization(eng: &Arc<PlaybackEngine>, state: &tauri::State<AppState>, song_id: &str) {
    let mode = {
        let s = state.settings.lock().unwrap_or_else(|e| e.into_inner());
        s.normalization.clone()
    };
    if mode != "Track" {
        return;
    }
    let Some(rec) = &state.recommender else { return };
    if let Ok(mut r) = rec.lock() {
        if let Ok(Some(record)) = r.get_song_by_id(song_id) {
            let factor = 10f32.powf((-18.0 - record.replay_gain_db) / 20.0);
            let _ = eng.set_norm_gain(factor.clamp(0.1, 4.0));
        }
    }
}

fn emit_queue_snapshot(handle: &AppHandle, state: &tauri::State<AppState>) {
    let q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
    let songs: Vec<SongMeta> = q.iter().cloned().map(SongMeta::from).collect();
    let _ = handle.emit(QUEUE_EVENT, &songs);
}

/// The event loop. Spawned once at startup; runs for the app's lifetime.
pub fn spawn(engine: Arc<PlaybackEngine>, handle: AppHandle) {
    std::thread::Builder::new()
        .name("playback-event-bridge".into())
        .spawn(move || loop {
            for ev in engine.poll_events() {
                let ended = matches!(ev, PlaybackEvent::SongEnded { .. });
                if ended {
                    // Frontend still gets the SongEnded event; advance first so
                    // the follow-up Playing event arrives immediately after.
                    advance_from(&handle, Some(&ev), true);
                }
                #[cfg(target_os = "linux")]
                update_mpris(&engine, &handle);
                #[cfg(not(target_os = "linux"))]
                crate::media_controls::update(&engine, &handle);
                let _ = handle.emit(EVENT_NAME, to_payload(&ev));
            }
            std::thread::sleep(POLL_INTERVAL);
        })
        .expect("failed to spawn playback event bridge");
}
