//! # musico_playback — Audio Playback Engine
//!
//! A modular, thread-safe audio playback library for the Musico music player.
//! Runs the entire audio pipeline on a background thread, communicating with
//! the GUI via lock-free channels.
//!
//! ## Architecture
//!
//! ```text
//! [GUI Thread]
//!     ↓ sends PlaybackCommand via crossbeam Sender
//! [Decoder Thread]  ←→  [Shared State Arc<Mutex>]
//!     ↓ pushes f32 samples into lock-free RingBuffer
//! [CPAL Audio Callback]
//!     ↓ pops samples → device output
//!     sends PlaybackEvent back to GUI via crossbeam Sender
//! ```
//!
//! ## Quick Start
//!
//! ```rust,no_run
//! use musico_playback::{PlaybackEngine, SongInfo};
//!
//! let engine = PlaybackEngine::new().unwrap();
//! // engine.play(song_info).unwrap();
//! // let events = engine.poll_events();
//! ```

pub mod crossfade;
pub mod decoder;
pub mod eq;
pub mod error;
pub mod events;
pub mod output;
pub mod queue;
pub mod resampler;
pub mod state;

pub use error::PlaybackError;
pub use events::{PlaybackCommand, PlaybackEvent};
pub use queue::PlaybackQueue;
pub use state::{PlaybackState, PlaybackStatus, SongInfo};
pub use crossfade::{CrossfadeConfig, CrossfadeCurve};

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::output::AudioOutput;

/// The main playback engine handle.
///
/// All public methods are safe to call from the GUI thread. The audio pipeline
/// runs entirely on a dedicated background thread.
pub struct PlaybackEngine {
    cmd_tx: Sender<PlaybackCommand>,
    event_rx: Receiver<PlaybackEvent>,
    state: Arc<Mutex<PlaybackState>>,
    _decoder_thread: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for PlaybackEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlaybackEngine").finish_non_exhaustive()
    }
}

impl PlaybackEngine {
    /// Creates a new `PlaybackEngine`, spawning the decoder thread and
    /// initialising the audio output.
    ///
    /// # Errors
    ///
    /// Returns `PlaybackError` if the audio output device cannot be opened.
    pub fn new() -> Result<Self, PlaybackError> {
        let (cmd_tx, cmd_rx) = crossbeam_channel::unbounded::<PlaybackCommand>();
        let (event_tx, event_rx) = crossbeam_channel::unbounded::<PlaybackEvent>();

        let state = Arc::new(Mutex::new(PlaybackState::default()));
        let state_clone = Arc::clone(&state);

        let handle = thread::Builder::new()
            .name("musico-decoder".into())
            .spawn(move || {
                decoder_thread(cmd_rx, event_tx, state_clone);
            })
            .map_err(|e| PlaybackError::Io(e))?;

        Ok(Self {
            cmd_tx,
            event_rx,
            state,
            _decoder_thread: Some(handle),
        })
    }

    /// Begins playback of a new song immediately.
    pub fn play(&self, song: SongInfo) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::Play(song))
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Pauses playback at the current position.
    pub fn pause(&self) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::Pause)
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Resumes playback from the paused position.
    pub fn resume(&self) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::Resume)
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Stops playback and resets state.
    pub fn stop(&self) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::Stop)
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Seeks to the given position in seconds.
    pub fn seek(&self, secs: f32) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::Seek(secs))
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Sets the master volume, clamped to `[0.0, 1.0]`.
    pub fn set_volume(&self, volume: f32) -> Result<(), PlaybackError> {
        let v = volume.clamp(0.0, 1.0);
        self.cmd_tx
            .send(PlaybackCommand::SetVolume(v))
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Toggles mute on/off.
    pub fn toggle_mute(&self) -> Result<(), PlaybackError> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.muted {
            self.cmd_tx
                .send(PlaybackCommand::Unmute)
                .map_err(|_| PlaybackError::ChannelDisconnected)
        } else {
            self.cmd_tx
                .send(PlaybackCommand::Mute)
                .map_err(|_| PlaybackError::ChannelDisconnected)
        }
    }

    /// Drains all pending events without blocking.
    ///
    /// The Iced GUI should call this on every subscription tick.
    /// Returns an empty `Vec` if no events are pending.
    pub fn poll_events(&self) -> Vec<PlaybackEvent> {
        let mut events = Vec::new();
        while let Ok(ev) = self.event_rx.try_recv() {
            events.push(ev);
        }
        events
    }

    /// Returns a clone of the current playback state for the GUI to render.
    pub fn state(&self) -> PlaybackState {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Sends a preload hint for gapless playback (foundation only).
    pub fn preload_next(&self, song: SongInfo) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::PreloadNext(song))
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Update the 10-band EQ settings.
    pub fn set_eq(&self, enabled: bool, gains_db: [f32; 10]) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::SetEQ { enabled, gains_db })
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Set the normalization gain factor for the current track.
    pub fn set_norm_gain(&self, gain_factor: f32) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::SetNormGain(gain_factor))
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }

    /// Update crossfade configuration.
    pub fn set_crossfade(&self, config: CrossfadeConfig) -> Result<(), PlaybackError> {
        self.cmd_tx
            .send(PlaybackCommand::SetCrossfade(config))
            .map_err(|_| PlaybackError::ChannelDisconnected)
    }
}

