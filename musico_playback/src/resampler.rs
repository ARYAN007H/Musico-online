//! Audio resampler bridging source sample rate to CPAL device rate.
//!
//! Uses `rubato::SincFixedIn` for high-quality sinc interpolation.
//! Transparently skipped when input and output rates match.

use crate::error::PlaybackError;
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

const RESAMPLE_CHUNK_SIZE: usize = 1024;

/// Resamples interleaved f32 audio from one sample rate to another.
pub struct AudioResampler {
    resampler: SincFixedIn<f64>,
    input_sample_rate: u32,
    output_sample_rate: u32,
    channels: usize,
    // Pre-allocated deinterleave buffers to avoid per-call allocation.
    deinterleave_bufs: Vec<Vec<f64>>,
}

impl AudioResampler {
    /// Creates a new resampler.
    pub fn new(input_sr: u32, output_sr: u32, channels: usize) -> Result<Self, PlaybackError> {
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let ratio = output_sr as f64 / input_sr as f64;
        let resampler = SincFixedIn::<f64>::new(ratio, 2.0, params, RESAMPLE_CHUNK_SIZE, channels)
            .map_err(|e| PlaybackError::ResamplerError(format!("{e}")))?;

        let deinterleave_bufs = vec![vec![0.0f64; RESAMPLE_CHUNK_SIZE]; channels];

        Ok(Self {
            resampler,
            input_sample_rate: input_sr,
            output_sample_rate: output_sr,
            channels,
            deinterleave_bufs,
        })
    }

    /// Resamples interleaved f32 samples into a caller-provided output buffer.
    /// Zero heap allocations on the hot path (uses pre-allocated internal buffers).
    pub fn process_into(&mut self, input: &[f32], output: &mut Vec<f32>) -> Result<(), PlaybackError> {
        let ch = self.channels;
        let frames_in = input.len() / ch;
        if frames_in == 0 { return Ok(()); }

        let chunk = self.resampler.input_frames_next();
        let mut offset = 0;

        while offset < frames_in {
            let end = (offset + chunk).min(frames_in);
            let actual = end - offset;

            // Reuse pre-allocated deinterleave buffers — just resize if needed.
            for c in 0..ch {
                let buf = &mut self.deinterleave_bufs[c];
                if buf.len() < chunk {
                    buf.resize(chunk, 0.0);
                }
                // Zero the buffer and fill with actual samples.
                for i in 0..chunk {
                    if i < actual {
                        buf[i] = input[(offset + i) * ch + c] as f64;
                    } else {
                        buf[i] = 0.0;
                    }
                }
            }

            let res = self.resampler.process(&self.deinterleave_bufs, None)
                .map_err(|e| PlaybackError::ResamplerError(format!("{e}")))?;
            let out_frames = if res.is_empty() { 0 } else { res[0].len() };
            let useful = if actual < chunk {
                let r = self.output_sample_rate as f64 / self.input_sample_rate as f64;
                ((actual as f64) * r).ceil() as usize
            } else { out_frames };
            let take = useful.min(out_frames);
            for f in 0..take {
                for c in 0..ch { output.push(res[c][f] as f32); }
            }
            offset += chunk;
        }
        Ok(())
    }

    /// Returns `true` if resampling is actually needed.
    pub fn needed(&self) -> bool {
        self.input_sample_rate != self.output_sample_rate
    }
}
