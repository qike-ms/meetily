//! Architectural enforcement: the Tauri transcription path must be
//! mixer-free.
//!
//! Per per-source-pipeline-design v3.2 §4 / WI-Tauri-Unmix #57, mixing
//! mic + system audio in the transcription path is forbidden — it loses
//! per-source labels and architecturally collapses two streams into one.
//!
//! Two passes:
//! 1. **`pipeline.rs`** is allowed to *mention* `RecordingMixer` because
//!    the recording-WAV path legitimately routes through it. But it must
//!    not invoke the low-level mixer types
//!    (`ProfessionalAudioMixer`, `AudioMixerRingBuffer`, `mix_window`)
//!    directly — those live exclusively in
//!    `frontend/src-tauri/src/audio/recording_mix.rs`.
//! 2. **`transcription/`** must be free of *all* mixer references, low-
//!    or high-level. The transcription worker should never see a mixed
//!    chunk; the upstream pipeline guarantees per-source `device_type`.
//!
//! If you are intentionally adding mixing to the transcription path
//! (don't), update this test and bring the change to PM-M1 / Qi.

use std::fs;
use std::path::{Path, PathBuf};

/// Patterns that must never appear in the Tauri transcription path —
/// neither the low-level mixer types nor the wrapper.
const FORBIDDEN_IN_TRANSCRIPTION: &[&str] = &[
    "ProfessionalAudioMixer",
    "AudioMixerRingBuffer",
    "mix_window",
    "RecordingMixer",
    "drain_mixed_windows",
];

/// Patterns banned in `pipeline.rs` specifically. Subset of the above —
/// `RecordingMixer` / `drain_mixed_windows` are allowed in pipeline.rs
/// because the recording-WAV path legitimately uses them.
const FORBIDDEN_IN_PIPELINE: &[&str] = &[
    "ProfessionalAudioMixer",
    "AudioMixerRingBuffer",
    "mix_window",
];

/// Returns the workspace root regardless of where the test is invoked
/// from. The `meetily-audio` crate dir contains this test; its parent is
/// the workspace root.
fn workspace_root() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().expect("workspace root").to_path_buf()
}

fn collect_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rust_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

fn scan_for_violations(file: &Path, root: &Path, patterns: &[&str], violations: &mut Vec<String>) {
    let contents = match fs::read_to_string(file) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[no_mixing_in_tauri] cannot read {}: {}", file.display(), e);
            return;
        }
    };
    let rel = file
        .strip_prefix(root)
        .unwrap_or(file)
        .display()
        .to_string();
    for (lineno, line) in contents.lines().enumerate() {
        // Skip comments — the rationale text below mentions the names
        // and we don't want to false-positive on documentation.
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with("//!") {
            continue;
        }
        for pat in patterns {
            if line.contains(pat) {
                violations.push(format!(
                    "{}:{}: forbidden pattern `{}`: `{}`",
                    rel,
                    lineno + 1,
                    pat,
                    line.trim()
                ));
            }
        }
    }
}

#[test]
fn no_mixing_in_tauri_transcription_path() {
    let root = workspace_root();
    let pipeline = root.join("frontend/src-tauri/src/audio/pipeline.rs");
    let transcription_dir = root.join("frontend/src-tauri/src/audio/transcription");

    if !pipeline.exists() {
        // Tauri tree may not be present in some checkout configurations
        // (e.g. agent sandboxes). Skip silently rather than fail.
        eprintln!(
            "[no_mixing_in_tauri] skipping: {} not found",
            pipeline.display()
        );
        return;
    }

    let mut violations: Vec<String> = Vec::new();

    // Pass 1: pipeline.rs — block low-level mixer types (high-level
    // RecordingMixer is allowed here for the recording-WAV path).
    scan_for_violations(&pipeline, &root, FORBIDDEN_IN_PIPELINE, &mut violations);

    // Pass 2: transcription/ — block ALL mixer references.
    let mut transcription_files: Vec<PathBuf> = Vec::new();
    collect_rust_files(&transcription_dir, &mut transcription_files);
    for file in &transcription_files {
        scan_for_violations(file, &root, FORBIDDEN_IN_TRANSCRIPTION, &mut violations);
    }

    assert!(
        violations.is_empty(),
        "Found {} mixing reference(s) in the Tauri transcription path. \
         Mixing is permitted only in `frontend/src-tauri/src/audio/recording_mix.rs` \
         (the recording-WAV path). pipeline.rs may use `RecordingMixer` for the \
         recording path but must NOT invoke low-level mixer types directly. \
         transcription/ must be free of all mixer references. See \
         per-source-pipeline-design v3.2 §4 and the rustdoc on this test for \
         rationale.\n\nViolations:\n{}",
        violations.len(),
        violations.join("\n")
    );
}

