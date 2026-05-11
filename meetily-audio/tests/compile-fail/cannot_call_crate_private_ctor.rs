//! External callers must NOT be able to call the crate-private
//! constructors directly.

use meetily_audio::TranscriptionFrame;

fn main() {
    // This must NOT compile: from_mic_capture is `pub(crate)`.
    let _ = TranscriptionFrame::from_mic_capture(vec![0.0; 160], 0);
}
