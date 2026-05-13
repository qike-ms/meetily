use anyhow::{anyhow, Context, Result};
use hound::SampleFormat;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const WHISPER_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptSegment {
    #[serde(default = "new_segment_id")]
    pub id: String,
    pub timestamp: String,
    pub text: String,
    pub source: String,
    pub confidence: f32,
    pub duration_ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_start_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_end_time: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<f64>,
}

use std::sync::Once;

/// Install a no-op log callback into whisper.cpp / ggml so the C-side
/// stderr spam (`whisper_init_state: kv self size = ...`,
/// `ggml_metal_init: picking default device: ...`, etc.) doesn't flood the
/// terminal. We call this lazily once before the first transcribe.
fn silence_whisper_native_logs() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        unsafe extern "C" fn noop_log(
            _level: u32,
            _text: *const std::os::raw::c_char,
            _user_data: *mut std::os::raw::c_void,
        ) {
            // discard everything
        }
        unsafe {
            whisper_rs::set_log_callback(Some(noop_log), std::ptr::null_mut());
        }
    });
}

pub fn load_model(model_path: impl AsRef<Path>) -> Result<WhisperContext> {
    silence_whisper_native_logs();
    let model = model_path.as_ref().to_string_lossy();
    WhisperContext::new_with_params(&model, WhisperContextParameters::default()).map_err(|err| {
        anyhow!(
            "failed to load whisper model {}: {err}",
            model_path.as_ref().display()
        )
    })
}

pub fn transcribe_wav(
    wav_path: impl AsRef<Path>,
    ctx: &WhisperContext,
    source_tag: &str,
) -> Result<Vec<TranscriptSegment>> {
    let samples = read_wav_mono_16k(wav_path.as_ref())?;
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    transcribe_samples(&samples, ctx, source_tag, 0.0)
}

/// Transcribe a single VAD-extracted utterance (mono f32 @ 16 kHz).
///
/// `wall_clock_offset_seconds` shifts the produced segment timestamps onto
/// the recording's wall clock so they line up across mic + system streams.
pub fn transcribe_chunk(
    samples: &[f32],
    ctx: &WhisperContext,
    source_tag: &str,
    wall_clock_offset_seconds: f64,
) -> Result<Vec<TranscriptSegment>> {
    if samples.is_empty() {
        return Ok(Vec::new());
    }
    transcribe_samples(samples, ctx, source_tag, wall_clock_offset_seconds)
}

fn transcribe_samples(
    samples: &[f32],
    ctx: &WhisperContext,
    source_tag: &str,
    wall_clock_offset_seconds: f64,
) -> Result<Vec<TranscriptSegment>> {
    let mut state = ctx.create_state()?;
    let mut params = FullParams::new(SamplingStrategy::BeamSearch {
        beam_size: 5,
        patience: 1.0,
    });

    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_suppress_non_speech_tokens(true);
    params.set_temperature(0.2);
    params.set_no_speech_thold(0.55);
    params.set_single_segment(false);

    state.full(params, samples)?;
    let segment_count = state.full_n_segments()?;
    let mut segments = Vec::new();

    for index in 0..segment_count {
        let text = state.full_get_segment_text_lossy(index)?.trim().to_string();
        if text.is_empty() {
            continue;
        }

        let start = state.full_get_segment_t0(index).unwrap_or(0).max(0) as u64;
        let end = state
            .full_get_segment_t1(index)
            .unwrap_or(start as i64)
            .max(start as i64) as u64;
        let start_seconds = start as f64 / 100.0 + wall_clock_offset_seconds;
        let end_seconds = end as f64 / 100.0 + wall_clock_offset_seconds;
        let duration = (end_seconds - start_seconds).max(0.0);

        segments.push(TranscriptSegment {
            id: new_segment_id(),
            timestamp: format_timestamp(start_seconds),
            text,
            source: source_tag.to_string(),
            confidence: 0.8,
            duration_ms: (duration * 1000.0).round() as u32,
            audio_start_time: Some(start_seconds),
            audio_end_time: Some(end_seconds),
            duration: Some(duration),
        });
    }

    Ok(segments)
}

