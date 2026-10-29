//! # Musico — Tauri IPC Command Handlers
//!
//! Each `#[tauri::command]` function maps to a JS `invoke()` call in the
//! frontend. These handlers delegate to `musico_playback::PlaybackEngine`
//! and the shared `AppState`.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use std::path::PathBuf;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::config::AppSettings;
use crate::state::{AppState, SongMeta};
/// Resolves the engine or fails with a user-readable error.
fn engine(state: &State<AppState>) -> Result<std::sync::Arc<musico_playback::PlaybackEngine>, String> {
    state
        .engine
        .clone()
        .ok_or_else(|| "Playback engine not initialized".into())
}

/// Pushes the current queue contents to the frontend.
fn emit_queue(handle: &AppHandle, state: &State<AppState>) {
    let q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
    let songs: Vec<SongMeta> = q.iter().cloned().map(SongMeta::from).collect();
    let _ = handle.emit("queue-changed", &songs);
}

// ─── Library ─────────────────────────────────────────────────────────────────

/// Returns the full library as a Vec of SongMeta.
#[tauri::command]
pub fn get_library(state: State<AppState>) -> Vec<SongMeta> {
    state.library.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Starts an async library scan. Progress arrives via `scan-progress` events,
/// the final library via `library-updated`. Returns false if already scanning.
#[tauri::command]
pub fn start_scan(handle: AppHandle, state: State<AppState>) -> bool {
    let _ = state; // accessor keeps State alive for the guard check inside
    crate::scanner::start_scan(handle)
}

/// Returns the song's cached cover as a base64 data URL, if one exists.
#[tauri::command]
pub fn get_cover(state: State<AppState>, song_id: String) -> Option<String> {
    let _ = state;
    let bytes = std::fs::read(crate::scanner::cover_path(&song_id)).ok()?;
    Some(format!("data:image/jpeg;base64,{}", B64.encode(bytes)))
}

/// Search the library by query string.
#[tauri::command]
pub fn search_library(state: State<AppState>, query: String) -> Vec<SongMeta> {
    let q = query.to_lowercase();
    let lib = state.library.lock().unwrap_or_else(|e| e.into_inner());
    lib.iter()
        .filter(|s| {
            s.title.to_lowercase().contains(&q)
                || s.artist.to_lowercase().contains(&q)
                || s.album.to_lowercase().contains(&q)
        })
        .cloned()
        .collect()
}

// ─── Playback ────────────────────────────────────────────────────────────────

/// Play a specific song. Routes through the queue so `previous()` works,
/// and updates the recommender's session context.
#[tauri::command]
pub fn play_song(handle: AppHandle, state: State<AppState>, song: SongMeta) -> Result<(), String> {
    let eng = engine(&state)?;
    {
        let mut q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.push_back(song.into());
        let next_song = q.next().ok_or("Failed to enqueue song")?;
        drop(q);
        crate::event_bridge::play_and_track(&eng, &state, next_song)
            .then_some(())
            .ok_or("Failed to start playback")?;
    }
    emit_queue(&handle, &state);
    Ok(())
}

/// Skip to the next song using the full advance policy (queue → repeat/shuffle).
#[tauri::command]
pub fn next(handle: AppHandle, state: State<AppState>) -> Result<(), String> {
    let _ = state;
    crate::event_bridge::advance(&handle);
    Ok(())
}

/// Go back: restart the song if >3s in, otherwise pop history.
#[tauri::command]
pub fn previous(handle: AppHandle, state: State<AppState>) -> Result<(), String> {
    let eng = engine(&state)?;
    if eng.state().position_secs > 3.0 {
        return eng.seek(0.0).map_err(|e| e.to_string());
    }
    let prev_song = {
        let mut q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
        q.previous()
    };
    match prev_song {
        Some(song) => {
            crate::event_bridge::play_and_track(&eng, &state, song)
                .then_some(())
                .ok_or("Failed to start playback")?;
            emit_queue(&handle, &state);
            Ok(())
        }
        None => eng.seek(0.0).map_err(|e| e.to_string()),
    }
}

/// Toggles autoplay (keep playing when the queue runs dry) and persists it.
#[tauri::command]
pub fn set_autoplay(state: State<AppState>, enabled: bool) -> Result<(), String> {
    let mut settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    settings.autoplay = enabled;
    settings.save()?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    Ok(())
}

/// Sets shuffle/repeat modes (0=off, shuffle: 2=smart radio; repeat: 1=one, 2=all)
/// and persists them.
#[tauri::command]
pub fn set_playback_modes(state: State<AppState>, shuffle: u8, repeat: u8) -> Result<(), String> {
    let mut settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    settings.shuffle_mode = shuffle.min(2);
    settings.repeat_mode = repeat.min(2);
    settings.save()?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    Ok(())
}

/// Pause playback.
#[tauri::command]
pub fn pause(state: State<AppState>) -> Result<(), String> {
    engine(&state)?.pause().map_err(|e| e.to_string())
}

/// Resume playback.
#[tauri::command]
pub fn resume(state: State<AppState>) -> Result<(), String> {
    engine(&state)?.resume().map_err(|e| e.to_string())
}

/// Seek to a position in seconds.
#[tauri::command]
pub fn seek(state: State<AppState>, position_secs: f32) -> Result<(), String> {
    engine(&state)?.seek(position_secs).map_err(|e| e.to_string())
}

/// Set volume (0.0 – 1.0).
#[tauri::command]
pub fn set_volume(state: State<AppState>, volume: f32) -> Result<(), String> {
    engine(&state)?.set_volume(volume).map_err(|e| e.to_string())
}

/// Toggle mute.
#[tauri::command]
pub fn toggle_mute(state: State<AppState>) -> Result<(), String> {
    engine(&state)?.toggle_mute().map_err(|e| e.to_string())
}

// ─── Playback State ──────────────────────────────────────────────────────────

/// Get current playback state for the frontend to poll.
#[derive(serde::Serialize)]
pub struct PlaybackStateDto {
    pub status: String,
    pub position_secs: f32,
    pub duration_secs: f32,
    pub volume: f32,
    pub muted: bool,
    pub current_song: Option<SongMeta>,
}

#[tauri::command]
pub fn get_playback_state(state: State<AppState>) -> PlaybackStateDto {
    match state.engine.as_ref() {
        Some(eng) => {
            let ps = eng.state();
            PlaybackStateDto {
                status: format!("{:?}", ps.status),
                position_secs: ps.position_secs,
                duration_secs: ps.duration_secs,
                volume: ps.volume,
                muted: ps.muted,
                current_song: ps.current_song.map(SongMeta::from),
            }
        }
        None => PlaybackStateDto {
            status: "Stopped".into(),
            position_secs: 0.0,
            duration_secs: 0.0,
            volume: 0.68,
            muted: false,
            current_song: None,
        },
    }
}

// ─── Queue ───────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_queue(state: State<AppState>) -> Vec<SongMeta> {
    let q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
    q.iter().cloned().map(SongMeta::from).collect()
}

