//! Recording-only audio mixing for the WAV-output path.
//!
//! Per the per-source-pipeline-design v3.2 §1, mixing is **forbidden in
//! the transcription path** but **permitted in the recording-only path**
//! (recording WAV files are deprioritized for v1, and the user may want a
//! single-file recording for archival purposes regardless of how
//! transcription is structured).
//!
//! This module owns the `ProfessionalAudioMixer` and `AudioMixerRingBuffer`
//! types that previously lived in `pipeline.rs`. Lifting them out enforces
//! the architectural separation: a grep for `mix_window` in
//! `frontend/src-tauri/src/audio/{pipeline.rs,transcription/}` will return
//! zero hits, making the no-mix-in-transcription rule trivially auditable
//! (covered by `meetily-audio/tests/no_mixing_in_tauri.rs`).

use std::collections::VecDeque;
use log::{debug, error, info, warn};

use super::recording_state::DeviceType;

/// Ring buffer for synchronized audio mixing.
///
/// Accumulates samples from mic and system streams until aligned mixing
/// windows are available. Used **only** by the recording-WAV pipeline.
pub struct AudioMixerRingBuffer {
    mic_buffer: VecDeque<f32>,
    system_buffer: VecDeque<f32>,
    window_size_samples: usize,
    max_buffer_size: usize,
}

impl AudioMixerRingBuffer {
    pub fn new(sample_rate: u32) -> Self {
        let window_ms = 600.0;
        let window_size_samples = (sample_rate as f32 * window_ms / 1000.0) as usize;

        // Increased max buffer to 8 windows for system audio stability:
        // System audio (especially Core Audio on macOS) can have significant
        // jitter due to sample-by-sample streaming → batching → channel
        // transmission. Accounts for: RNNoise buffering + Core Audio jitter
        // + processing delays.
        let max_buffer_size = window_size_samples * 8;

        info!(
            "🔊 [recording_mix] Ring buffer initialized: window={}ms ({} samples), max={}ms ({} samples)",
            window_ms,
            window_size_samples,
            window_ms * 8.0,
            max_buffer_size
        );

        Self {
            mic_buffer: VecDeque::with_capacity(max_buffer_size),
            system_buffer: VecDeque::with_capacity(max_buffer_size),
            window_size_samples,
            max_buffer_size,
        }
    }

    pub fn add_samples(&mut self, device_type: DeviceType, samples: Vec<f32>) {
        match device_type {
            DeviceType::Microphone => self.mic_buffer.extend(samples),
            DeviceType::System => self.system_buffer.extend(samples),
        }

        // Warn before dropping samples to help diagnose timing issues in
        // production.
        if self.mic_buffer.len() > self.max_buffer_size {
            warn!(
                "⚠️ [recording_mix] Microphone buffer overflow: {} > {} samples, dropping oldest {}",
                self.mic_buffer.len(),
                self.max_buffer_size,
                self.mic_buffer.len() - self.max_buffer_size
            );
        }
        if self.system_buffer.len() > self.max_buffer_size {
            error!(
                "🔴 [recording_mix] SYSTEM AUDIO BUFFER OVERFLOW: {} > {} samples, dropping {}",
                self.system_buffer.len(),
                self.max_buffer_size,
                self.system_buffer.len() - self.max_buffer_size
            );
        }

        while self.mic_buffer.len() > self.max_buffer_size {
            self.mic_buffer.pop_front();
        }
        while self.system_buffer.len() > self.max_buffer_size {
            self.system_buffer.pop_front();
        }
    }

    pub fn can_mix(&self) -> bool {
        self.mic_buffer.len() >= self.window_size_samples
            || self.system_buffer.len() >= self.window_size_samples
    }

