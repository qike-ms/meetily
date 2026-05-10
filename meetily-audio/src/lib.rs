//! # meetily-audio
//!
//! Shared audio types, capture backends, and DSP primitives for the meetily
//! desktop client (Tauri) and the meetily-client CLI.
//!
//! ## Architectural rule (load-bearing)
//!
//! **No mixing in the transcription path. Ever.** This is enforced at the type
//! level: [`TranscriptionFrame`] has private fields and crate-private constructors
//! ([`TranscriptionFrame::from_mic_capture`] and
//! [`TranscriptionFrame::from_system_capture`]). External crates cannot construct
//! a `TranscriptionFrame` from arbitrary mixed samples. The recording-only mixer
//! produces [`RecordingMixFrame`], a distinct type that the transcription
//! pipeline does not accept.
//!
//! See `obsidian-vault/projects/meetily/per-source-pipeline-design.md` (v3.1) for
//! the full design.

#![warn(missing_docs)]

mod source;

pub mod resample;
pub mod vad;

pub use resample::Resampler16k;
pub use source::{AudioFrame, AudioSource, RecordingMixFrame, SourceLabel, TranscriptionFrame};
pub use vad::{Utterance, Vad, MAX_UTTERANCE_MS, MIN_UTTERANCE_SAMPLES, VAD_FRAME_SAMPLES, VAD_SAMPLE_RATE};