#[tauri::command]
pub fn add_to_queue(handle: AppHandle, state: State<AppState>, song: SongMeta) {
    state
        .queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(song.into());
    emit_queue(&handle, &state);
}

/// Add a song to the front of the queue (play next).
#[tauri::command]
pub fn play_next(handle: AppHandle, state: State<AppState>, song: SongMeta) {
    state
        .queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_front(song.into());
    emit_queue(&handle, &state);
}

#[tauri::command]
pub fn remove_from_queue(handle: AppHandle, state: State<AppState>, song_id: String) {
    let mut q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
    let idx = q.current_queue().iter().position(|s| s.id == song_id);
    if let Some(i) = idx {
        q.remove_at(i);
    }
    drop(q);
    emit_queue(&handle, &state);
}

#[tauri::command]
pub fn clear_queue(handle: AppHandle, state: State<AppState>) {
    state.queue.lock().unwrap_or_else(|e| e.into_inner()).clear();
    emit_queue(&handle, &state);
}

// ─── Settings ────────────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> AppSettings {
    state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Applies and persists settings to ~/.config/musico/settings.json.
#[tauri::command]
pub fn save_settings(state: State<AppState>, settings: AppSettings) -> Result<(), String> {
    settings.save()?;
    if let Some(eng) = state.engine.as_ref() {
        let _ = eng.set_volume(settings.volume.clamp(0.0, 1.0));
    }
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    Ok(())
}