    pub fn extract_window(&mut self) -> Option<(Vec<f32>, Vec<f32>)> {
        if !self.can_mix() {
            return None;
        }

        let mic_window = if self.mic_buffer.len() >= self.window_size_samples {
            self.mic_buffer.drain(0..self.window_size_samples).collect()
        } else if !self.mic_buffer.is_empty() {
            let available: Vec<f32> = self.mic_buffer.drain(..).collect();
            let mut padded = Vec::with_capacity(self.window_size_samples);
            padded.extend_from_slice(&available);
            padded.resize(self.window_size_samples, 0.0);
            padded
        } else {
            vec![0.0; self.window_size_samples]
        };

        let sys_window = if self.system_buffer.len() >= self.window_size_samples {
            self.system_buffer
                .drain(0..self.window_size_samples)
                .collect()
        } else if !self.system_buffer.is_empty() {
            let available: Vec<f32> = self.system_buffer.drain(..).collect();
            let mut padded = Vec::with_capacity(self.window_size_samples);
            padded.extend_from_slice(&available);
            padded.resize(self.window_size_samples, 0.0);
            padded
        } else {
            vec![0.0; self.window_size_samples]
        };

        Some((mic_window, sys_window))
    }
}

/// Simple audio mixer without aggressive ducking. Combines mic + system
/// audio with proportional soft-clipping to prevent distortion. Used
/// **only** by the recording-WAV pipeline.
pub struct ProfessionalAudioMixer;

impl ProfessionalAudioMixer {
    pub fn new(_sample_rate: u32) -> Self {
        Self
    }

    pub fn mix_window(&mut self, mic_window: &[f32], sys_window: &[f32]) -> Vec<f32> {
        let max_len = mic_window.len().max(sys_window.len());
        let mut mixed = Vec::with_capacity(max_len);

        for i in 0..max_len {
            let mic = mic_window.get(i).copied().unwrap_or(0.0);
            let sys = sys_window.get(i).copied().unwrap_or(0.0);

            // Pre-scale system audio (was 0.7 historically; now 1.0 since
            // mic is normalized to -23 LUFS upstream and the headroom is
            // sufficient). Reserved factor for mic kept commented for
            // future tuning.
            let sys_scaled = sys * 1.0;
            let _mic_scaled = mic * 0.8;

            let sum = mic + sys_scaled;

            // Soft scaling prevents distortion artifacts: if the sum would
            // exceed ±1.0, scale down proportionally instead of hard clip.
            let sum_abs = sum.abs();
            let mixed_sample = if sum_abs > 1.0 {
                sum / sum_abs
            } else {
                sum
            };

            mixed.push(mixed_sample);
        }

        mixed
    }
}

/// Convenience wrapper: feeds raw per-source samples in, emits mixed
/// windows out. Owns both the ring buffer and the mixer so the embedding
/// pipeline only needs one type.
pub struct RecordingMixer {
    ring_buffer: AudioMixerRingBuffer,
    mixer: ProfessionalAudioMixer,
}

impl RecordingMixer {
    pub fn new(sample_rate: u32) -> Self {
        Self {
            ring_buffer: AudioMixerRingBuffer::new(sample_rate),
            mixer: ProfessionalAudioMixer::new(sample_rate),
        }
    }

    /// Push raw samples for the given source.
    pub fn add_samples(&mut self, device_type: DeviceType, samples: Vec<f32>) {
        self.ring_buffer.add_samples(device_type, samples);
    }

    /// Drain any aligned windows ready for mixing. Returns one mixed
    /// `Vec<f32>` per available window; empty `Vec` if none.
    pub fn drain_mixed_windows(&mut self) -> Vec<Vec<f32>> {
        let mut out = Vec::new();
        while self.ring_buffer.can_mix() {
            if let Some((mic_window, sys_window)) = self.ring_buffer.extract_window() {
                let mixed = self.mixer.mix_window(&mic_window, &sys_window);
                debug!("[recording_mix] emitted mixed window: {} samples", mixed.len());
                out.push(mixed);
            }
        }
        out
    }
}
