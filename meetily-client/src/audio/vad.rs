//! Silero VAD wrapper for streaming utterance detection.
//!
//! Wraps `silero::VadSession` with parameters matched to Meetily Tauri's
//! production pipeline. Designed to be fed 30 ms frames of 16 kHz mono f32
//! samples (480 samples per frame) and emit `Utterance` events when a
//! speech segment ends — or, for runaway speech, every `MAX_UTTERANCE_MS`.
//!
//! Production parameters (mirrored from `frontend/src-tauri/src/audio/vad.rs`):
//! - positive_speech_threshold: 0.50 (enter speech)
//! - negative_speech_threshold: 0.35 (exit speech, hysteresis)
//! - redemption_time: 400 ms (silence required to end speech)
//! - pre_speech_pad: 300 ms (pre-roll captured before speech start)
//! - post_speech_pad: 400 ms (tail captured after speech end)
//! - min_speech_time: 250 ms (drop utterances shorter than this)
//! - sample_rate: 16 kHz
//!
//! `max_speech_time` is enforced manually because the silero revision we
//! pin doesn't expose it: when ongoing speech exceeds 30 s we slice off the
//! leading window with `take_until()` and emit it as an utterance.

use anyhow::{Context, Result};
use silero::{VadConfig, VadSession, VadTransition};
use std::time::Duration;

/// 16 kHz target sample rate for Silero.
pub const VAD_SAMPLE_RATE: usize = 16_000;

/// 30 ms frame size at 16 kHz.
pub const VAD_FRAME_SAMPLES: usize = 480;

/// Minimum utterance length in samples (250 ms @ 16 kHz). Shorter
/// utterances are dropped to avoid hallucinations on noise spikes.
pub const MIN_UTTERANCE_SAMPLES: usize = VAD_SAMPLE_RATE / 4;

/// Maximum utterance length before we force a cut. 30 seconds matches
/// Whisper's native window and the Tauri production pipeline.
pub const MAX_UTTERANCE_MS: u64 = 30_000;

/// A complete speech utterance ready for transcription.
#[derive(Debug, Clone)]
pub struct Utterance {
    /// Mono f32 samples at 16 kHz.
    pub samples: Vec<f32>,
    /// Speech start timestamp in milliseconds, relative to session start.
    pub start_ms: u64,
    /// Speech end timestamp in milliseconds, relative to session start.
    pub end_ms: u64,
    /// True if this utterance was force-emitted at `MAX_UTTERANCE_MS`
    /// rather than ended naturally by the VAD.
    pub forced: bool,
}

impl Utterance {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
}

/// Streaming Silero VAD wrapper.
pub struct Vad {
    session: VadSession,
    /// Wall-clock start of the current speech segment, used to assign a
    /// `start_ms` to force-emitted utterances.
    current_speech_start_ms: Option<u64>,
}

impl Vad {
    /// Create a new VAD with Meetily production parameters.
    pub fn new() -> Result<Self> {
        let config = VadConfig {
            positive_speech_threshold: 0.50,
            negative_speech_threshold: 0.35,
            pre_speech_pad: Duration::from_millis(300),
            post_speech_pad: Duration::from_millis(400),
            redemption_time: Duration::from_millis(400),
            sample_rate: VAD_SAMPLE_RATE,
            min_speech_time: Duration::from_millis(250),
        };
        let session = VadSession::new(config).context("failed to initialize Silero VAD session")?;
        Ok(Self {
            session,
            current_speech_start_ms: None,
        })
    }

    /// Process one 30 ms frame (480 samples @ 16 kHz). Returns any completed
    /// utterances triggered by this frame, plus any force-emitted slices for
    /// runaway speech longer than `MAX_UTTERANCE_MS`.
    pub fn process_frame(&mut self, samples: &[f32]) -> Result<Vec<Utterance>> {
        let transitions = self
            .session
            .process(samples)
            .context("silero VAD process failed")?;

        let mut utterances = Vec::new();
        for transition in transitions {
            match transition {
                VadTransition::SpeechStart { timestamp_ms } => {
                    self.current_speech_start_ms = Some(timestamp_ms as u64);
                }
                VadTransition::SpeechEnd {
                    start_timestamp_ms,
                    end_timestamp_ms,
                    samples: speech_samples,
                } => {
                    self.current_speech_start_ms = None;
                    if speech_samples.len() < MIN_UTTERANCE_SAMPLES {
                        log::debug!(
                            "vad: dropping short utterance {} samples ({} ms)",
                            speech_samples.len(),
                            speech_samples.len() * 1000 / VAD_SAMPLE_RATE
                        );
                        continue;
                    }
                    utterances.push(Utterance {
                        samples: speech_samples,
                        start_ms: start_timestamp_ms as u64,
                        end_ms: end_timestamp_ms as u64,
                        forced: false,
                    });
                }
            }
        }

        // Force-emit if currently speaking and the speech segment has grown
        // beyond MAX_UTTERANCE_MS. take_until slices samples off the front
        // of the active speech buffer so the session can keep going.
        if self.session.is_speaking() {
            let dur_ms = self.session.current_speech_duration().as_millis() as u64;
            if dur_ms >= MAX_UTTERANCE_MS {
                let cut = Duration::from_millis(MAX_UTTERANCE_MS);
                let forced_samples = self.session.take_until(cut);
                if forced_samples.len() >= MIN_UTTERANCE_SAMPLES {
                    let start_ms = self.current_speech_start_ms.unwrap_or(0);
                    let end_ms = start_ms + MAX_UTTERANCE_MS;
                    utterances.push(Utterance {
                        samples: forced_samples,
                        start_ms,
                        end_ms,
                        forced: true,
                    });
                    // Advance the logical start for any subsequent forced cut.
                    self.current_speech_start_ms = Some(end_ms);
                }
            }
        }

        Ok(utterances)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vad_initializes() {
        let vad = Vad::new();
        assert!(vad.is_ok(), "VAD should initialize with bundled model");
    }

    #[test]
    fn silence_yields_no_utterances() {
        let mut vad = Vad::new().expect("vad init");
        let frame = vec![0.0f32; VAD_FRAME_SAMPLES];
        let mut total = Vec::new();
        for _ in 0..(VAD_SAMPLE_RATE / VAD_FRAME_SAMPLES) {
            total.extend(vad.process_frame(&frame).expect("process"));
        }
        assert!(total.is_empty(), "silence should not produce utterances");
    }
}