/// Opens a native folder dialog; on success, persists the folder and starts
/// a scan. Returns the chosen path or null if cancelled.
#[tauri::command]
pub async fn pick_folder(handle: tauri::AppHandle) -> Option<String> {
    let dir = rfd::AsyncFileDialog::new()
        .set_title("Choose your music folder")
        .pick_folder()
        .await?;
    let path = dir.path().to_string_lossy().to_string();
    apply_music_folder(&handle, path.clone()).await?;
    Some(path)
}

/// Sets the music folder, persists it, and rescans. Returns the new path.
#[tauri::command]
pub async fn set_music_folder(handle: tauri::AppHandle, path: String) -> Result<String, String> {
    apply_music_folder(&handle, path.clone()).await.ok_or("scan failed to start")?;
    Ok(path)
}

async fn apply_music_folder(handle: &tauri::AppHandle, path: String) -> Option<()> {
    {
        let state = handle.state::<AppState>();
        *state.music_folder.lock().unwrap_or_else(|e| e.into_inner()) = PathBuf::from(&path);
        let mut settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        settings.music_folder = Some(path);
        let _ = settings.save();
    }
    crate::scanner::start_scan(handle.clone());
    Some(())
}

// ─── Stats & Playlists ───────────────────────────────────────────────────────

#[derive(serde::Serialize)]
pub struct TopSongDto {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub plays: u32,
}

#[derive(serde::Serialize)]
pub struct StatsDto {
    pub total_plays: u32,
    pub total_listened_secs: u64,
    pub total_skips: u32,
    pub streak_days: u32,
    pub avg_listen_ratio: f32,
    pub top_songs: Vec<TopSongDto>,
    pub top_artists: Vec<(String, u32)>,
    pub daily_minutes: Vec<(String, u32)>,
}

#[tauri::command]
pub fn get_stats(state: State<AppState>) -> StatsDto {
    let empty = StatsDto {
        total_plays: 0,
        total_listened_secs: 0,
        total_skips: 0,
        streak_days: 0,
        avg_listen_ratio: 0.0,
        top_songs: vec![],
        top_artists: vec![],
        daily_minutes: vec![],
    };
    let Some(rec) = state.recommender.as_ref() else { return empty };
    let stats = match rec.lock().unwrap_or_else(|e| e.into_inner()).get_stats() {
        Ok(s) => s,
        Err(_) => return empty,
    };
    // Resolve top song ids against the library for titles.
    let lib = state.library.lock().unwrap_or_else(|e| e.into_inner());
    let top_songs = stats
        .top_songs
        .into_iter()
        .map(|(id, plays)| {
            lib.iter()
                .find(|s| s.id == id)
                .map(|s| TopSongDto { id: id.clone(), title: s.title.clone(), artist: s.artist.clone(), plays })
                .unwrap_or(TopSongDto { id, title: "Unknown".into(), artist: String::new(), plays })
        })
        .collect();
    StatsDto {
        total_plays: stats.total_plays,
        total_listened_secs: stats.total_listened_secs,
        total_skips: stats.total_skips,
        streak_days: stats.streak_days,
        avg_listen_ratio: stats.avg_listen_ratio,
        top_songs,
        top_artists: stats.top_artists,
        daily_minutes: stats.daily_minutes,
    }
}

