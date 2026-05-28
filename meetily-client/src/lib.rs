pub mod audio;
pub mod transcribe;
pub mod upload;

pub use audio::capture::record_dual_stream;
pub use audio::devices::{list_devices, AudioDeviceInfo};
pub use transcribe::{
    download_model, get_model_path, load_model, merge_segments, transcribe_wav, TranscriptSegment,
};
pub use upload::{create_meeting, end_meeting, trigger_summarize, upload_transcript};
