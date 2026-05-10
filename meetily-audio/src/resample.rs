//! Audio resampling: 48 kHz (typical cpal capture rate) → 16 kHz mono f32
//! frames suitable for Silero VAD and Whisper.
//!
//! Uses rubato's `SincFixedIn` resampler with a fixed input chunk size so we
//! get back a deterministic output frame count. The output is sliced into
//! fixed 30 ms / 480-sample frames so downstream VAD calls stay aligned.

use anyhow::{Context, Result};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

use crate::vad::{VAD_FRAME_SAMPLES, VAD_SAMPLE_RATE};

/// Number of input samples (per channel) we feed to rubato per call.
/// 1440 input samples @ 48 kHz = 30 ms = 480 output samples @ 16 kHz.
const RUBATO_CHUNK_IN: usize = 1440;

/// Resamples interleaved-or-mono cpal input at `input_rate` Hz down to
/// 16 kHz mono f32, accumulating output and yielding 30 ms VAD frames.
pub struct Resampler16k {
    input_rate: u32,
    input_channels: u16,
    /// Mono input buffer at `input_rate`, flushed in `RUBATO_CHUNK_IN` chunks.
    mono_input: Vec<f32>,
    /// 16 kHz mono output buffer, sliced into `VAD_FRAME_SAMPLES` frames.
    output_buf: Vec<f32>,
    resampler: SincFixedIn<f32>,
}

impl Resampler16k {
    /// Construct a 16 kHz resampler for cpal input at `input_rate` Hz with
    /// `input_channels` interleaved channels (downmixed to mono).
    pub fn new(input_rate: u32, input_channels: u16) -> Result<Self> {
        anyhow::ensure!(input_channels >= 1, "input_channels must be >= 1");
        let params = SincInterpolationParameters {
            sinc_len: 128,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 128,
            window: WindowFunction::BlackmanHarris2,
        };
        let resampler = SincFixedIn::<f32>::new(
            VAD_SAMPLE_RATE as f64 / input_rate as f64,
            2.0,
            params,
            RUBATO_CHUNK_IN,
            1,
        )
        .context("failed to initialize rubato resampler")?;
        Ok(Self {
            input_rate,
            input_channels,
            mono_input: Vec::with_capacity(RUBATO_CHUNK_IN * 2),
            output_buf: Vec::with_capacity(VAD_FRAME_SAMPLES * 4),
            resampler,
        })
    }

    /// Feed cpal samples (interleaved if multi-channel) and pop any complete
    /// 30 ms / 480-sample @ 16 kHz frames produced.
    pub fn push(&mut self, samples: &[f32]) -> Result<Vec<Vec<f32>>> {
        // Downmix to mono.
        if self.input_channels == 1 {
            self.mono_input.extend_from_slice(samples);
        } else {
            let ch = self.input_channels as usize;
            for chunk in samples.chunks_exact(ch) {
                let sum: f32 = chunk.iter().sum();
                self.mono_input.push(sum / ch as f32);
            }
        }

        // Pump fixed-size chunks through the resampler.
        let mut frames = Vec::new();
        while self.mono_input.len() >= RUBATO_CHUNK_IN {
            let chunk: Vec<f32> = self.mono_input.drain(..RUBATO_CHUNK_IN).collect();
            let input = vec![chunk];
            let resampled = self
                .resampler
                .process(&input, None)
                .context("rubato process failed")?;
            self.output_buf.extend_from_slice(&resampled[0]);

            while self.output_buf.len() >= VAD_FRAME_SAMPLES {
                let frame: Vec<f32> = self.output_buf.drain(..VAD_FRAME_SAMPLES).collect();
                frames.push(frame);
            }
        }
        Ok(frames)
    }

    /// Input sample rate this resampler was constructed for.
    pub fn input_rate(&self) -> u32 {
        self.input_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampler_initializes_48k_mono() {
        assert!(Resampler16k::new(48_000, 1).is_ok());
    }

    #[test]
    fn resampler_initializes_48k_stereo() {
        assert!(Resampler16k::new(48_000, 2).is_ok());
    }

    #[test]
    fn resampler_produces_480_sample_frames() {
        let mut r = Resampler16k::new(48_000, 1).unwrap();
        // 48000 samples = 1 second of input -> ~16000 samples out -> ~33 frames of 480.
        let input = vec![0.0f32; 48_000];
        let frames = r.push(&input).unwrap();
        assert!(frames.len() >= 30, "expected ~33 frames, got {}", frames.len());
        for f in &frames {
            assert_eq!(f.len(), VAD_FRAME_SAMPLES);
        }
    }
}
