//! System media keys off Linux — Windows SMTC and macOS
//! MPNowPlayingInfoCenter / MPRemoteCommandCenter, via `souvlaki`.
//! Linux uses the hand-rolled MPRIS server (`mpris.rs`) instead.

use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use musico_playback::state::PlaybackStatus;
use musico_playback::PlaybackEngine;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

/// Souvlaki handlers are `'static` closures with no way back into Tauri
/// state, so the app handle is parked here once `init` runs.
static APP: OnceLock<AppHandle> = OnceLock::new();

/// Controls live here rather than in AppState: nothing else needs to reach
/// them, and it keeps one more cfg out of the shared state struct.
static CONTROLS: OnceLock<Mutex<Option<MediaControls>>> = OnceLock::new();

/// Creates the platform controls and attaches key handlers. Best-effort:
/// every failure logs and leaves the app fully functional without media keys.
pub(crate) fn init(handle: &AppHandle, hwnd: Option<*mut c_void>) {
    let _ = APP.set(handle.clone());

    let mut controls = match MediaControls::new(PlatformConfig {
        dbus_name: "musico",
        display_name: "Musico",
        hwnd,
    }) {
        Ok(c) => c,
        Err(e) => {
            log::warn!("OS media controls unavailable: {e}");
            return;
        }
    };

    let app = handle.clone();
    if let Err(e) = controls.attach(move |ev| dispatch(&app, ev)) {
        log::warn!("media key handlers failed to attach: {e}");
        return;
    }

    let _ = CONTROLS.set(Mutex::new(Some(controls)));
    // Idle start: report Stopped before anything plays.
    let engine = handle.state::<AppState>().engine.clone();
    if let Err(e) = set_playback_status(engine.as_deref()) {
        log::warn!("media controls initial state failed: {e}");
    }
}

/// Routes an OS media-key event through the same commands the UI uses.
fn dispatch(app: &AppHandle, ev: MediaControlEvent) {
    let Some(engine) = app.state::<AppState>().engine.clone() else {
        return;
    };
    let result: Result<(), String> = match ev {
        MediaControlEvent::Play => crate::commands::resume(app.state()),
        MediaControlEvent::Pause => crate::commands::pause(app.state()),
        MediaControlEvent::Toggle => {
            if engine.state().status == PlaybackStatus::Playing {
                crate::commands::pause(app.state())
            } else {
                crate::commands::resume(app.state())
            }
        }
        MediaControlEvent::Next => crate::commands::next(app.clone(), app.state()),
        MediaControlEvent::Previous => crate::commands::previous(app.clone(), app.state()),
        MediaControlEvent::Stop => engine.stop().map_err(|e| e.to_string()),
        MediaControlEvent::Seek(dir) => {
            let secs = engine.state().position_secs;
            let target = match dir {
                souvlaki::SeekDirection::Forward => secs + 5.0,
                souvlaki::SeekDirection::Backward => (secs - 5.0).max(0.0),
            };
            crate::commands::seek(app.state(), target)
        }
        MediaControlEvent::SeekBy(dir, d) => {
            let secs = engine.state().position_secs;
            let d = d.as_secs_f32();
            let target = match dir {
                souvlaki::SeekDirection::Forward => secs + d,
                souvlaki::SeekDirection::Backward => (secs - d).max(0.0),
            };
            crate::commands::seek(app.state(), target)
        }
        MediaControlEvent::SetPosition(pos) => {
            crate::commands::seek(app.state(), pos.0.as_secs_f32())
        }
        MediaControlEvent::SetVolume(v) => {
            engine.set_volume(v.clamp(0.0, 1.0) as f32).map_err(|e| e.to_string())
        }
        MediaControlEvent::Raise => {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
            Ok(())
        }
        MediaControlEvent::Quit => {
            app.exit(0);
            Ok(())
        }
        // SetShuffle / OpenUri: not wired to OS controls yet.
        _ => Ok(()),
    };
    if let Err(e) = result {
        log::warn!("media key action failed: {e}");
    }
}

/// Mirrors engine state into the OS controls. Called from the event bridge
/// on every playback event (~250ms ticks drive the progress bar).
pub(crate) fn update(engine: &PlaybackEngine, handle: &AppHandle) {
    if let Err(e) = set_playback_status(Some(engine)) {
        log::warn!("media controls playback update failed: {e}");
    }
    if let Err(e) = set_metadata(engine) {
        log::warn!("media controls metadata update failed: {e}");
    }
}

fn set_playback_status(engine: Option<&PlaybackEngine>) -> Result<(), String> {
    let mut guard = controls().ok_or("media controls not initialized")?
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let controls = guard.as_mut().expect("controls present after init");

    let playback = match engine.map(PlaybackEngine::state) {
        Some(ps) => match ps.status {
            PlaybackStatus::Playing => MediaPlayback::Playing {
                progress: Some(MediaPosition(Duration::from_secs_f32(ps.position_secs))),
            },
            PlaybackStatus::Paused | PlaybackStatus::Buffering => MediaPlayback::Paused {
                progress: Some(MediaPosition(Duration::from_secs_f32(ps.position_secs))),
            },
            PlaybackStatus::Stopped => MediaPlayback::Stopped,
        },
        None => MediaPlayback::Stopped,
    };
    controls.set_playback(playback).map_err(|e| e.to_string())
}

/// SMTC/now-playing metadata only changes with the song — track the last
/// id so 250ms position ticks don't re-push cover art four times a second.
fn set_metadata(engine: &PlaybackEngine) -> Result<(), String> {
    static LAST_SONG: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    let last = LAST_SONG.get_or_init(|| Mutex::new(None));

    let Some(song) = engine.state().current_song else {
        return Ok(());
    };
    if last.lock().unwrap_or_else(|e| e.into_inner()).as_deref() == Some(song.id.as_str()) {
        return Ok(());
    }

    let cover = crate::scanner::cover_path(&song.id);
    let art_url = cover
        .exists()
        .then(|| format!("file://{}", cover.display()));

    let meta = MediaMetadata {
        title: Some(&song.title),
        artist: Some(&song.artist),
        album: Some(&song.album),
        cover_url: art_url.as_deref(),
        duration: Some(Duration::from_secs_f32(song.duration_secs)),
    };
    let mut guard = controls().ok_or("media controls not initialized")?
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    guard
        .as_mut()
        .expect("controls present after init")
        .set_metadata(meta)
        .map_err(|e| e.to_string())?;
    *last.lock().unwrap_or_else(|e| e.into_inner()) = Some(song.id);
    Ok(())
}

fn controls() -> Option<&'static Mutex<Option<MediaControls>>> {
    CONTROLS.get()
}