pub fn merge_segments(
    mut mic_segments: Vec<TranscriptSegment>,
    mut system_segments: Vec<TranscriptSegment>,
) -> Vec<TranscriptSegment> {
    mic_segments.append(&mut system_segments);
    mic_segments.sort_by(|a, b| {
        timestamp_seconds(&a.timestamp)
            .partial_cmp(&timestamp_seconds(&b.timestamp))
            .unwrap_or(Ordering::Equal)
    });
    mic_segments
}

pub fn get_model_path(model_name: &str) -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".local")
        .join("share")
        .join("meetily")
        .join("models")
        .join(format!("ggml-{model_name}.bin"))
}

pub async fn download_model(model_name: &str) -> Result<PathBuf> {
    let path = get_model_path(model_name);
    if path.exists() {
        return Ok(path);
    }

    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let url =
        format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-{model_name}.bin");
    let temp_path = path.with_extension("bin.part");
    let download_result = async {
        let response = reqwest::get(&url).await?.error_for_status()?;
        let total = response.content_length();
        let mut file = tokio::fs::File::create(&temp_path).await?;
        let mut downloaded = 0_u64;
        let mut response = response;

        while let Some(chunk) = response.chunk().await? {
            use tokio::io::AsyncWriteExt;

            file.write_all(&chunk).await?;
            downloaded += chunk.len() as u64;

            if let Some(total) = total {
                let pct = (downloaded as f64 / total as f64 * 100.0).min(100.0);
                eprint!("\rDownloading {model_name}: {pct:>5.1}%");
            } else {
                eprint!(
                    "\rDownloading {model_name}: {:.1} MB",
                    downloaded as f64 / 1_048_576.0
                );
            }
        }

        eprintln!();
        file.sync_all().await?;
        tokio::fs::rename(&temp_path, &path).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;

    if let Err(err) = download_result {
        if let Err(cleanup_err) = tokio::fs::remove_file(&temp_path).await {
            if cleanup_err.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "failed to remove partial model download {}: {cleanup_err}",
                    temp_path.display()
                );
            }
        }
        return Err(err);
    }

    Ok(path)
}

fn read_wav_mono_16k(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("failed to open WAV {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 || spec.sample_rate != WHISPER_SAMPLE_RATE {
        return Err(anyhow!(
            "expected 16 kHz mono WAV, got {} Hz / {} channels",
            spec.sample_rate,
            spec.channels
        ));
    }

    match (spec.sample_format, spec.bits_per_sample) {
        (SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into),
        (SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into),
        _ => Err(anyhow!(
            "unsupported WAV format: {:?} / {} bits",
            spec.sample_format,
            spec.bits_per_sample
        )),
    }
}

fn format_timestamp(seconds: f64) -> String {
    let millis = (seconds * 1000.0).round() as u64;
    let hours = millis / 3_600_000;
    let minutes = (millis / 60_000) % 60;
    let secs = (millis / 1000) % 60;
    let ms = millis % 1000;
    format!("{hours:02}:{minutes:02}:{secs:02}.{ms:03}")
}

fn timestamp_seconds(timestamp: &str) -> f64 {
    let parts: Vec<_> = timestamp.split([':', '.']).collect();
    if parts.len() != 4 {
        return 0.0;
    }

    let hours = parts[0].parse::<f64>().unwrap_or(0.0);
    let minutes = parts[1].parse::<f64>().unwrap_or(0.0);
    let seconds = parts[2].parse::<f64>().unwrap_or(0.0);
    let millis = parts[3].parse::<f64>().unwrap_or(0.0);
    hours * 3600.0 + minutes * 60.0 + seconds + millis / 1000.0
}

fn new_segment_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