#[tauri::command]
pub fn get_playlists(state: State<AppState>) -> Vec<musico_recommender::playlists::SmartPlaylist> {
    match state.recommender.as_ref() {
        Some(rec) => rec
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_playlists()
            .unwrap_or_default(),
        None => vec![],
    }
}

// ─── EQ & Normalization ──────────────────────────────────────────────────────

/// Applies the 10-band EQ. `gains` is up to 10 dB values (-12..12).
#[tauri::command]
pub fn set_eq(state: State<AppState>, enabled: bool, gains: Vec<f32>) -> Result<(), String> {
    let eng = engine(&state)?;
    let mut arr = [0f32; 10];
    for (i, g) in gains.iter().take(10).enumerate() {
        arr[i] = g.clamp(-12.0, 12.0);
    }
    eng.set_eq(enabled, arr).map_err(|e| e.to_string())
}

/// Sets normalization mode ("Off" | "Track") and applies it to the current track.
#[tauri::command]
pub fn set_normalization(state: State<AppState>, mode: String) -> Result<(), String> {
    let eng = engine(&state)?;
    {
        let mut settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
        settings.normalization = mode;
        settings.save()?;
        *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    }
    if state
        .settings
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .normalization
        != "Track"
    {
        // Reset to unity gain when disabled.
        let _ = eng.set_norm_gain(1.0);
    } else if let Some(song) = eng.state().current_song {
        let rec = state.recommender.clone();
        if let Some(rec) = rec {
            if let Ok(mut r) = rec.lock() {
                if let Ok(Some(record)) = r.get_song_by_id(&song.id) {
                    let factor = 10f32.powf((-18.0 - record.replay_gain_db) / 20.0);
                    let _ = eng.set_norm_gain(factor.clamp(0.1, 4.0));
                }
            }
        }
    }
    Ok(())
}

// ─── Lyrics ──────────────────────────────────────────────────────────────────

/// Sidecar .lrc lyrics for a song, looked up by library song id.
#[tauri::command]
pub fn get_lyrics(state: State<AppState>, song_id: String) -> Option<crate::lyrics::LyricsDto> {
    let path = {
        let lib = state.library.lock().unwrap_or_else(|e| e.into_inner());
        lib.iter().find(|s| s.id == song_id).map(|s| s.file_path.clone())
    }?;
    crate::lyrics::load_sidecar(&path)
}

// ─── Recommendations ─────────────────────────────────────────────────────────

#[tauri::command]
pub fn get_recommendations(handle: AppHandle, state: State<AppState>) -> Vec<SongMeta> {
    let Some(rec) = state.recommender.as_ref() else {
        return fallback_recs(&state);
    };
    let current_id = handle
        .state::<AppState>()
        .engine
        .as_ref()
        .and_then(|e| e.state().current_song)
        .map(|s| s.id)
        .unwrap_or_default();

    let mut r = match rec.try_lock() {
        // Indexer may hold the lock during long analysis — fall back
        // instantly instead of blocking the UI.
        Ok(g) => g,
        Err(_) => return fallback_recs(&state),
    };
    match r.get_recommendations(&current_id, 6) {
        Ok(recs) if !recs.is_empty() => recs
            .into_iter()
            .map(|rs| SongMeta {
                id: rs.record.id,
                title: rs.record.title,
                artist: rs.record.artist,
                album: rs.record.album,
                duration_secs: rs.record.duration_secs as f32,
                file_path: rs.record.file_path,
            })
            .collect(),
        // Fresh DB / no current song: cosine has nothing to anchor on.
        _ => fallback_recs(&state),
    }
}

/// Non-smart fallback: a deterministic-ish spread from the library.
fn fallback_recs(state: &State<AppState>) -> Vec<SongMeta> {
    let lib = state.library.lock().unwrap_or_else(|e| e.into_inner());
    if lib.len() <= 6 {
        return lib.clone();
    }
    let step = lib.len() / 6;
    (0..6).map(|i| lib[i * step].clone()).collect()
}

// ─── Likes ───────────────────────────────────────────────────────────────────