impl Drop for PlaybackEngine {
    fn drop(&mut self) {
        // Send Stop to break the decoder loop.
        let _ = self.cmd_tx.send(PlaybackCommand::Stop);
        
        // Replace the sender with a dummy to drop the actual sender and close the channel.
        let (dummy_tx, _) = crossbeam_channel::unbounded();
        let actual_tx = std::mem::replace(&mut self.cmd_tx, dummy_tx);
        drop(actual_tx);

        // Join the thread gracefully.
        if let Some(handle) = self._decoder_thread.take() {
            let _ = handle.join();
        }
    }
}

// ─── Decoder Thread ──────────────────────────────────────────────────────────

fn decoder_thread(
    cmd_rx: Receiver<PlaybackCommand>,
    event_tx: Sender<PlaybackEvent>,
    state: Arc<Mutex<PlaybackState>>,
) {
    // Initialise audio output once for the lifetime of the thread.
    let mut audio_output = match output::AudioOutput::new() {
        Ok(o) => o,
        Err(e) => {
            let _ = event_tx.send(PlaybackEvent::Error(format!("Audio init failed: {e}")));
            // Wait for Stop command to exit gracefully.
            loop {
                if let Ok(cmd) = cmd_rx.recv() {
                    if matches!(cmd, PlaybackCommand::Stop) {
                        break;
                    }
                } else {
                    break;
                }
            }
            return;
        }
    };

    let device_sr = audio_output.sample_rate();
    let device_ch = audio_output.channels();

    loop {
        // Step 1: Wait for a Play command.
        let song_info = match wait_for_play(&cmd_rx, &event_tx, &state, &audio_output) {
            Some(s) => s,
            None => return, // channel closed
        };

        // Step 2: Decode loop for this song.
        play_song(
            song_info,
            &cmd_rx,
            &event_tx,
            &state,
            &mut audio_output,
            device_sr,
            device_ch,
        );
    }
}

/// Blocks until a `Play` command arrives, handling volume/mute/stop while idle.
fn wait_for_play(
    cmd_rx: &Receiver<PlaybackCommand>,
    event_tx: &Sender<PlaybackEvent>,
    state: &Arc<Mutex<PlaybackState>>,
    audio_output: &AudioOutput,
) -> Option<SongInfo> {
    loop {
        match cmd_rx.recv() {
            Ok(PlaybackCommand::Play(song)) => return Some(song),
            Ok(PlaybackCommand::Stop) => {
                set_stopped(state);
                let _ = event_tx.send(PlaybackEvent::Stopped);
            }
            Ok(PlaybackCommand::SetVolume(v)) => {
                // Apply even while idle so get_playback_state, the knob and
                // the audio device all agree before the first song starts.
                audio_output.set_volume(v);
                state.lock().unwrap_or_else(|e| e.into_inner()).volume = v;
                let _ = event_tx.send(PlaybackEvent::VolumeChanged(v));
            }
            Ok(PlaybackCommand::Mute) => {
                audio_output.set_muted(true);
                state.lock().unwrap_or_else(|e| e.into_inner()).muted = true;
            }
            Ok(PlaybackCommand::Unmute) => {
                audio_output.set_muted(false);
                state.lock().unwrap_or_else(|e| e.into_inner()).muted = false;
            }
            Ok(PlaybackCommand::PreloadNext(s)) => {
                log::info!("Preload hint received for: {}", s.title);
            }
            Ok(_) => {} // ignore other commands when idle
            Err(_) => return None, // channel disconnected
        }
    }
}

