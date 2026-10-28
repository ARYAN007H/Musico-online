//! Crossfade mixer for gapless transitions between songs.
//!
//! Operates entirely on interleaved f32 sample buffers.
//! Zero allocation on the hot path — uses pre-computed gain tables.

/// Crossfade curve type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CrossfadeCurve {
    /// Linear: out_gain = 1 - t, in_gain = t
    Linear,
    /// Equal power: out_gain = cos(t * π/2), in_gain = sin(t * π/2)
    EqualPower,
    /// Overlap-only: both streams at full volume, just overlap
    Overlap,
}

impl CrossfadeCurve {
    pub fn id(&self) -> &str {
        match self {
            Self::Linear => "linear",
            Self::EqualPower => "equal_power",
            Self::Overlap => "overlap",
        }
    }
    pub fn from_id(id: &str) -> Self {
        match id {
            "equal_power" => Self::EqualPower,
            "overlap" => Self::Overlap,
            _ => Self::Linear,
        }
    }
    pub fn label(&self) -> &str {
        match self {
            Self::Linear => "Linear",
            Self::EqualPower => "Equal Power",
            Self::Overlap => "Overlap",
        }
    }
}

/// Configuration for crossfade.
#[derive(Debug, Clone, Copy)]
pub struct CrossfadeConfig {
    /// Duration of the crossfade in seconds (0 = gapless with no fade).
    pub duration_secs: f32,
    /// Curve type for gain interpolation.
    pub curve: CrossfadeCurve,
    /// Whether crossfade is enabled at all.
    pub enabled: bool,
}

impl Default for CrossfadeConfig {
    fn default() -> Self {
        Self {
            duration_secs: 3.0,
            curve: CrossfadeCurve::EqualPower,
            enabled: false,
        }
    }
}

/// Crossfade mixer state — lives in the decoder thread.
pub struct CrossfadeMixer {
    /// Config (may be updated at any time).
    pub config: CrossfadeConfig,
    /// Pre-decoded "next song" samples buffer.
    next_buf: Vec<f32>,
    /// How many samples of crossfade we've consumed so far.
    crossfade_pos: usize,
    /// Total number of samples in the crossfade region.
    crossfade_total: usize,
    /// Whether we're actively in a crossfade.
    active: bool,
}

impl CrossfadeMixer {
    pub fn new(config: CrossfadeConfig) -> Self {
        Self {
            config,
            next_buf: Vec::new(),
            crossfade_pos: 0,
            crossfade_total: 0,
            active: false,
        }
    }

    /// Start a crossfade from the pre-loaded next-song buffer.
    /// `next_samples` = the first N samples of the next song (pre-decoded).
    /// `sample_rate` = device sample rate, `channels` = device channel count.
    pub fn begin_crossfade(&mut self, next_samples: Vec<f32>, sample_rate: u32, channels: usize) {
        if !self.config.enabled || self.config.duration_secs <= 0.0 {
            // No crossfade — just buffer the samples for gapless handoff.
            self.next_buf = next_samples;
            self.active = false;
            return;
        }

        let total_frames = (self.config.duration_secs * sample_rate as f32) as usize;
        self.crossfade_total = total_frames * channels;
        self.crossfade_pos = 0;
        self.next_buf = next_samples;
        self.active = true;
    }

    /// Mix crossfade into the current buffer (in-place).
    /// Returns how many samples of the next-song buffer were consumed.
    pub fn mix_into(&mut self, current_buf: &mut [f32], channels: usize) -> usize {
        if !self.active || self.next_buf.is_empty() {
            return 0;
        }

        let remaining = self.crossfade_total.saturating_sub(self.crossfade_pos);
        let mix_len = current_buf.len().min(remaining).min(self.next_buf.len());

        if mix_len == 0 {
            self.active = false;
            return 0;
        }

        for i in 0..mix_len {
            let t = (self.crossfade_pos + i) as f32 / self.crossfade_total as f32;
            let (out_gain, in_gain) = match self.config.curve {
                CrossfadeCurve::Linear => (1.0 - t, t),
                CrossfadeCurve::EqualPower => {
                    let half_pi = std::f32::consts::FRAC_PI_2;
                    ((t * half_pi).cos(), (t * half_pi).sin())
                }
                CrossfadeCurve::Overlap => (1.0, 1.0),
            };

            current_buf[i] = current_buf[i] * out_gain + self.next_buf[i] * in_gain;
        }

        self.crossfade_pos += mix_len;
        // Remove consumed samples from the front of next_buf.
        self.next_buf.drain(..mix_len);

        if self.crossfade_pos >= self.crossfade_total {
            self.active = false;
        }

        mix_len
    }

    /// Check if crossfade is currently active.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Get the remaining next-song samples after crossfade completes.
    pub fn take_remaining(&mut self) -> Vec<f32> {
        std::mem::take(&mut self.next_buf)
    }

    /// Reset crossfade state (e.g., on seek or stop).
    pub fn reset(&mut self) {
        self.next_buf.clear();
        self.crossfade_pos = 0;
        self.crossfade_total = 0;
        self.active = false;
    }
}