/// Sets (or clears) a song's like — the backend is the source of truth for ♥,
/// feeding the recommender's profile, affinity and cooldown signals.
#[tauri::command]
pub fn set_song_like(state: State<AppState>, song_id: String, liked: bool) -> Result<(), String> {
    let Some(rec) = state.recommender.as_ref() else {
        return Err("Recommender unavailable".into());
    };
    let like_state = if liked {
        musico_recommender::LikeState::Liked
    } else {
        musico_recommender::LikeState::Neutral
    };
    rec.lock()
        .unwrap_or_else(|e| e.into_inner())
        .set_like(&song_id, like_state)
        .map_err(|e| e.to_string())
}

/// All liked song ids — used by the frontend for one-time migration of its
/// localStorage likes and for the Liked filter pill.
#[tauri::command]
pub fn get_liked_ids(state: State<AppState>) -> Vec<String> {
    state
        .recommender
        .as_ref()
        .map(|rec| {
            rec.lock()
                .unwrap_or_else(|e| e.into_inner())
                .liked_ids()
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

// ─── Catalog library (C4.1: saved albums / artists / tracks) ─────────────────

pub use musico_recommender::library::CatalogEntry as LibraryEntry;

#[tauri::command]
pub fn library_add(state: State<AppState>, mut entry: LibraryEntry) -> Result<bool, String> {
    // Backend stamps added_at — a client-supplied value would let a stale
    // frontend bury entries in Recently Added forever.
    entry.added_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    rec(&state)?.library_put(&entry).map_err(|e| e.to_string())?;
    // Bool, not unit: Tauri serializes Ok(()) as null, and the frontend
    // distinguishes success from failure by null-vs-value.
    Ok(true)
}

#[tauri::command]
pub fn library_remove(state: State<AppState>, kind: String, id: String) -> Result<(), String> {
    rec(&state)?.library_remove(&kind, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn library_list(state: State<AppState>, kind: String) -> Result<Vec<LibraryEntry>, String> {
    rec(&state)?.library_list(&kind).map_err(|e| e.to_string())
}

/// Which of `ids` are saved — drives Add-to-Library button state in bulk.
#[tauri::command]
pub fn library_contains(
    state: State<AppState>,
    kind: String,
    ids: Vec<String>,
) -> Result<Vec<String>, String> {
    rec(&state)?.library_contains(&kind, &ids).map_err(|e| e.to_string())
}

fn rec<'a>(
    state: &'a State<'a, AppState>,
) -> Result<std::sync::MutexGuard<'a, musico_recommender::MusicRecommender>, String> {
    state
        .recommender
        .as_ref()
        .ok_or("recommender unavailable".to_string())?
        .lock()
        .map_err(|e| format!("recommender lock: {e}"))
}

// ─── Online (InnerTube search + yt-dlp playback) ─────────────────────────────

use crate::online::OnlineTrack;

/// Search YouTube Music (InnerTube WEB_REMIX).  `videos` selects the Videos
/// chip; `continuation` fetches the next page of a previous result.  Runs on
/// the blocking pool — the request takes ~0.5–2s and sync commands would pin
/// a worker for that long.
#[tauri::command]
pub async fn online_search(
    query: String,
    videos: Option<bool>,
    continuation: Option<String>,
) -> Result<crate::online::SearchResultPage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::online::search(&query, videos.unwrap_or(false), continuation.as_deref())
    })
    .await
    .map_err(|e| format!("search task failed: {e}"))?
}

