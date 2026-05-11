//! Source-tagged audio types and the [`AudioSource`] trait.
//!
//! The types in this module enforce the no-mixing rule at the type level:
//! [`TranscriptionFrame`] cannot be constructed from arbitrary samples by
//! external code; only the per-source capture pipeline (within this crate)
//! can build them via the crate-private constructors.

use std::pin::Pin;

use anyhow::Result;
use futures::Stream;

/// Identifies which capture stream a frame came from.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SourceLabel {
    /// Local microphone (the "you" stream).
    Mic,
    /// System / loopback audio (the "them" stream — remote participants,
    /// media playback, etc.).
    System,
}

impl SourceLabel {
    /// Stable string representation suitable for JSON / API payloads.
    pub fn as_str(self) -> &'static str {
        match self {
            SourceLabel::Mic => "mic",
            SourceLabel::System => "system",
        }
    }
}

/// A raw audio frame straight out of a capture backend.
///
/// Sample rate is whatever the underlying device provides; downstream stages
/// resample as needed. Mono `f32` samples in `[-1.0, 1.0]`.
#[derive(Clone, Debug)]
pub struct AudioFrame {
    /// Mono PCM samples.
    pub samples: Vec<f32>,
    /// Native sample rate of the source device.
    pub sample_rate: u32,
    /// Monotonic timestamp (milliseconds since session start) for the first
    /// sample in this frame.
    pub timestamp_ms: u64,
}

/// A single-source frame ready for transcription. Cannot be constructed by
/// external callers — only the per-source capture pipeline can produce these
/// via [`TranscriptionFrame::from_mic_capture`] /
/// [`TranscriptionFrame::from_system_capture`].
///
/// The privacy of the fields is the load-bearing enforcement of the no-mixing
/// rule: there is no `From<(MicSamples, SystemSamples)>` impl, no public
/// constructor that accepts a [`SourceLabel`] alongside arbitrary samples, and
/// (by design) no way for downstream code to forge a frame whose label
/// disagrees with its origin.
#[derive(Clone, Debug)]
pub struct TranscriptionFrame {
    source: SourceLabel,
    samples: Vec<f32>,
    timestamp_ms: u64,
}

impl TranscriptionFrame {
    /// Build a mic-tagged transcription frame. Crate-private: only the mic
    /// capture pipeline calls this.
    #[allow(dead_code)] // wired up by capture pipeline in WI-A3
    pub(crate) fn from_mic_capture(samples: Vec<f32>, timestamp_ms: u64) -> Self {
        Self {
            source: SourceLabel::Mic,
            samples,
            timestamp_ms,
        }
    }

    /// Build a system-tagged transcription frame. Crate-private: only the
    /// system capture pipeline calls this.
    #[allow(dead_code)] // wired up by capture pipeline in WI-A3
    pub(crate) fn from_system_capture(samples: Vec<f32>, timestamp_ms: u64) -> Self {
        Self {
            source: SourceLabel::System,
            samples,
            timestamp_ms,
        }
    }

    /// Source this frame originated from (mic or system).
    pub fn source(&self) -> SourceLabel {
        self.source
    }

    /// 16 kHz mono samples ready for VAD / Whisper.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Monotonic timestamp (ms from session start) of this frame's first sample.
    pub fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }
}

/// A frame produced by the recording-path mixer. Distinct type from
/// [`TranscriptionFrame`] so the transcription pipeline cannot accept it.
///
/// This exists only for an optional "single mixed playback WAV" recording
/// feature (deferred, low priority for v1). The transcription path takes
/// only [`TranscriptionFrame`].
#[derive(Clone, Debug)]
pub struct RecordingMixFrame {
    samples: Vec<f32>,
    timestamp_ms: u64,
}

impl RecordingMixFrame {
    /// Build a mixed recording frame. Crate-private — only the recording-path
    /// mixer calls this.
    #[allow(dead_code)] // not used yet; recording mixer not implemented in A1
    pub(crate) fn from_mixer(samples: Vec<f32>, timestamp_ms: u64) -> Self {
        Self {
            samples,
            timestamp_ms,
        }
    }

    /// Mixed PCM samples.
    pub fn samples(&self) -> &[f32] {
        &self.samples
    }

    /// Monotonic timestamp (ms from session start).
    pub fn timestamp_ms(&self) -> u64 {
        self.timestamp_ms
    }
}

/// An audio capture source (mic or system). Object-safe: implementors can be
/// stored as `Box<dyn AudioSource>`.
///
/// Implementations live in the `capture` module behind feature-gated platform
/// backends (added in WI-A3).
pub trait AudioSource: Send {
    /// Whether this source produces mic or system audio.
    fn label(&self) -> SourceLabel;

    /// Native sample rate of the source. Frames returned from [`Self::start`]
    /// will use this rate; downstream stages resample as needed.
    fn sample_rate(&self) -> u32;

    /// Begin streaming. Returns a `Stream` of raw [`AudioFrame`]s. Calling
    /// twice without a [`Self::stop`] in between is implementation-defined.
    fn start(&mut self) -> Result<Pin<Box<dyn Stream<Item = AudioFrame> + Send>>>;

    /// Stop the underlying capture stream. Safe to call multiple times.
    fn stop(&mut self) -> Result<()>;
}