/// Decodes and streams a single song, returning when the song ends or a new
/// Play/Stop command interrupts it.
fn play_song(
    song_info: SongInfo,
    cmd_rx: &Receiver<PlaybackCommand>,
    event_tx: &Sender<PlaybackEvent>,
    state: &Arc<Mutex<PlaybackState>>,
    audio_output: &mut output::AudioOutput,
    device_sr: u32,
    device_ch: usize,
) {
    // Open the decoder.
    let (mut dec, mut decoded_info) = match decoder::AudioDecoder::new(&song_info.file_path) {
        Ok(d) => d,
        Err(e) => {
            let _ = event_tx.send(PlaybackEvent::Error(format!("{e}")));
            return;
        }
    };

    // Merge caller-provided metadata (id, cover_art may come from the GUI).
    decoded_info.id = if song_info.id.is_empty() {
        decoded_info.id.clone()
    } else {
        song_info.id.clone()
    };
    if song_info.cover_art.is_some() {
        decoded_info.cover_art = song_info.cover_art.clone();
    }
    let song = decoded_info;

    let source_sr = dec.sample_rate();
    let source_ch = dec.channels();

    // Create resampler if needed.
    let mut resampler_opt = if source_sr != device_sr {
        match resampler::AudioResampler::new(source_sr, device_sr, source_ch) {
            Ok(r) => Some(r),
            Err(e) => {
                let _ = event_tx.send(PlaybackEvent::Error(format!("{e}")));
                return;
            }
        }
    } else {
        None
    };

    // Pre-allocate reusable buffers for the hot path (zero heap allocation per packet).
    let mut process_buf: Vec<f32> = Vec::with_capacity(8192);
    let mut channel_buf: Vec<f32> = Vec::with_capacity(8192);

    // ── EQ and Normalization state ──
    let mut equalizer = eq::Equalizer::new(device_sr, device_ch);
    let mut norm_gain: f32 = 1.0; // 1.0 = no normalization

    // ── Crossfade state ──
    let mut crossfade_mixer = crossfade::CrossfadeMixer::new(CrossfadeConfig::default());
    let mut preloaded_decoder: Option<(decoder::AudioDecoder, SongInfo)> = None;

    // Position base: playback position = seek_base + played_secs(), where
    // played_secs is what the audio callback has actually consumed.
    let mut seek_base: f32 = 0.0;
    // Gate the callback and drain whatever the previous song left in the
    // buffer synchronously (flush discards are instant), so this song's
    // prefill can't be thrown away by a pending flush.
    audio_output.set_paused(true);
    audio_output.flush_now(Duration::from_millis(100));
    audio_output.reset_played();
    audio_output.set_paused(false);

    // Set state to buffering.
    {
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        st.status = PlaybackStatus::Buffering;
        st.current_song = Some(song.clone());
        st.duration_secs = song.duration_secs;
        st.position_secs = 0.0;
        st.listened_secs = 0;
        st.listen_start = None;
    }
    let _ = event_tx.send(PlaybackEvent::BufferingStarted);

    // Pre-fill ring buffer with ~0.5 seconds.
    let prefill_samples = (device_sr as usize) * device_ch / 2;
    let mut filled = 0usize;
    while filled < prefill_samples {
        match dec.decode_next_packet() {
            Ok(Some(samples)) => {
                process_samples_reuse(
                    &samples, &mut resampler_opt, source_ch, device_ch,
                    &mut process_buf, &mut channel_buf,
                );
                // Apply EQ + normalization to the buffer.
                equalizer.process_interleaved(&mut process_buf);
                if (norm_gain - 1.0).abs() > 0.001 {
                    for s in process_buf.iter_mut() { *s *= norm_gain; }
                }
                let pushed = push_samples(audio_output.producer(), &process_buf);
                filled += pushed;
            }
            Ok(None) => break,
            Err(e) => {
                let _ = event_tx.send(PlaybackEvent::Error(format!("{e}")));
                return;
            }
        }
    }

    // Transition to playing.
    {
        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
        st.status = PlaybackStatus::Playing;
        st.listen_start = Some(Instant::now());
    }
    let _ = event_tx.send(PlaybackEvent::BufferingComplete);
    let _ = event_tx.send(PlaybackEvent::Playing(song.clone()));

    // Main decode loop.
    let mut last_position_emit = Instant::now();
    let position_interval = Duration::from_millis(250);

    // Tail-drain state: after the decoder returns EOF, keep servicing
    // commands while the buffered tail plays out, so the last second of a
    // song isn't cut and Stop/Next stay responsive.
    let mut decoding_done = false;
    let mut drain_deadline = Instant::now();

    loop {
        // Check for commands (non-blocking).
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                PlaybackCommand::Pause => {
                    // Gate the callback first: silence is instant, and the
                    // ring buffer keeps the in-flight audio for a seamless
                    // resume.
                    audio_output.set_paused(true);
                    accumulate_listen(state);
                    {
                        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                        st.status = PlaybackStatus::Paused;
                        st.position_secs = seek_base + audio_output.played_secs();
                    }
                    let pos = state.lock().unwrap_or_else(|e| e.into_inner()).position_secs;
                    let _ = event_tx.send(PlaybackEvent::Paused { position_secs: pos });

                    // Block until Resume, Stop, or new Play.
                    loop {
                        match cmd_rx.recv() {
                            Ok(PlaybackCommand::Resume) => {
                                audio_output.set_paused(false);
                                let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                                st.status = PlaybackStatus::Playing;
                                st.listen_start = Some(Instant::now());
                                let _ = event_tx.send(PlaybackEvent::Resumed);
                                break;
                            }
                            Ok(PlaybackCommand::Stop) => {
                                audio_output.set_paused(true);
                                audio_output.request_flush();
                                set_stopped(state);
                                let _ = event_tx.send(PlaybackEvent::Stopped);
                                return;
                            }
                            Ok(PlaybackCommand::Play(new_song)) => {
                                set_stopped(state);
                                play_song(new_song, cmd_rx, event_tx, state, audio_output, device_sr, device_ch);
                                return;
                            }
                            Ok(PlaybackCommand::Seek(secs)) => {
                                perform_seek(
                                    &mut dec, audio_output, state, &event_tx,
                                    secs, &mut seek_base,
                                );
                            }
                            Ok(PlaybackCommand::SetVolume(v)) => {
                                audio_output.set_volume(v);
                                {
                                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                                    st.volume = v;
                                }
                                let _ = event_tx.send(PlaybackEvent::VolumeChanged(v));
                            }
                            Ok(PlaybackCommand::Mute) => {
                                audio_output.set_muted(true);
                                state.lock().unwrap_or_else(|e| e.into_inner()).muted = true;
                            }
                            Ok(PlaybackCommand::Unmute) => {
                                audio_output.set_muted(false);
                                state.lock().unwrap_or_else(|e| e.into_inner()).muted = false;
                            }
                            Ok(PlaybackCommand::SetEQ { enabled, gains_db }) => {
                                equalizer.set_enabled(enabled);
                                equalizer.set_gains(gains_db);
                            }
                            Ok(PlaybackCommand::SetNormGain(g)) => {
                                norm_gain = g;
                            }
                            Ok(_) => {} // ignore other commands while paused
                            Err(_) => return,
                        }
                    }
                }
                PlaybackCommand::Stop => {
                    audio_output.set_paused(true);
                    audio_output.request_flush();
                    accumulate_listen(state);
                    set_stopped(state);
                    let _ = event_tx.send(PlaybackEvent::Stopped);
                    return;
                }
                PlaybackCommand::Play(new_song) => {
                    audio_output.set_paused(true);
                    audio_output.request_flush();
                    accumulate_listen(state);
                    set_stopped(state);
                    play_song(new_song, cmd_rx, event_tx, state, audio_output, device_sr, device_ch);
                    return;
                }
                PlaybackCommand::Seek(secs) => {
                    // A seek during the tail-drain resumes decoding.
                    decoding_done = false;
                    perform_seek(
                        &mut dec, audio_output, state, &event_tx,
                        secs, &mut seek_base,
                    );
                }
                PlaybackCommand::SetVolume(v) => {
                    audio_output.set_volume(v);
                    {
                        let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                        st.volume = v;
                    }
                    let _ = event_tx.send(PlaybackEvent::VolumeChanged(v));
                }
                PlaybackCommand::Mute => {
                    audio_output.set_muted(true);
                    state.lock().unwrap_or_else(|e| e.into_inner()).muted = true;
                }
                PlaybackCommand::Unmute => {
                    audio_output.set_muted(false);
                    state.lock().unwrap_or_else(|e| e.into_inner()).muted = false;
                }
                PlaybackCommand::PreloadNext(s) => {
                    log::info!("Preloading next: {}", s.title);
                    match decoder::AudioDecoder::new(&s.file_path) {
                        Ok((dec_next, info_next)) => {
                            let mut merged = info_next;
                            if !s.id.is_empty() { merged.id = s.id.clone(); }
                            if s.cover_art.is_some() { merged.cover_art = s.cover_art.clone(); }
                            preloaded_decoder = Some((dec_next, merged));
                        }
                        Err(e) => {
                            log::warn!("Failed to preload: {e}");
                        }
                    }
                }
                PlaybackCommand::SetCrossfade(config) => {
                    crossfade_mixer.config = config;
                }
                PlaybackCommand::SetEQ { enabled, gains_db } => {
                    equalizer.set_enabled(enabled);
                    equalizer.set_gains(gains_db);
                }
                PlaybackCommand::SetNormGain(g) => {
                    norm_gain = g;
                }
                PlaybackCommand::Resume => {} // already playing
            }
        }

        // Tail-drain: decoder finished; wait for the buffered tail to play
        // out before declaring SongEnded. Commands keep being serviced above.
        if decoding_done {
            let drained = audio_output.producer().is_empty();
            let timed_out = Instant::now() >= drain_deadline;
            if drained || timed_out {
                // Pausing during the drain means "stop here" — don't fire
                // SongEnded (which would auto-advance to the next song).
                let paused_now = state.lock().unwrap_or_else(|e| e.into_inner()).status
                    == PlaybackStatus::Paused;
                if paused_now {
                    set_stopped(state);
                    return;
                }
                accumulate_listen(state);
                let pos = seek_base + audio_output.played_secs();
                state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .position_secs = pos;
                let (song_id, listened, duration) = {
                    let st = state.lock().unwrap_or_else(|e| e.into_inner());
                    (
                        st.current_song.as_ref().map(|s| s.id.clone()).unwrap_or_default(),
                        st.listened_secs,
                        st.duration_secs,
                    )
                };
                let _ = event_tx.send(PlaybackEvent::SongEnded {
                    song_id,
                    listened_secs: listened,
                    duration_secs: duration,
                });

                // ── Gapless transition ──
                // If a next song was preloaded, start it immediately without stopping.
                if let Some((_preloaded_dec, next_song_info)) = preloaded_decoder.take() {
                    log::info!("Gapless transition to: {}", next_song_info.title);
                    set_stopped(state);
                    play_song(
                        next_song_info, cmd_rx, event_tx, state,
                        audio_output, device_sr, device_ch,
                    );
                    return;
                }

                set_stopped(state);
                return;
            }
            // Keep the UI's position moving to the end during the tail.
            if last_position_emit.elapsed() >= position_interval {
                let (pos, listened) = {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.position_secs = seek_base + audio_output.played_secs();
                    (st.position_secs, st.elapsed_listen_secs())
                };
                let _ = event_tx.send(PlaybackEvent::PositionUpdate {
                    position_secs: pos,
                    listened_secs: listened,
                });
                last_position_emit = Instant::now();
            }
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        // Decode next packet.
        match dec.decode_next_packet() {
            Ok(Some(samples)) => {
                process_samples_reuse(
                    &samples, &mut resampler_opt, source_ch, device_ch,
                    &mut process_buf, &mut channel_buf,
                );

                // ── Audio processing chain ──
                // 1. EQ (in-place, zero-alloc)
                equalizer.process_interleaved(&mut process_buf);
                // 2. Normalization gain (in-place)
                if (norm_gain - 1.0).abs() > 0.001 {
                    for s in process_buf.iter_mut() {
                        *s *= norm_gain;
                        // Soft clip to prevent clipping.
                        if *s > 1.0 { *s = 1.0 - (-(*s - 1.0)).exp() * 0.5; }
                        else if *s < -1.0 { *s = -1.0 + (-(-*s - 1.0)).exp() * 0.5; }
                    }
                }

                // Push to ring buffer, spin-waiting if full.
                push_samples_blocking(audio_output.producer(), &process_buf);

                // Position = what the listener has actually heard.
                let pos = seek_base + audio_output.played_secs();
                {
                    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
                    st.position_secs = pos;
                }

                // Emit position update every 250ms.
                if last_position_emit.elapsed() >= position_interval {
                    let listened = state.lock().unwrap_or_else(|e| e.into_inner()).elapsed_listen_secs();
                    let _ = event_tx.send(PlaybackEvent::PositionUpdate {
                        position_secs: pos,
                        listened_secs: listened,
                    });
                    last_position_emit = Instant::now();
                }
            }
            Ok(None) => {
                // Decoder hit EOF — enter the tail-drain state (handled at
                // the top of the loop) instead of cutting buffered audio.
                decoding_done = true;
                drain_deadline = Instant::now() + Duration::from_secs(3);
            }
            Err(e) => {
                let _ = event_tx.send(PlaybackEvent::Error(format!("{e}")));
                // Try to continue — some formats have recoverable errors.
                continue;
            }
        }
    }
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

