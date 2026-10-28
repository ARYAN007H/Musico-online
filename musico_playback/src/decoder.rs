//! Symphonia-based audio decoder pipeline.
//!
//! Opens audio files, extracts metadata + cover art, and produces
//! interleaved f32 PCM samples one packet at a time.

use crate::error::PlaybackError;
use crate::state::SongInfo;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{StandardTagKey, MetadataOptions};
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

use std::fs::File;

/// A streaming audio decoder backed by Symphonia.
///
/// Decodes one packet at a time — never loads the entire file into memory.
pub struct AudioDecoder {
    format_reader: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::Decoder>,
    track_id: u32,
    sample_rate: u32,
    channels: usize,
    total_frames: Option<u64>,
}

impl AudioDecoder {
    /// Opens an audio file and returns a ready-to-decode `AudioDecoder`
    /// along with the extracted `SongInfo` (metadata + cover art).
    ///
    /// # Errors
    ///
    /// Returns `PlaybackError::DecodeFailed` if the file cannot be opened,
    /// probed, or if no supported audio track is found.
    pub fn new(file_path: &str) -> Result<(Self, SongInfo), PlaybackError> {
        let file = File::open(file_path).map_err(|e| PlaybackError::DecodeFailed {
            path: file_path.to_string(),
            reason: format!("cannot open file: {e}"),
        })?;

        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = std::path::Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
        {
            hint.with_extension(ext);
        }

        let format_opts = FormatOptions {
            enable_gapless: true,
            ..Default::default()
        };
        let metadata_opts = MetadataOptions::default();

        let probed = symphonia::default::get_probe()
            .format(&hint, mss, &format_opts, &metadata_opts)
            .map_err(|e| PlaybackError::DecodeFailed {
                path: file_path.to_string(),
                reason: format!("probe failed: {e}"),
            })?;

        let mut format_reader = probed.format;

        // Select the first audio track that has a supported codec.
        let track = format_reader
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| PlaybackError::DecodeFailed {
                path: file_path.to_string(),
                reason: "no supported audio track found".into(),
            })?;

        let track_id = track.id;

        let sample_rate = track
            .codec_params
            .sample_rate
            .ok_or_else(|| PlaybackError::DecodeFailed {
                path: file_path.to_string(),
                reason: "sample rate unknown".into(),
            })?;

        let channels = track
            .codec_params
            .channels
            .map(|ch| ch.count())
            .unwrap_or(2);

        let total_frames = track.codec_params.n_frames;

        let duration_secs = match total_frames {
            Some(n) if sample_rate > 0 => n as f32 / sample_rate as f32,
            _ => {
                // Fallback: try time_base * n_frames
                track
                    .codec_params
                    .time_base
                    .and_then(|tb| {
                        total_frames.map(|n| {
                            let t = tb.calc_time(n);
                            t.seconds as f32 + t.frac as f32
                        })
                    })
                    .unwrap_or(0.0)
            }
        };

        let decoder_opts = DecoderOptions::default();
        let decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &decoder_opts)
            .map_err(|e| PlaybackError::DecodeFailed {
                path: file_path.to_string(),
                reason: format!("codec init failed: {e}"),
            })?;

        // Extract metadata.
        let mut title = String::new();
        let mut artist = String::new();
        let mut album = String::new();
        let mut cover_art: Option<Vec<u8>> = None;

        // Check metadata from probe result (container-level).
        if let Some(md) = format_reader.metadata().current() {
            Self::extract_tags(md, &mut title, &mut artist, &mut album);
            Self::extract_visuals(md, &mut cover_art);
        }

        // Fallback: filename as title.
        if title.is_empty() {
            title = std::path::Path::new(file_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Unknown")
                .to_string();
        }

        let song_info = SongInfo {
            id: String::new(), // Caller should set this from musico_recommender
            file_path: file_path.to_string(),
            title,
            artist,
            album,
            duration_secs,
            cover_art,
        };

        let dec = Self {
            format_reader,
            decoder,
            track_id,
            sample_rate,
            channels,
            total_frames,
        };

        Ok((dec, song_info))
    }

    /// Decodes the next packet and returns interleaved f32 samples.
    ///
    /// Returns `Ok(None)` when the stream is exhausted (song ended).
    /// Never decodes the entire file at once — one packet per call.
    pub fn decode_next_packet(&mut self) -> Result<Option<Vec<f32>>, PlaybackError> {
        loop {
            let packet = match self.format_reader.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(None); // End of stream
                }
                Err(symphonia::core::errors::Error::ResetRequired) => {
                    // Seek or format change: reset the decoder.
                    self.decoder.reset();
                    continue;
                }
                Err(e) => {
                    return Err(PlaybackError::DecodeFailed {
                        path: String::new(),
                        reason: format!("packet read error: {e}"),
                    });
                }
            };

            // Skip packets from other tracks.
            if packet.track_id() != self.track_id {
                continue;
            }

            match self.decoder.decode(&packet) {
                Ok(decoded) => {
                    let spec = *decoded.spec();
                    let num_frames = decoded.capacity();

                    let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
                    sample_buf.copy_interleaved_ref(decoded);

                    return Ok(Some(sample_buf.samples().to_vec()));
                }
                Err(symphonia::core::errors::Error::DecodeError(msg)) => {
                    // Non-fatal: skip corrupted packet.
                    log::warn!("decode error (skipping packet): {msg}");
                    continue;
                }
                Err(e) => {
                    return Err(PlaybackError::DecodeFailed {
                        path: String::new(),
                        reason: format!("decode error: {e}"),
                    });
                }
            }
        }
    }

    /// Seeks to the given position in seconds.
    ///
    /// Uses accurate seeking for precise position.
    pub fn seek_to(&mut self, secs: f32) -> Result<(), PlaybackError> {
        let seek_to = SeekTo::Time {
            time: Time::new(secs as u64, (secs.fract()) as f64),
            track_id: Some(self.track_id),
        };

        self.format_reader
            .seek(SeekMode::Accurate, seek_to)
            .map_err(|e| PlaybackError::DecodeFailed {
                path: String::new(),
                reason: format!("seek failed: {e}"),
            })?;

        // Reset decoder state after seek.
        self.decoder.reset();

        Ok(())
    }

    /// Returns the source audio sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Returns the number of audio channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Returns the total number of frames, if known.
    pub fn total_frames(&self) -> Option<u64> {
        self.total_frames
    }

    // ── private helpers ──────────────────────────────────────────────

    fn extract_tags(
        md: &symphonia::core::meta::MetadataRevision,
        title: &mut String,
        artist: &mut String,
        album: &mut String,
    ) {
        for tag in md.tags() {
            if let Some(key) = tag.std_key {
                match key {
                    StandardTagKey::TrackTitle => {
                        *title = tag.value.to_string();
                    }
                    StandardTagKey::Artist | StandardTagKey::AlbumArtist => {
                        if artist.is_empty() {
                            *artist = tag.value.to_string();
                        }
                    }
                    StandardTagKey::Album => {
                        *album = tag.value.to_string();
                    }
                    _ => {}
                }
            }
        }
    }

    fn extract_visuals(
        md: &symphonia::core::meta::MetadataRevision,
        cover_art: &mut Option<Vec<u8>>,
    ) {
        for visual in md.visuals() {
            // Prefer front cover, but accept anything.
            *cover_art = Some(visual.data.to_vec());
            break;
        }
    }
}
