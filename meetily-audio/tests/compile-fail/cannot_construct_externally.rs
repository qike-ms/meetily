//! External callers must NOT be able to construct a TranscriptionFrame
//! by passing arbitrary samples + a SourceLabel. The fields are private;
//! only the crate-private `from_mic_capture` / `from_system_capture`
//! constructors can produce one.

use meetily_audio::{SourceLabel, TranscriptionFrame};

fn main() {
    // This must NOT compile: fields are private.
    let _ = TranscriptionFrame {
        source: SourceLabel::Mic,
        samples: vec![0.0; 160],
        timestamp_ms: 0,
    };
}