/// Resolve + download (if needed) an online track and start playing it
/// through the normal queue path, so previous()/queue ops all work.
/// Progress goes to the frontend via `online-status` events.
#[tauri::command]
pub async fn online_play(
    handle: AppHandle,
    state: State<'_, AppState>,
    track: OnlineTrack,
) -> Result<(), String> {
    let _ = handle.emit(
        "online-status",
        serde_json::json!({ "stage": "resolving", "videoId": track.video_id, "title": track.title }),
    );

    let file = resolve_online_track(&handle, &track, false).await?;

    let _ = handle.emit(
        "online-status",
        serde_json::json!({ "stage": "playing", "videoId": track.video_id, "title": track.title }),
    );

    let song = crate::online::to_song_info(&track, &file);
    let eng = engine(&state)?;
    {
        let mut q = state.queue.lock().unwrap_or_else(|e| e.into_inner());
        // Front, not back: "play this now" must survive concurrent enqueues
        // (queue-all drains through a 3-wide pool for seconds) and play the
        // clicked track, not whatever landed in the queue meanwhile.
        q.push_front(song);
        let next_song = q.next().ok_or("Failed to enqueue song")?;
        drop(q);
        crate::event_bridge::play_and_track(&eng, &state, next_song)
            .then_some(())
            .ok_or("Failed to start playback")?;
    }
    emit_queue(&handle, &state);
    Ok(())
}

/// Resolve an online track to a local file, emitting `online-status`
/// progress events. Shared by every online play/queue command.  `quiet`
/// skips the error event — downloads report failure via `download-state`
/// instead (a triple toast helped nobody).
async fn resolve_online_track(
    handle: &AppHandle,
    track: &OnlineTrack,
    quiet: bool,
) -> Result<std::path::PathBuf, String> {
    let video_id = track.video_id.clone();
    let file = tauri::async_runtime::spawn_blocking(move || {
        crate::online::resolve_stream(&video_id, std::time::Duration::from_secs(240))
    })
    .await
    .map_err(|e| format!("resolve task failed: {e}"))?;
    match file {
        Ok(f) => Ok(f),
        Err(e) => {
            if !quiet {
                let _ = handle.emit(
                    "online-status",
                    serde_json::json!({ "stage": "error", "videoId": track.video_id, "message": e }),
                );
            }
            Err(e)
        }
    }
}

/// Add an online track to the queue without playing it.  The stream is
/// resolved in the background so hitting the track in the queue later is
/// instant; the `online-status` event reports progress.
#[tauri::command]
pub async fn online_enqueue(
    handle: AppHandle,
    state: State<'_, AppState>,
    track: OnlineTrack,
) -> Result<(), String> {
    let _ = handle.emit(
        "online-status",
        serde_json::json!({ "stage": "resolving", "videoId": track.video_id, "title": track.title }),
    );

    let file = resolve_online_track(&handle, &track, false).await?;

    let song = crate::online::to_song_info(&track, &file);
    state
        .queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(song);
    emit_queue(&handle, &state);
    Ok(())
}

/// Insert an online track at the front of the queue (Play Next).  Resolved
/// first like online_enqueue so reaching it in the queue is instant.
#[tauri::command]
pub async fn online_play_next(
    handle: AppHandle,
    state: State<'_, AppState>,
    track: OnlineTrack,
) -> Result<(), String> {
    let _ = handle.emit(
        "online-status",
        serde_json::json!({ "stage": "resolving", "videoId": track.video_id, "title": track.title }),
    );

    let file = resolve_online_track(&handle, &track, false).await?;

    let song = crate::online::to_song_info(&track, &file);
    state
        .queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_front(song);
    let _ = handle.emit(
        "online-status",
        serde_json::json!({ "stage": "queued-next", "videoId": track.video_id, "title": track.title }),
    );
    emit_queue(&handle, &state);
    Ok(())
}

/// Start a radio station from an online track: drops every upcoming song
/// (history is kept, so Previous still works) and plays the track now.
/// When it ends with the queue empty, the online-radio advance chains
/// related tracks from it — that IS the station.
#[tauri::command]
pub async fn online_start_radio(
    handle: AppHandle,
    state: State<'_, AppState>,
    track: OnlineTrack,
) -> Result<(), String> {
    state
        .queue
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear_upcoming();
    let _ = handle.emit(
        "online-status",
        serde_json::json!({ "stage": "radio", "videoId": track.video_id, "title": track.title }),
    );
    online_play(handle, state, track).await
}