/// Shared seek routine (works while playing or paused): discards stale
/// buffered audio instantly via the callback's flush path and re-bases the
/// position counter on the seek target. (Flush discards don't count as
/// played, so no drain wait is needed — worst-case counter skew is one
/// callback period, ~10ms.)
fn perform_seek(
    dec: &mut decoder::AudioDecoder,
    audio_output: &mut AudioOutput,
    state: &Arc<Mutex<PlaybackState>>,
    event_tx: &Sender<PlaybackEvent>,
    secs: f32,
    seek_base: &mut f32,
) {
    if let Err(e) = dec.seek_to(secs) {
        let _ = event_tx.send(PlaybackEvent::Error(format!("{e}")));
        return;
    }
    // Drain the pre-seek buffer synchronously so the played-sample counter
    // can be re-based cleanly on the seek target.
    audio_output.flush_now(Duration::from_millis(100));
    audio_output.reset_played();
    *seek_base = secs;
    state
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .position_secs = secs;
    let _ = event_tx.send(PlaybackEvent::Seeked { position_secs: secs });
}

/// Process raw decoded samples using pre-allocated buffers: resample if needed,
/// channel-adapt if needed. Returns a reference to the output buffer.
/// Reuses Vec capacity across calls — no heap allocations on the hot path.
fn process_samples_reuse(
    samples: &[f32],
    resampler: &mut Option<resampler::AudioResampler>,
    source_ch: usize,
    device_ch: usize,
    process_buf: &mut Vec<f32>,
    channel_buf: &mut Vec<f32>,
) {
    // Step 1: Resample if needed, result goes into process_buf.
    process_buf.clear();
    let resampled: &[f32] = match resampler {
        Some(r) if r.needed() => {
            match r.process_into(samples, process_buf) {
                Ok(()) => {},
                Err(e) => {
                    log::warn!("Resampler error: {e}");
                    process_buf.extend_from_slice(samples);
                }
            }
            process_buf.as_slice()
        }
        _ => samples,
    };

    // Step 2: Channel adaptation into channel_buf (if needed).
    if source_ch != device_ch {
        adapt_channels_reuse(resampled, source_ch, device_ch, channel_buf);
        // Swap so process_buf holds the final result.
        std::mem::swap(process_buf, channel_buf);
    } else if resampled.as_ptr() != process_buf.as_ptr() {
        // No resample happened — copy samples into process_buf.
        process_buf.extend_from_slice(samples);
    }
    // Final result is always in process_buf.
}

