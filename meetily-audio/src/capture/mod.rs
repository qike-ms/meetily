//! Capture backend implementations.
//!
//! Each submodule is gated behind a Cargo feature so consumers (CLI, Tauri)
//! only compile what they need. macOS-only modules also use `cfg(target_os
//! = "macos")` so non-macOS consumers can still depend on the crate without
//! a build break.

#[cfg(all(target_os = "macos", feature = "coreaudio"))]
/// macOS Core Audio Tap system-audio capture (gated on `coreaudio` feature).
pub mod core_audio;
