//! Audio feature extraction — single-pass decode + metadata + feature computation.
//!
//! Decodes only the first 60 seconds of audio (sufficient for statistical
//! convergence of all features).  Mel filterbank and Hamming window are
//! computed once via `LazyLock`.

use std::f32::consts::PI;
use std::path::Path;
use std::sync::LazyLock;

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::errors::RecommenderError;
use crate::models::{AnalysisResult, FeatureVector};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const TARGET_SR: f32 = 22050.0;
const PRE_EMPH: f32 = 0.97;
const FRAME_LEN: usize = 551;
const HOP_LEN: usize = 221;
const N_MELS: usize = 26;
const N_MFCC: usize = 13;
const MEL_LOW: f32 = 0.0;
const MEL_HIGH: f32 = 8000.0;
const FFT_SIZE: usize = 1024;
/// Maximum samples to decode (60 s at 22 050 Hz).
const MAX_ANALYSIS_SAMPLES: usize = 60 * 22050;

// ---------------------------------------------------------------------------
// Static precomputed tables (LazyLock — computed once, zero cost thereafter)
// ---------------------------------------------------------------------------

static MEL_FILTERS: LazyLock<Vec<Vec<f32>>> = LazyLock::new(|| build_mel_filterbank(FFT_SIZE / 2 + 1));
static HAMMING: LazyLock<Vec<f32>> = LazyLock::new(|| {
    (0..FRAME_LEN)
        .map(|i| 0.54 - 0.46 * (2.0 * PI * i as f32 / (FRAME_LEN as f32 - 1.0)).cos())
        .collect()
});

// ---------------------------------------------------------------------------
// Public API — single-pass analysis
// ---------------------------------------------------------------------------