/// Explicitly download an online track (cache fill without touching the
/// queue).  State machine `resolving → done | failed` is tracked by
/// online::download and mirrored to the frontend via `download-state`
/// events.  Retrying a failure = invoking this again.
#[tauri::command]
pub async fn online_download(handle: AppHandle, track: OnlineTrack) -> Result<(), String> {
    crate::online::download::start(&track.video_id, &track.title);
    let _ = handle.emit(
        "download-state",
        serde_json::json!({ "videoId": track.video_id, "stage": "resolving", "title": track.title }),
    );

    let result = match resolve_online_track(&handle, &track, true).await {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    };

    crate::online::download::finish(&track.video_id, result.clone());
    let (stage, message) = match &result {
        Ok(()) => ("done", serde_json::Value::Null),
        Err(e) => ("failed", serde_json::json!(e)),
    };
    let _ = handle.emit(
        "download-state",
        serde_json::json!({ "videoId": track.video_id, "stage": stage, "title": track.title, "message": message }),
    );
    result
}

/// Stream-cache usage + the effective cap (MB) for the storage settings UI.
#[tauri::command]
pub fn online_cache_usage() -> serde_json::Value {
    let used = crate::online::stream::cache_usage_bytes();
    let cap_mb = crate::online::stream::effective_cap_mb();
    serde_json::json!({ "used_bytes": used, "cap_mb": cap_mb })
}

/// Persists the storage-slider cap (MB), applies it immediately (LRU-prune)
/// and reports the post-prune usage.  `None` resets to the built-in default.
#[tauri::command]
pub fn set_online_cache_cap(
    state: State<'_, AppState>,
    mb: Option<u32>,
) -> Result<serde_json::Value, String> {
    let mut settings = state.settings.lock().unwrap_or_else(|e| e.into_inner()).clone();
    settings.online_cache_mb = mb;
    settings.save()?;
    *state.settings.lock().unwrap_or_else(|e| e.into_inner()) = settings;
    let mb64 = u64::from(mb.unwrap_or(0));
    crate::online::stream::set_cache_cap_override_mb(mb64);
    crate::online::prune_cache_to_cap();
    let used = crate::online::stream::cache_usage_bytes();
    // Report the EFFECTIVE cap — the env override may differ from the slider.
    Ok(serde_json::json!({ "used_bytes": used, "cap_mb": crate::online::stream::effective_cap_mb() }))
}

/// The home (Listen Now) feed: editorial + personalized shelves.
#[tauri::command]
pub async fn online_home() -> Result<crate::online::BrowsePage, String> {
    tauri::async_runtime::spawn_blocking(crate::online::home_feed)
        .await
        .map_err(|e| format!("home task failed: {e}"))?
}

/// The mood/genre category grid for the Browse tab.
#[tauri::command]
pub async fn online_genres() -> Result<Vec<crate::online::ShelfItem>, String> {
    tauri::async_runtime::spawn_blocking(crate::online::genres)
        .await
        .map_err(|e| format!("genres task failed: {e}"))?
}

/// Generic catalog browse page (album / artist / playlist / genre) by its
/// InnerTube navigation endpoint.  Detail pages (C2.2) call this with
/// browseId+params captured from shelf items.
#[tauri::command]
pub async fn online_browse(
    browse_id: String,
    params: Option<String>,
) -> Result<crate::online::BrowsePage, String> {
    tauri::async_runtime::spawn_blocking(move || {
        crate::online::browse(&browse_id, params.as_deref())
    })
    .await
    .map_err(|e| format!("browse task failed: {e}"))?
}

/// Is yt-dlp present?  Frontend shows an install hint when not.
#[tauri::command]
pub fn online_available() -> bool {
    crate::online::yt_dlp_available()
}

/// Wipe the downloaded-stream cache.  Returns bytes freed.
#[tauri::command]
pub fn online_clear_cache() -> Result<u64, String> {
    crate::online::clear_cache()
}