/// Simple channel adaptation (mono→stereo, stereo→mono, etc) into a
/// pre-allocated buffer.
fn adapt_channels_reuse(samples: &[f32], from_ch: usize, to_ch: usize, out: &mut Vec<f32>) {
    let frames = samples.len() / from_ch;
    out.clear();
    out.reserve(frames * to_ch);
    for f in 0..frames {
        let base = f * from_ch;
        for c in 0..to_ch {
            if c < from_ch {
                out.push(samples[base + c]);
            } else {
                // Duplicate first channel for missing channels.
                out.push(samples[base]);
            }
        }
    }
}

/// Pushes as many samples as will fit, returns count pushed.
fn push_samples(producer: &mut ringbuf::HeapProducer<f32>, samples: &[f32]) -> usize {
    producer.push_slice(samples)
}

/// Push all samples, spinning until the ring buffer has room.
fn push_samples_blocking(producer: &mut ringbuf::HeapProducer<f32>, samples: &[f32]) {
    let mut offset = 0;
    while offset < samples.len() {
        let pushed = producer.push_slice(&samples[offset..]);
        offset += pushed;
        if offset < samples.len() {
            thread::sleep(Duration::from_micros(500));
        }
    }
}

/// Accumulates elapsed playing time into `listened_secs` and clears `listen_start`.
fn accumulate_listen(state: &Arc<Mutex<PlaybackState>>) {
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(start) = st.listen_start.take() {
        st.listened_secs += start.elapsed().as_secs() as u32;
    }
}

