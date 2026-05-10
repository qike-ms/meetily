// Audio capture implementations module

pub mod microphone;
pub mod system;
pub mod backend_config;

// Core Audio Tap (macOS system-audio capture) lives in the shared
// `meetily-audio` crate behind the `coreaudio` feature. Re-export under the
// previous path so existing call sites (e.g. `super::capture::CoreAudioCapture`)
// continue to work unchanged.
#[cfg(target_os = "macos")]
pub use meetily_audio::capture::core_audio;

// Re-export capture functionality
pub use system::{
    SystemAudioCapture, SystemAudioStream,
    start_system_audio_capture, list_system_audio_devices,
    check_system_audio_permissions
};

#[cfg(target_os = "macos")]
pub use meetily_audio::capture::core_audio::{CoreAudioCapture, CoreAudioStream};

// Re-export backend configuration
pub use backend_config::{
    AudioCaptureBackend, BackendConfig, BACKEND_CONFIG,
    get_current_backend, set_current_backend, get_available_backends
};