/// Analyses an audio file in a single pass: probes for metadata, decodes the
/// first 60 seconds to PCM, and extracts all features.
///
/// Returns an [`AnalysisResult`] containing the feature vector, duration, and
/// metadata tags.
pub fn analyze_file(path: &str) -> Result<AnalysisResult, RecommenderError> {
    let file_path = Path::new(path);
    let fallback_title = file_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Unknown".into());

    let file = std::fs::File::open(path).map_err(RecommenderError::IoError)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = file_path.extension() {
        hint.with_extension(&ext.to_string_lossy());
    }

    let mut probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| RecommenderError::DecodeError(format!("Probe failed: {e}")))?;

    // ---- Metadata (from the same probe, no second file open) ----
    let mut title = String::new();
    let mut artist = String::new();
    let mut album = String::new();

    if let Some(metadata) = probed.metadata.get() {
        if let Some(rev) = metadata.current() {
            for tag in rev.tags() {
                if let Some(std_key) = &tag.std_key {
                    use symphonia::core::meta::StandardTagKey::*;
                    match std_key {
                        TrackTitle => title = tag.value.to_string(),
                        Artist => artist = tag.value.to_string(),
                        Album => album = tag.value.to_string(),
                        _ => {}
                    }
                }
            }
        }
    }

    // Also check format-level metadata (some containers store tags there).
    if title.is_empty() || artist.is_empty() || album.is_empty() {
        let fmt_meta = probed.format.metadata();
        if let Some(rev) = fmt_meta.current() {
            for tag in rev.tags() {
                if let Some(std_key) = &tag.std_key {
                    use symphonia::core::meta::StandardTagKey::*;
                    match std_key {
                        TrackTitle if title.is_empty() => title = tag.value.to_string(),
                        Artist if artist.is_empty() => artist = tag.value.to_string(),
                        Album if album.is_empty() => album = tag.value.to_string(),
                        _ => {}
                    }
                }
            }
        }
    }

    if title.is_empty() {
        title = fallback_title;
    }

    // ---- Decode first 60 s to mono PCM ----
    let format = &mut probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| RecommenderError::DecodeError("No audio track found".into()))?;

    let track_id = track.id;
    let codec_params = track.codec_params.clone();
    let source_sr = codec_params.sample_rate.unwrap_or(44100) as f32;
    let n_channels = codec_params.channels.map(|c| c.count()).unwrap_or(2) as f32;

    // Compute full duration from codec params if available.
    let full_duration_secs = codec_params
        .n_frames
        .map(|n| (n as f32 / source_sr) as u32)
        .unwrap_or(0);

    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| RecommenderError::DecodeError(format!("Codec init failed: {e}")))?;

    // Target number of source samples corresponding to 60 s at source SR.
    let max_source_samples = (60.0 * source_sr) as usize;
    let mut all_samples: Vec<f32> = Vec::with_capacity(max_source_samples);

    loop {
        if all_samples.len() >= max_source_samples {
            break;
        }

        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(_) => continue,
        };

        let spec = *decoded.spec();
        let num_frames = decoded.capacity();
        let mut sample_buf = SampleBuffer::<f32>::new(num_frames as u64, spec);
        sample_buf.copy_interleaved_ref(decoded);

        let ch = spec.channels.count() as usize;
        for frame_samples in sample_buf.samples().chunks(ch) {
            let mono: f32 = frame_samples.iter().sum::<f32>() / n_channels;
            all_samples.push(mono);
        }
    }

    // Resample to TARGET_SR if necessary.
    if (source_sr - TARGET_SR).abs() > 1.0 {
        all_samples = resample_linear(&all_samples, source_sr, TARGET_SR);
    }

    // Cap at MAX_ANALYSIS_SAMPLES after resampling.
    all_samples.truncate(MAX_ANALYSIS_SAMPLES);

    if all_samples.len() < FRAME_LEN {
        return Err(RecommenderError::DecodeError("Audio too short for analysis".into()));
    }

    // Use decoded length for duration if codec didn't provide n_frames.
    let duration_secs = if full_duration_secs > 0 {
        full_duration_secs
    } else {
        (all_samples.len() as f32 / TARGET_SR) as u32
    };

    // ---- Feature extraction ----
    let emphasised = pre_emphasis(&all_samples);

    let frames = frame_and_window(&emphasised);
    if frames.is_empty() {
        return Err(RecommenderError::DecodeError("No frames produced".into()));
    }

    let power_spectra = compute_power_spectra(&frames);
    let mel_spectra = apply_mel_filterbank(&power_spectra, &MEL_FILTERS);
    let mfcc = compute_mfcc(&mel_spectra);
    let spectral_centroid = compute_spectral_centroid(&power_spectra);
    let spectral_rolloff = compute_spectral_rolloff(&power_spectra, 0.85);
    let zero_crossing_rate = compute_zcr(&emphasised);
    let rms_energy = compute_rms(&all_samples);
    let tempo_bpm = estimate_tempo(&power_spectra);
    let chroma = compute_chroma(&power_spectra);

    let mut fv = FeatureVector {
        version: FeatureVector::CURRENT_VERSION,
        mfcc,
        spectral_centroid,
        spectral_rolloff,
        zero_crossing_rate,
        rms_energy,
        tempo_bpm,
        chroma,
    };
    normalise_feature_vector(&mut fv);

    // Compute RMS in dB for ReplayGain normalization.
    let rms_db = if rms_energy > 1e-10 {
        20.0 * rms_energy.log10()
    } else {
        -96.0 // silence floor
    };

    Ok(AnalysisResult { feature_vector: fv, duration_secs, title, artist, album, rms_db })
}

// ---------------------------------------------------------------------------
// Resampling
// ---------------------------------------------------------------------------

fn resample_linear(input: &[f32], src_sr: f32, dst_sr: f32) -> Vec<f32> {
    let ratio = src_sr / dst_sr;
    let out_len = (input.len() as f32 / ratio) as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_idx = i as f32 * ratio;
        let idx0 = src_idx as usize;
        let frac = src_idx - idx0 as f32;
        let s0 = input[idx0.min(input.len() - 1)];
        let s1 = input[(idx0 + 1).min(input.len() - 1)];
        output.push(s0 + frac * (s1 - s0));
    }
    output
}

// ---------------------------------------------------------------------------
// Pre-emphasis, framing, windowing
// ---------------------------------------------------------------------------

fn pre_emphasis(samples: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(samples.len());
    out.push(samples[0]);
    for i in 1..samples.len() {
        out.push(samples[i] - PRE_EMPH * samples[i - 1]);
    }
    out
}

fn frame_and_window(samples: &[f32]) -> Vec<Vec<f32>> {
    let window = &*HAMMING;
    let mut frames = Vec::new();
    let mut start = 0;
    while start + FRAME_LEN <= samples.len() {
        let frame: Vec<f32> = samples[start..start + FRAME_LEN]
            .iter()
            .zip(window.iter())
            .map(|(s, w)| s * w)
            .collect();
        frames.push(frame);
        start += HOP_LEN;
    }
    frames
}

// ---------------------------------------------------------------------------
// FFT & power spectrum
// ---------------------------------------------------------------------------

