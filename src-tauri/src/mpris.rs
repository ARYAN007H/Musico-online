//! MPRIS2 D-Bus integration for Musico.
//!
//! Registers `org.mpris.MediaPlayer2.musico` on the session bus.
//! Exposes Player controls and track metadata so system media keys,
//! Bluetooth headphones, KDE/GNOME widgets, and `playerctl` all work.
//!
//! Runs as a `tokio::spawn` task — zero impact on GUI thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use zbus::{interface, Connection};

/// Commands from D-Bus → GUI.
#[derive(Debug, Clone)]
pub enum MprisCommand {
    PlayPause,
    Next,
    Previous,
    Stop,
    Seek(f64),    // offset in microseconds
    SetVolume(f64),
}

/// Metadata snapshot for MPRIS properties.
#[derive(Debug, Clone, Default)]
pub struct MprisMetadata {
    pub track_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_us: i64,
    pub art_url: String,
    pub playback_status: String, // "Playing", "Paused", "Stopped"
    pub volume: f64,
    pub position_us: i64,
}

/// Shared state between MPRIS server and GUI.
pub type MprisState = Arc<Mutex<MprisMetadata>>;

// ─── MPRIS2 Root Interface ──────────────────────────────────────────────────

struct MprisRoot;

#[interface(name = "org.mpris.MediaPlayer2")]
impl MprisRoot {
    fn raise(&self) {
        // Could raise the window — no-op for now.
    }

    fn quit(&self) {
        std::process::exit(0);
    }

    #[zbus(property)]
    fn can_quit(&self) -> bool { true }

    #[zbus(property)]
    fn can_raise(&self) -> bool { false }

    #[zbus(property)]
    fn has_track_list(&self) -> bool { false }

    #[zbus(property)]
    fn identity(&self) -> &str { "Musico" }

    #[zbus(property)]
    fn desktop_entry(&self) -> &str { "musico" }

    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<&str> { vec!["file"] }

    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<&str> {
        vec!["audio/mpeg", "audio/flac", "audio/ogg", "audio/wav", "audio/mp4", "audio/aac"]
    }
}

// ─── MPRIS2 Player Interface ────────────────────────────────────────────────

struct MprisPlayer {
    state: MprisState,
    cmd_tx: mpsc::UnboundedSender<MprisCommand>,
}

#[interface(name = "org.mpris.MediaPlayer2.Player")]
impl MprisPlayer {
    fn next(&self) {
        let _ = self.cmd_tx.send(MprisCommand::Next);
    }

    fn previous(&self) {
        let _ = self.cmd_tx.send(MprisCommand::Previous);
    }

    fn pause(&self) {
        let _ = self.cmd_tx.send(MprisCommand::PlayPause);
    }

    fn play_pause(&self) {
        let _ = self.cmd_tx.send(MprisCommand::PlayPause);
    }

    fn stop(&self) {
        let _ = self.cmd_tx.send(MprisCommand::Stop);
    }

    fn play(&self) {
        let _ = self.cmd_tx.send(MprisCommand::PlayPause);
    }

    fn seek(&self, offset: i64) {
        let _ = self.cmd_tx.send(MprisCommand::Seek(offset as f64));
    }

    #[zbus(property)]
    fn playback_status(&self) -> String {
        let st = self.state.lock().unwrap();
        st.playback_status.clone()
    }

    #[zbus(property)]
    fn rate(&self) -> f64 { 1.0 }