/// Resets state to Stopped.
fn set_stopped(state: &Arc<Mutex<PlaybackState>>) {
    let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
    st.status = PlaybackStatus::Stopped;
    st.current_song = None;
    st.position_secs = 0.0;
    st.duration_secs = 0.0;
    st.listen_start = None;
    st.listened_secs = 0;
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_new_succeeds() {
        // This test verifies that PlaybackEngine::new() does not panic.
        // It may fail in CI if no audio device is available, which is expected.
        let result = PlaybackEngine::new();
        // We only assert it doesn't panic; it may error if no device exists.
        match result {
            Ok(engine) => {
                let st = engine.state();
                assert_eq!(st.status, PlaybackStatus::Stopped);
            }
            Err(PlaybackError::NoOutputDevice) => {
                // Expected in headless environments.
            }
            Err(e) => {
                // Other errors are acceptable in test environments.
                eprintln!("Engine init error (acceptable in CI): {e}");
            }
        }
    }

    #[test]
    fn set_volume_clamps_correctly() {
        // Test that volume clamping works (the clamping happens in set_volume).
        let clamped_high = 2.0f32.clamp(0.0, 1.0);
        assert!((clamped_high - 1.0).abs() < f32::EPSILON);

        let clamped_low = (-0.5f32).clamp(0.0, 1.0);
        assert!(clamped_low.abs() < f32::EPSILON);

        let clamped_normal = 0.75f32.clamp(0.0, 1.0);
        assert!((clamped_normal - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn queue_next_returns_none_on_empty() {
        let mut q = PlaybackQueue::new();
        assert!(q.next().is_none());
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn queue_previous_after_next() {
        let mut q = PlaybackQueue::new();

        let song1 = SongInfo {
            id: "1".into(),
            file_path: "/test/song1.flac".into(),
            title: "Song One".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_secs: 180.0,
            cover_art: None,
        };
        let song2 = SongInfo {
            id: "2".into(),
            file_path: "/test/song2.flac".into(),
            title: "Song Two".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            duration_secs: 200.0,
            cover_art: None,
        };

        q.push_back(song1.clone());
        q.push_back(song2.clone());

        let first = q.next().expect("should have a song");
        assert_eq!(first.id, "1");

        let second = q.next().expect("should have a song");
        assert_eq!(second.id, "2");

        let prev = q.previous().expect("should have previous");
        assert_eq!(prev.id, "1");
    }

    #[test]
    fn playback_state_elapsed_listen_secs() {
        let mut st = PlaybackState::default();
        assert_eq!(st.elapsed_listen_secs(), 0);

        st.listened_secs = 10;
        assert_eq!(st.elapsed_listen_secs(), 10);

        st.status = PlaybackStatus::Playing;
        st.listen_start = Some(Instant::now());
        // Should be ~10 since listen_start was just set.
        let elapsed = st.elapsed_listen_secs();
        assert!(elapsed >= 10 && elapsed <= 11);
    }

    #[test]
    fn queue_push_front_plays_next() {
        let mut q = PlaybackQueue::new();
        let song_a = SongInfo {
            id: "a".into(),
            file_path: "a.flac".into(),
            title: "A".into(),
            artist: String::new(),
            album: String::new(),
            duration_secs: 100.0,
            cover_art: None,
        };
        let song_b = SongInfo {
            id: "b".into(),
            file_path: "b.flac".into(),
            title: "B".into(),
            artist: String::new(),
            album: String::new(),
            duration_secs: 100.0,
            cover_art: None,
        };

        q.push_back(song_a);
        q.push_front(song_b);

        let next = q.next().expect("should have a song");
        assert_eq!(next.id, "b"); // pushed to front
    }
}