fn compute_power_spectra(frames: &[Vec<f32>]) -> Vec<Vec<f32>> {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    frames
        .iter()
        .map(|frame| {
            let mut buffer: Vec<Complex<f32>> =
                frame.iter().map(|&s| Complex::new(s, 0.0)).collect();
            buffer.resize(FFT_SIZE, Complex::new(0.0, 0.0));
            fft.process(&mut buffer);
            buffer[..FFT_SIZE / 2 + 1]
                .iter()
                .map(|c| c.norm_sqr() / FFT_SIZE as f32)
                .collect()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Mel filterbank + MFCC
// ---------------------------------------------------------------------------

fn hz_to_mel(hz: f32) -> f32 { 2595.0 * (1.0 + hz / 700.0).log10() }
fn mel_to_hz(mel: f32) -> f32 { 700.0 * (10.0_f32.powf(mel / 2595.0) - 1.0) }

fn build_mel_filterbank(n_fft_bins: usize) -> Vec<Vec<f32>> {
    let mel_low = hz_to_mel(MEL_LOW);
    let mel_high = hz_to_mel(MEL_HIGH);
    let mel_points: Vec<f32> = (0..N_MELS + 2)
        .map(|i| mel_low + (mel_high - mel_low) * i as f32 / (N_MELS + 1) as f32)
        .collect();
    let hz_points: Vec<f32> = mel_points.iter().map(|&m| mel_to_hz(m)).collect();
    let bin_points: Vec<usize> = hz_points
        .iter()
        .map(|&h| ((h / (TARGET_SR / 2.0)) * (n_fft_bins - 1) as f32) as usize)
        .collect();

    let mut filters = Vec::with_capacity(N_MELS);
    for m in 0..N_MELS {
        let mut filt = vec![0.0_f32; n_fft_bins];
        let start = bin_points[m];
        let center = bin_points[m + 1];
        let end = bin_points[m + 2];
        for k in start..center {
            if center > start { filt[k] = (k - start) as f32 / (center - start) as f32; }
        }
        for k in center..end {
            if end > center { filt[k] = (end - k) as f32 / (end - center) as f32; }
        }
        filters.push(filt);
    }
    filters
}

fn apply_mel_filterbank(power_spectra: &[Vec<f32>], filters: &[Vec<f32>]) -> Vec<Vec<f32>> {
    power_spectra
        .iter()
        .map(|spectrum| {
            filters
                .iter()
                .map(|filt| {
                    let energy: f32 = spectrum.iter().zip(filt.iter()).map(|(s, f)| s * f).sum();
                    (energy.max(1e-10)).ln()
                })
                .collect()
        })
        .collect()
}

fn compute_mfcc(mel_spectra: &[Vec<f32>]) -> [f32; N_MFCC] {
    let n_frames = mel_spectra.len();
    let mut avg = [0.0_f32; N_MFCC];
    for frame_mel in mel_spectra {
        let n = frame_mel.len() as f32;
        for i in 0..N_MFCC {
            let mut coeff = 0.0_f32;
            for (j, &mel_val) in frame_mel.iter().enumerate() {
                coeff += mel_val * (PI * (i as f32) * (j as f32 + 0.5) / n).cos();
            }
            avg[i] += coeff;
        }
    }
    for v in avg.iter_mut() { *v /= n_frames as f32; }
    avg
}

// ---------------------------------------------------------------------------
// Spectral features
// ---------------------------------------------------------------------------

fn compute_spectral_centroid(power_spectra: &[Vec<f32>]) -> f32 {
    let mut total = 0.0_f64;
    let n = power_spectra.len() as f64;
    for spectrum in power_spectra {
        let sum_mag: f64 = spectrum.iter().map(|&s| s as f64).sum();
        if sum_mag < 1e-12 { continue; }
        let weighted: f64 = spectrum.iter().enumerate().map(|(i, &s)| i as f64 * s as f64).sum();
        total += weighted / sum_mag;
    }
    (total / n) as f32
}

fn compute_spectral_rolloff(power_spectra: &[Vec<f32>], threshold: f32) -> f32 {
    let mut total = 0.0_f64;
    let n = power_spectra.len() as f64;
    for spectrum in power_spectra {
        let energy_total: f64 = spectrum.iter().map(|&s| s as f64).sum();
        let target = energy_total * threshold as f64;
        let mut cumulative = 0.0_f64;
        let mut rolloff_bin = 0;
        for (i, &s) in spectrum.iter().enumerate() {
            cumulative += s as f64;
            if cumulative >= target { rolloff_bin = i; break; }
        }
        total += rolloff_bin as f64;
    }
    (total / n) as f32
}

fn compute_zcr(samples: &[f32]) -> f32 {
    if samples.len() < 2 { return 0.0; }
    let crossings: usize = samples.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
    crossings as f32 / (samples.len() - 1) as f32
}

fn compute_rms(samples: &[f32]) -> f32 {
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}

// ---------------------------------------------------------------------------
// Tempo estimation
// ---------------------------------------------------------------------------

fn estimate_tempo(power_spectra: &[Vec<f32>]) -> f32 {
    let mut onset_env = Vec::with_capacity(power_spectra.len());
    onset_env.push(0.0_f32);
    for i in 1..power_spectra.len() {
        let flux: f32 = power_spectra[i]
            .iter()
            .zip(power_spectra[i - 1].iter())
            .map(|(&cur, &prev)| (cur - prev).max(0.0))
            .sum();
        onset_env.push(flux);
    }

    let max_lag = onset_env.len().min(((TARGET_SR / HOP_LEN as f32) * 60.0 / 60.0) as usize + 1);
    let min_lag = ((TARGET_SR / HOP_LEN as f32) * 60.0 / 200.0) as usize;

    if max_lag <= min_lag || max_lag > onset_env.len() { return 120.0; }

    let mut autocorr = vec![0.0_f32; max_lag];
    for lag in min_lag..max_lag {
        let mut sum = 0.0_f32;
        for i in 0..onset_env.len() - lag { sum += onset_env[i] * onset_env[i + lag]; }
        autocorr[lag] = sum;
    }

    let mut best_lag = min_lag;
    let mut best_val = f32::NEG_INFINITY;
    for lag in min_lag..max_lag {
        if autocorr[lag] > best_val { best_val = autocorr[lag]; best_lag = lag; }
    }
    if best_lag == 0 { return 120.0; }

    let bpm = 60.0 * (TARGET_SR / HOP_LEN as f32) / best_lag as f32;
    bpm.clamp(60.0, 200.0)
}

// ---------------------------------------------------------------------------
// Chroma
// ---------------------------------------------------------------------------

fn compute_chroma(power_spectra: &[Vec<f32>]) -> [f32; 12] {
    let n_bins = FFT_SIZE / 2 + 1;
    let bin_to_chroma: Vec<Option<usize>> = (0..n_bins)
        .map(|k| {
            if k == 0 { return None; }
            let freq = k as f32 * TARGET_SR / FFT_SIZE as f32;
            if freq < 20.0 || freq > TARGET_SR / 2.0 { return None; }
            let midi = 12.0 * (freq / 440.0).log2() + 69.0;
            Some(((midi.round() as i32) % 12).rem_euclid(12) as usize)
        })
        .collect();

    let mut chroma = [0.0_f32; 12];
    let n_frames = power_spectra.len() as f32;
    for spectrum in power_spectra {
        for (k, &mag) in spectrum.iter().enumerate() {
            if let Some(ci) = bin_to_chroma[k] { chroma[ci] += mag; }
        }
    }
    for c in chroma.iter_mut() { *c /= n_frames; }
    let sum: f32 = chroma.iter().sum();
    if sum > 1e-10 { for c in chroma.iter_mut() { *c /= sum; } }
    chroma
}

// ---------------------------------------------------------------------------
// Normalisation
// ---------------------------------------------------------------------------

fn normalise_feature_vector(fv: &mut FeatureVector) {
    for c in fv.mfcc.iter_mut() { *c = ((*c + 50.0) / 100.0).clamp(0.0, 1.0); }
    fv.spectral_centroid = (fv.spectral_centroid / (FFT_SIZE as f32 / 2.0)).clamp(0.0, 1.0);
    fv.spectral_rolloff = (fv.spectral_rolloff / (FFT_SIZE as f32 / 2.0)).clamp(0.0, 1.0);
    fv.zero_crossing_rate = fv.zero_crossing_rate.clamp(0.0, 1.0);
    fv.rms_energy = fv.rms_energy.clamp(0.0, 1.0);
    fv.tempo_bpm = ((fv.tempo_bpm - 60.0) / 140.0).clamp(0.0, 1.0);
    for c in fv.chroma.iter_mut() { *c = c.clamp(0.0, 1.0); }
}