    #[zbus(property)]
    fn set_rate(&self, _rate: f64) {}

    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, zbus::zvariant::Value<'_>> {
        let st = self.state.lock().unwrap();
        let mut m: HashMap<String, zbus::zvariant::Value> = HashMap::new();
        m.insert("mpris:trackid".into(), zbus::zvariant::Value::from(
            zbus::zvariant::ObjectPath::try_from(format!("/org/mpris/MediaPlayer2/Track/{}", st.track_id.replace('-', "_"))).unwrap_or_else(|_| zbus::zvariant::ObjectPath::try_from("/org/mpris/MediaPlayer2/Track/none").unwrap())
        ));
        m.insert("mpris:length".into(), zbus::zvariant::Value::from(st.duration_us));
        m.insert("xesam:title".into(), zbus::zvariant::Value::from(st.title.clone()));
        m.insert("xesam:artist".into(), zbus::zvariant::Value::from(vec![st.artist.clone()]));
        m.insert("xesam:album".into(), zbus::zvariant::Value::from(st.album.clone()));
        m
    }

    #[zbus(property)]
    fn volume(&self) -> f64 {
        let st = self.state.lock().unwrap();
        st.volume
    }

    #[zbus(property)]
    fn set_volume(&self, vol: f64) {
        let _ = self.cmd_tx.send(MprisCommand::SetVolume(vol));
    }

    #[zbus(property)]
    fn position(&self) -> i64 {
        let st = self.state.lock().unwrap();
        st.position_us
    }

    #[zbus(property)]
    fn minimum_rate(&self) -> f64 { 1.0 }

    #[zbus(property)]
    fn maximum_rate(&self) -> f64 { 1.0 }

    #[zbus(property)]
    fn can_go_next(&self) -> bool { true }

    #[zbus(property)]
    fn can_go_previous(&self) -> bool { true }

    #[zbus(property)]
    fn can_play(&self) -> bool { true }

    #[zbus(property)]
    fn can_pause(&self) -> bool { true }

    #[zbus(property)]
    fn can_seek(&self) -> bool { true }

    #[zbus(property)]
    fn can_control(&self) -> bool { true }
}

// ─── Public API ─────────────────────────────────────────────────────────────

/// Spawns the MPRIS2 D-Bus server.
/// Returns a channel to receive commands from D-Bus, and a shared state handle
/// for the GUI to update.
pub async fn start_mpris_server() -> Result<(mpsc::UnboundedReceiver<MprisCommand>, MprisState), Box<dyn std::error::Error + Send + Sync>> {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let state: MprisState = Arc::new(Mutex::new(MprisMetadata::default()));

    let state_clone = state.clone();
    let connection = Connection::session().await?;

    connection
        .object_server()
        .at("/org/mpris/MediaPlayer2", MprisRoot)
        .await?;

    connection
        .object_server()
        .at("/org/mpris/MediaPlayer2", MprisPlayer {
            state: state_clone,
            cmd_tx,
        })
        .await?;

    connection
        .request_name("org.mpris.MediaPlayer2.musico")
        .await?;

    // Keep the connection alive in the background.
    tokio::spawn(async move {
        let _conn = connection;
        // Connection stays alive as long as _conn is held.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
        }
    });

    Ok((cmd_rx, state))
}

/// Update the MPRIS metadata from the GUI.
pub fn update_mpris_state(mpris_state: &MprisState, meta: MprisMetadata) {
    if let Ok(mut st) = mpris_state.lock() {
        *st = meta;
    }
}

/// Blocking loop: translates D-Bus commands into engine calls.
/// Runs on its own thread (started from Tauri setup).
pub fn command_loop(mut cmd_rx: tokio::sync::mpsc::UnboundedReceiver<MprisCommand>, handle: tauri::AppHandle) {
    use tauri::Manager;
    while let Some(cmd) = cmd_rx.blocking_recv() {
        let state = handle.state::<crate::state::AppState>();
        let Some(eng) = state.engine.as_ref().cloned() else { continue };
        match cmd {
            MprisCommand::PlayPause => {
                use musico_playback::state::PlaybackStatus;
                if eng.state().status == PlaybackStatus::Playing {
                    let _ = eng.pause();
                } else {
                    let _ = eng.resume();
                }
            }
            MprisCommand::Next => {
                crate::event_bridge::advance(&handle);
            }
            MprisCommand::Previous => {
                if eng.state().position_secs > 3.0 {
                    let _ = eng.seek(0.0);
                } else {
                    let prev = state
                        .queue
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .previous();
                    if let Some(song) = prev {
                        let _ = eng.play(song);
                    } else {
                        let _ = eng.seek(0.0);
                    }
                }
            }
            MprisCommand::Stop => {
                let _ = eng.stop();
            }
            MprisCommand::Seek(offset_us) => {
                let cur = eng.state().position_secs as f64;
                let target = (cur + offset_us / 1_000_000.0).max(0.0);
                let _ = eng.seek(target as f32);
            }
            MprisCommand::SetVolume(vol) => {
                let _ = eng.set_volume(vol as f32);
            }
        }
    }
}
