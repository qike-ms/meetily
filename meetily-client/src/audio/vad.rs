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
//! pin doesn't expose it: when ongoing speech exceeds 30 s we snapshot the
//! active speech buffer, emit the leading window as a forced utterance,
//! and `reset()` the silero session. See the force-cut block in
//! `Vad::process_frame` for the rationale (avoids a silero panic that
//! `take_until` would trigger on subsequent natural SpeechEnd).

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
    /// `start_ms` to force-emitted utterances. Cleared on natural SpeechEnd
    /// and on a forced emit (since we `reset()` the session there, the next
    /// segment will produce a fresh `SpeechStart` with its own timestamp).
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

        // Force-emit if currently speaking and the live speech buffer has
        // grown beyond MAX_UTTERANCE_MS.
        //
        // We deliberately do NOT use `take_until` here. silero rev 26a6460
        // updates an internal `speech_start_ms` field on `take_until`, but
        // does NOT update the `start_ms` carried inside `VadState::Speech`.
        // When the segment eventually ends naturally, silero computes
        // `unchecked_duration_to_index(VadState::Speech.start_ms)`, which is
        // now before `deleted_samples` and panics inside the silero process
        // loop, killing the pump. Verified against
        // ~/.cargo/git/.../silero-rs/26a6460/src/lib.rs lines 311-358, 440-458.
        //
        // Safe alternative: snapshot the active speech buffer, emit the
        // first MAX_UTTERANCE_MS as a forced utterance, then `reset()` the
        // session. `reset()` clears LSTM state + marks state Silence but
        // does not clear `session_audio`/`processed_samples`, so subsequent
        // frames still produce correct timestamps. Trade-off: any audio in
        // the live buffer beyond MAX_UTTERANCE_MS (the trailing tail of a
        // continuous monologue past the 30s mark) is dropped until the next
        // SpeechStart is detected. For 30s force-cuts this is rare and far
        // preferable to a panic.
        if self.session.is_speaking() {
            let speech_dur = self.session.current_speech_duration();
            let max = Duration::from_millis(MAX_UTTERANCE_MS);
            if speech_dur >= max {
                let active = self.session.get_current_speech();
                let take = MAX_UTTERANCE_MS as usize * VAD_SAMPLE_RATE / 1000;
                let take = take.min(active.len());
                let forced_samples: Vec<f32> = active[..take].to_vec();
                // Derive start_ms defensively. If for any reason
                // `current_speech_start_ms` is unset while silero claims
                // is_speaking, fall back to (session_time - speech_dur).
                let start_ms = self.current_speech_start_ms.unwrap_or_else(|| {
                    self.session
                        .session_time()
                        .saturating_sub(speech_dur)
                        .as_millis() as u64
                });
                // Compute end_ms from actual sample count, not the request,
                // so a short forced_samples (e.g. silero/get_current_speech
                // length disagrees with current_speech_duration) still
                // produces a consistent timestamp.
                let dur_ms = (forced_samples.len() as u64) * 1000 / VAD_SAMPLE_RATE as u64;
                let end_ms = start_ms + dur_ms;
                self.session.reset();
                self.current_speech_start_ms = None;
                if forced_samples.len() >= MIN_UTTERANCE_SAMPLES {
                    utterances.push(Utterance {
                        samples: forced_samples,
                        start_ms,
                        end_ms,
                        forced: true,
                    });
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
