//! 10-band parametric equalizer using second-order IIR (biquad) filters.
//!
//! Each band is a peaking EQ filter at an ISO standard center frequency.
//! Filter state is stack-allocated — zero heap allocations per sample.

use serde::{Deserialize, Serialize};

/// ISO standard center frequencies for 10-band EQ.
pub const BAND_FREQS: [f32; 10] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0, 16000.0,
];

pub const BAND_LABELS: [&str; 10] = [
    "31", "62", "125", "250", "500", "1K", "2K", "4K", "8K", "16K",
];

/// A single second-order biquad filter.
#[derive(Debug, Clone)]
struct Biquad {
    b0: f32, b1: f32, b2: f32,
    a1: f32, a2: f32,
    // Per-channel delay lines (stereo max).
    z1: [f32; 2],
    z2: [f32; 2],
}

impl Biquad {
    fn new() -> Self {
        Self {
            b0: 1.0, b1: 0.0, b2: 0.0,
            a1: 0.0, a2: 0.0,
            z1: [0.0; 2], z2: [0.0; 2],
        }
    }

    /// Compute peaking EQ coefficients.
    /// `freq` = center frequency, `gain_db` = boost/cut in dB, `q` = quality factor.
    fn set_peaking(&mut self, freq: f32, gain_db: f32, q: f32, sample_rate: f32) {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = 2.0 * std::f32::consts::PI * freq / sample_rate;
        let sin_w0 = w0.sin();
        let cos_w0 = w0.cos();
        let alpha = sin_w0 / (2.0 * q);

        let a0 = 1.0 + alpha / a;
        self.b0 = (1.0 + alpha * a) / a0;
        self.b1 = (-2.0 * cos_w0) / a0;
        self.b2 = (1.0 - alpha * a) / a0;
        self.a1 = (-2.0 * cos_w0) / a0;
        self.a2 = (1.0 - alpha / a) / a0;
    }

    /// Process a single sample (transposed direct form II).
    #[inline(always)]
    fn process(&mut self, input: f32, ch: usize) -> f32 {
        let output = self.b0 * input + self.z1[ch];
        self.z1[ch] = self.b1 * input - self.a1 * output + self.z2[ch];
        self.z2[ch] = self.b2 * input - self.a2 * output;
        output
    }

    fn reset(&mut self) {
        self.z1 = [0.0; 2];
        self.z2 = [0.0; 2];
    }
}

/// EQ preset definition: 10 gain values in dB.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EQPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub gains: [f32; 10],
}

pub const PRESET_FLAT: EQPreset = EQPreset {
    id: "flat", name: "Flat",
    gains: [0.0; 10],
};
pub const PRESET_BASS_BOOST: EQPreset = EQPreset {
    id: "bass_boost", name: "Bass Boost",
    gains: [8.0, 6.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
};
pub const PRESET_TREBLE_BOOST: EQPreset = EQPreset {
    id: "treble_boost", name: "Treble Boost",
    gains: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 4.0, 6.0, 8.0],
};
pub const PRESET_VOCAL: EQPreset = EQPreset {
    id: "vocal", name: "Vocal",
    gains: [-2.0, -1.0, 0.0, 2.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0],
};
pub const PRESET_ACOUSTIC: EQPreset = EQPreset {
    id: "acoustic", name: "Acoustic",
    gains: [3.0, 2.0, 0.0, 1.0, 2.0, 2.0, 3.0, 4.0, 3.0, 1.0],
};
pub const PRESET_ELECTRONIC: EQPreset = EQPreset {
    id: "electronic", name: "Electronic",
    gains: [6.0, 5.0, 3.0, 0.0, -2.0, 0.0, 2.0, 4.0, 5.0, 6.0],
};
pub const PRESET_CLASSICAL: EQPreset = EQPreset {
    id: "classical", name: "Classical",
    gains: [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -2.0, -2.0, -2.0, -4.0],
};
pub const PRESET_ROCK: EQPreset = EQPreset {
    id: "rock", name: "Rock",
    gains: [5.0, 4.0, 2.0, 0.0, -1.0, 0.0, 2.0, 3.0, 4.0, 5.0],
};
pub const PRESET_HIP_HOP: EQPreset = EQPreset {
    id: "hip_hop", name: "Hip Hop",
    gains: [6.0, 5.0, 3.0, 1.0, 0.0, 0.0, 1.0, 0.0, 2.0, 3.0],
};
pub const PRESET_JAZZ: EQPreset = EQPreset {
    id: "jazz", name: "Jazz",
    gains: [2.0, 1.0, 0.0, 2.0, 3.0, 3.0, 2.0, 1.0, 2.0, 3.0],
};
pub const PRESET_LATE_NIGHT: EQPreset = EQPreset {
    id: "late_night", name: "Late Night",
    gains: [4.0, 3.0, 2.0, 1.0, 0.0, 0.0, -1.0, -2.0, -3.0, -4.0],
};
pub const PRESET_LOUDNESS: EQPreset = EQPreset {
    id: "loudness", name: "Loudness",
    gains: [6.0, 4.0, 0.0, -2.0, -1.0, 0.0, -1.0, -2.0, 4.0, 6.0],
};

pub const ALL_PRESETS: &[EQPreset] = &[
    PRESET_FLAT, PRESET_BASS_BOOST, PRESET_TREBLE_BOOST, PRESET_VOCAL,
    PRESET_ACOUSTIC, PRESET_ELECTRONIC, PRESET_CLASSICAL, PRESET_ROCK,
    PRESET_HIP_HOP, PRESET_JAZZ, PRESET_LATE_NIGHT, PRESET_LOUDNESS,
];

pub fn preset_by_id(id: &str) -> EQPreset {
    ALL_PRESETS.iter().find(|p| p.id == id).cloned().unwrap_or(PRESET_FLAT)
}

/// The 10-band equalizer processor.
///
/// Call `set_gains()` to update bands, then `process_interleaved()` on the
/// sample buffer in the decode loop. All state is inline — zero allocations.
pub struct Equalizer {
    bands: [Biquad; 10],
    gains_db: [f32; 10],
    enabled: bool,
    sample_rate: f32,
    channels: usize,
}

impl Equalizer {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        Self {
            bands: std::array::from_fn(|_| Biquad::new()),
            gains_db: [0.0; 10],
            enabled: false,
            sample_rate: sample_rate as f32,
            channels: channels.min(2),
        }
    }

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            for band in &mut self.bands {
                band.reset();
            }
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Update all 10 band gains (in dB). Recomputes biquad coefficients.
    pub fn set_gains(&mut self, gains_db: [f32; 10]) {
        self.gains_db = gains_db;
        let q = 1.41; // Butterworth Q for gentle overlap
        for (i, band) in self.bands.iter_mut().enumerate() {
            band.set_peaking(BAND_FREQS[i], gains_db[i], q, self.sample_rate);
        }
    }

    /// Apply a preset.
    pub fn set_preset(&mut self, preset: &EQPreset) {
        self.set_gains(preset.gains);
        self.enabled = preset.id != "flat";
    }

    pub fn gains(&self) -> &[f32; 10] {
        &self.gains_db
    }

    /// Process interleaved samples in-place. Zero-alloc.
    #[inline]
    pub fn process_interleaved(&mut self, samples: &mut [f32]) {
        if !self.enabled {
            return;
        }
        let ch = self.channels;
        for frame in samples.chunks_exact_mut(ch) {
            for (c, sample) in frame.iter_mut().enumerate().take(ch) {
                for band in &mut self.bands {
                    *sample = band.process(*sample, c);
                }
            }
        }
    }
}
