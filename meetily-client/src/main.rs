use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use meetily_client::audio::capture::{
    record_dual_stream, record_streaming, StreamSource, StreamingChunk, SystemBackend,
};
use meetily_client::audio::devices::list_devices;
use meetily_client::transcribe::{
    download_model, get_model_path, load_model, merge_segments, transcribe_chunk, transcribe_wav,
    TranscriptSegment,
};
use meetily_client::upload::{
    create_meeting, end_meeting, trigger_summarize, upload_transcript,
};
use std::sync::Arc;
use std::time::Instant;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use whisper_rs::WhisperContext;

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum BackendArg {
    Cpal,
    Coreaudio,
}

impl BackendArg {
    fn into_system_backend(self) -> SystemBackend {
        match self {
            BackendArg::Cpal => SystemBackend::Cpal,
            BackendArg::Coreaudio => SystemBackend::CoreAudio,
        }
    }

    /// Per-platform default backend.
    ///
    /// macOS (with the `coreaudio` feature compiled in, which is the default
    /// for this crate): CoreAudio Tap — no third-party audio driver required
    /// on macOS 14.2+. Other platforms / feature-disabled builds: cpal.
    fn platform_default() -> Self {
        #[cfg(all(target_os = "macos", feature = "coreaudio"))]
        {
            BackendArg::Coreaudio
        }
        #[cfg(not(all(target_os = "macos", feature = "coreaudio")))]
        {
            BackendArg::Cpal
        }
    }
}

#[derive(Debug, Parser)]
#[command(name = "meetily-client")]
#[command(about = "Capture audio and transcribe meetings")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Record {
        #[arg(long)]
        server: String,

        #[arg(long, default_value = "Meetily CLI Recording")]
        title: String,

        #[arg(long)]
        mic: Option<String>,

        #[arg(long)]
        system: Option<String>,

        #[arg(long, default_value = "large-v3-turbo")]
        model: String,

        /// Use streaming VAD pipeline (transcribes per utterance, live print).
        /// Set to false for the legacy batch path that records full WAVs first.
        #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
        streaming: bool,

        /// System-audio capture backend. On macOS 14.2+ (with the `coreaudio`
        /// feature, on by default) this defaults to `coreaudio` — Apple's
        /// native Core Audio Tap, no third-party drivers required and
        /// `--system` is ignored (taps the default output mix). Set
        /// `--backend cpal` to use the legacy cross-platform cpal
        /// default-output loopback path (requires BlackHole / Multi-Output
        /// Device on macOS, and uses the device named by `--system`).
        /// On non-macOS platforms the default is `cpal`. Batch mode
        /// (`--streaming false`) always uses cpal regardless of this flag.
        #[arg(long, value_enum)]
        backend: Option<BackendArg>,

        /// Disable WebRTC AEC3 echo cancellation. Default: AEC enabled (when
        /// the `aec` feature is compiled in, which is the default for this
        /// crate). Use this for headphone users where there is no acoustic
        /// echo to cancel — saves a few % CPU and avoids any chance of AEC
        /// distorting clean mic audio. Only meaningful in streaming mode
        /// with a system source; ignored otherwise.
        #[arg(long, default_value_t = false, action = clap::ArgAction::SetTrue)]
        no_aec: bool,
    },
    Devices,
    DownloadModel {
        model_name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Record {
            server,
            title,
            mic,
            system,
            model,
            streaming,
            backend,
            no_aec,
        } => {
            // Resolve --backend now that we know the mode. Streaming uses the
            // platform default (CoreAudio on macOS 14.2+, cpal elsewhere) when
            // not specified; batch mode is cpal-only and rejects an explicit
            // --backend coreaudio.
            let resolved_backend = if streaming {
                backend.unwrap_or_else(BackendArg::platform_default)
            } else {
                if matches!(backend, Some(BackendArg::Coreaudio)) {
                    anyhow::bail!(
                        "--backend coreaudio is only supported in streaming mode. \
                         Drop --streaming false or use --backend cpal."
                    );
                }
                BackendArg::Cpal
            };

            println!("\n=== Meetily Client ===");
            println!("Available audio devices:");
            for device in list_devices() {
                println!("  {device}");
            }
            println!();

            let client_id = Uuid::new_v4().to_string();
            println!("Creating meeting on server...");
            let meeting_id = create_meeting(&server, &title, &client_id).await?;
            println!("Meeting created: {meeting_id}");

            let model_path = get_model_path(&model);
            if !model_path.exists() {
                anyhow::bail!(
                    "Whisper model not found at {}.\nRun: meetily-client download-model {}",
                    model_path.display(),
                    model
                );
            }
            println!("Loading Whisper model: {} ...", model);
            let whisper = Arc::new(load_model(&model_path)?);
            println!("Model loaded.");

            let segments = if streaming {
                run_streaming_session(mic, system, resolved_backend.into_system_backend(), !no_aec, whisper.clone()).await?
            } else {
                run_batch_session(mic, system, &whisper).await?
            };

            // Upload
            println!("Uploading transcript to server...");
            upload_transcript(&server, &meeting_id, &segments).await?;
            println!("Uploaded {} segments.", segments.len());

            // Summarize and end
            println!("Triggering summarization...");
            trigger_summarize(&server, &meeting_id).await?;
            end_meeting(&server, &meeting_id).await?;

            println!("\n=== Meeting Complete ===");
            println!("  ID:       {meeting_id}");
            println!("  Title:    {title}");
            println!("  Segments: {}", segments.len());
            println!("  View at:  {}/app/", server);
            println!();
        }
        Commands::Devices => {
            for device in list_devices() {
                println!("{device}");
            }
        }
        Commands::DownloadModel { model_name } => {
            let path = download_model(&model_name).await?;
            println!("Downloaded model to {}", path.display());
        }
    }

    Ok(())
}

/// Streaming pipeline: cpal -> resample -> VAD -> whisper per utterance,
/// live-printed as `[mm:ss] [SRC] text`.
async fn run_streaming_session(
    mic: Option<String>,
    system: Option<String>,
    system_backend: SystemBackend,
    enable_aec: bool,
    whisper: Arc<WhisperContext>,
) -> Result<Vec<TranscriptSegment>> {
    let backend_label = match system_backend {
        SystemBackend::Cpal => "cpal",
        SystemBackend::CoreAudio => "coreaudio (Apple Core Audio Tap)",
        _ => "unknown",
    };
    let system_active = match (system.as_deref(), system_backend) {
        (None, SystemBackend::Cpal) => false,
        _ => true,
    };
    // Bind the source-name string up front so the println! borrow doesn't
    // conflict with the move into record_streaming below.
    let system_label = if system_active {
        system.as_deref().unwrap_or("default-output-mix").to_string()
    } else {
        "DISABLED (no system audio)".to_string()
    };
    let mic_label = mic.as_deref().unwrap_or("default").to_string();
    println!(
        "\n>>> Recording started — mic={} system={} backend={} aec={} <<<",
        mic_label,
        system_label,
        backend_label,
        if enable_aec { "on" } else { "off" }
    );
    println!(">>> Press Ctrl+C to stop. <<<\n");

    let (mut handle, mut rx) = record_streaming(mic, system, system_backend, enable_aec).context("failed to start streaming capture")?;
    let recording_started = Instant::now();
    let stop = CancellationToken::new();
    let stop_for_signal = stop.clone();

    // Watch for Ctrl+C. First SIGINT signals stop (drain begins). A second
    // SIGINT at any point hard-exits the process — `tokio::task::spawn_blocking`
    // tasks (Whisper transcribes) cannot be aborted once started, so we
    // cannot cleanly join them; the only honest "abort" is `process::exit`.
    // The user trades any pending transcripts (and the final upload) for an
    // immediate prompt return.
    tokio::spawn(async move {
        // First Ctrl+C → stop signal.
        if tokio::signal::ctrl_c().await.is_ok() {
            stop_for_signal.cancel();
        }
        // Second Ctrl+C → hard exit. We listen forever; if user never
        // presses again, this task exits with the runtime.
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!(
                "\n>>> Second Ctrl+C received: hard-exiting. \
                 Any pending Whisper transcribes are abandoned; \
                 the final transcript was NOT uploaded. <<<"
            );
            // Best-effort flush so the warning is visible before we go.
            use std::io::Write;
            let _ = std::io::stderr().flush();
            let _ = std::io::stdout().flush();
            std::process::exit(130); // 128 + SIGINT
        }
    });

    let mut all_segments: Vec<TranscriptSegment> = Vec::new();
    let mut transcribe_tasks: Vec<tokio::task::JoinHandle<Result<Vec<TranscriptSegment>>>> = Vec::new();
    let mut mic_count = 0usize;
    let mut sys_count = 0usize;
    let mut system_silence_warned = false;
    // Warn after 15s of recording if system audio is supposedly active but
    // produced zero utterances. Most common cause on macOS: Core Audio
    // Tap permission denied (NSAudioCaptureUsageDescription dialog
    // dismissed) → tap returns silence. The mic still picks up speaker
    // echo and gets tagged [YOU], which looks like "system audio is
    // mislabeled as mic" but is actually "system stream is empty".
    let system_should_be_active = system_active;

    loop {
        tokio::select! {
            _ = stop.cancelled() => {
                println!("\n>>> Stop signal received, draining remaining utterances... <<<");
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                // Only warn when mic IS producing utterances but system
                // is silent — that's the diagnostic case (Core Audio Tap
                // permission denied, etc.). If neither side has produced
                // anything, the user just hasn't started playing audio yet.
                if !system_silence_warned
                    && system_should_be_active
                    && sys_count == 0
                    && mic_count >= 3
                    && recording_started.elapsed() >= std::time::Duration::from_secs(20)
                {
                    eprintln!(
                        "\n>>> WARN: mic captured {mic_count} utterances but system audio captured 0 in {}s. \
                         If you're playing audio through speakers, this is likely a \
                         Core Audio Tap permission issue (macOS 14.2+ NSAudioCaptureUsageDescription). \
                         Mic transcripts may show speaker echo tagged [YOU]. \
                         Workaround: pass --backend cpal --system \"<output device name>\". <<<\n",
                        recording_started.elapsed().as_secs()
                    );
                    system_silence_warned = true;
                }
            }
            maybe_chunk = rx.recv() => {
                match maybe_chunk {
                    Some(chunk) => {
                        match chunk.source {
                            StreamSource::Mic => mic_count += 1,
                            StreamSource::System => sys_count += 1,
                        }
                        let task = spawn_transcribe(whisper.clone(), chunk, recording_started);
                        transcribe_tasks.push(task);
                    }
                    None => break,
                }
            }
        }
    }

    // Stop capture streams (closes raw channel -> pump threads will finish
    // their loop and drop the utterance sender once their buffer drains).
    handle.request_stop().context("failed to request stop on streaming capture")?;

    // Drain remaining utterances so pumps can complete their blocking_send
    // calls and exit cleanly. rx returns None when all senders are dropped.
    while let Some(chunk) = rx.recv().await {
        let task = spawn_transcribe(whisper.clone(), chunk, recording_started);
        transcribe_tasks.push(task);
    }

    // Now safe to join pump threads -- channel drained, senders dropped.
    handle
        .await_completion()
        .context("failed to await pump completion")?;

    // Drain pending transcriptions with a live progress counter. A second
    // Ctrl+C fires the global handler installed at session start, which
    // hard-exits via process::exit(130) — `tokio::task::spawn_blocking` work
    // (Whisper transcribes) cannot be aborted once started, so the only
    // honest "abort" path is process exit. The user is told this in the
    // banner below.
    let total = transcribe_tasks.len();
    if total > 0 {
        println!(
            "\n>>> Transcribing {total} pending utterances... (Ctrl+C again to hard-exit, abandoning pending transcripts) <<<"
        );

        let mut joinset = tokio::task::JoinSet::new();
        for task in transcribe_tasks {
            // Move the JoinHandle into the JoinSet by spawning a thin wrapper
            // that awaits it. This adds one task layer but keeps the existing
            // spawn_transcribe contract unchanged.
            joinset.spawn(async move { task.await });
        }

        let mut completed = 0usize;
        while let Some(next) = joinset.join_next().await {
            match next {
                Ok(Ok(Ok(mut segs))) => {
                    all_segments.append(&mut segs);
                    completed += 1;
                    // Only print every 5 completions or the last one to
                    // keep the drain readable when N is large.
                    if completed % 5 == 0 || joinset.is_empty() {
                        println!(
                            ">>> drained {completed}/{total} ({} pending) <<<",
                            joinset.len()
                        );
                    }
                }
                Ok(Ok(Err(err))) => {
                    log::warn!("transcribe task failed: {err:#}");
                    completed += 1;
                }
                Ok(Err(err)) => {
                    log::warn!("transcribe join failed: {err:#}");
                    completed += 1;
                }
                Err(err) => {
                    // JoinSet's outer task panicked or was cancelled.
                    log::warn!("transcribe wrapper task failed: {err:#}");
                    completed += 1;
                }
            }
        }
    }

    all_segments.sort_by(|a, b| {
        a.audio_start_time
            .unwrap_or(0.0)
            .partial_cmp(&b.audio_start_time.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Dedup pass: drop mic segments that are duplicates of system segments.
    // Captures the common case where system audio plays through speakers
    // and the mic picks up the echo, producing a [YOU] copy of a [THEM]
    // segment. Heuristic: ±1.5s start window, ≥0.7 Jaccard token overlap
    // → keep [THEM], drop [YOU]. Tuned against `say` echo on a MacBook Pro
    // built-in mic; intentionally lenient because Whisper transcribes
    // mic-echo and system-tap with slightly different errors.
    // Pass 1: drop Whisper hallucinations on the mic side. When the mic
    // is mostly silent or carrying faint speaker echo, Whisper invents
    // stereotyped short phrases ("Yeah.", "Thank you.", "Subscribe to my
    // channel.", etc.). Filter them BEFORE dedup so they can't survive
    // simply because their tokens don't appear in the system stream.
    let original_n = all_segments.len();
    let halluc_indices = compute_whisper_hallucinations(&all_segments);
    if !halluc_indices.is_empty() {
        let halluc_set: std::collections::HashSet<usize> =
            halluc_indices.iter().copied().collect();
        all_segments = all_segments
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| if halluc_set.contains(&i) { None } else { Some(s) })
            .collect();
        println!(
            ">>> dropped {} suspected Whisper hallucination(s) on the mic stream",
            halluc_set.len()
        );
    }

    // Pass 2: drop mic segments that are echoes of system segments.
    let after_halluc_n = all_segments.len();
    let drop_indices = compute_mic_echo_dups(&all_segments);
    if !drop_indices.is_empty() {
        let drop_set: std::collections::HashSet<usize> = drop_indices.iter().copied().collect();
        all_segments = all_segments
            .into_iter()
            .enumerate()
            .filter_map(|(i, s)| if drop_set.contains(&i) { None } else { Some(s) })
            .collect();
        println!(
            ">>> deduped {} mic echo segment(s) of system audio (kept system as canonical)",
            after_halluc_n - all_segments.len()
        );
    }
    let _ = original_n;

    if !all_segments.is_empty() {
        println!("\n=== Final Transcript ({} segments) ===", all_segments.len());
        for seg in &all_segments {
            let label = if seg.source == "mic" { "YOU" } else { "THEM" };
            println!("  [{}] [{}] {}", seg.timestamp, label, seg.text);
        }
        println!("===\n");
    }

    Ok(all_segments)
}

/// Compute indices of mic segments that look like echoes of system
/// segments. Segments must already be sorted by audio_start_time.
/// Returns indices into the input slice that should be dropped (always
/// mic-side; system stays as canonical).
///
/// Uses **token containment** with **interval overlap** so we catch the
/// common case where mic and system VADs chunk speech at different
/// boundaries — e.g. one long system utterance covers several short mic
/// fragments. A mic segment is a duplicate if its time interval overlaps
/// (or is within `WINDOW_PAD_SECS`) of a system segment AND ≥75% of the
/// mic's tokens appear in that system segment.
/// Detect Whisper hallucinations on the mic stream.
///
/// When the mic input is mostly silence, room noise, or faint speaker
/// echo, Whisper invents short stereotyped phrases ("Yeah.", "Thank
/// you.", "Subscribe to my channel.", etc.) — these are well-known in
/// the openai/whisper community as quiet-input artifacts. We drop them
/// from the mic stream only (system-stream tap audio is digitally clean
/// and doesn't trigger this).
///
/// Heuristic: a mic segment is a hallucination if its normalized text
/// matches the stock-phrase list AND it's short enough to be plausibly
/// invented (≤ MAX_HALLUC_TOKENS).
fn compute_whisper_hallucinations(segments: &[TranscriptSegment]) -> Vec<usize> {
    const MAX_HALLUC_TOKENS: usize = 6;

    // Normalised stock phrases. Lowercase, alphanumeric tokens joined by
    // single space — match against `normalize(seg.text)`.
    const STOCK_PHRASES: &[&str] = &[
        "thank you",
        "thanks",
        "thanks for watching",
        "thank you for watching",
        "thank you so much",
        "thank you very much",
        "yeah",
        "yes",
        "ok",
        "okay",
        "mm hmm",
        "uh huh",
        "bye",
        "goodbye",
        "see you",
        "see you later",
        "see you next time",
        "subscribe",
        "subscribe to my channel",
        "please subscribe",
        "like and subscribe",
        "test test",
        "test",
        "hello",
        "hi",
        "hi everyone",
        "hello everyone",
    ];

    fn normalize(s: &str) -> String {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
    fn token_count(norm: &str) -> usize {
        if norm.is_empty() {
            0
        } else {
            norm.split(' ').count()
        }
    }

    let stock: std::collections::HashSet<&str> = STOCK_PHRASES.iter().copied().collect();
    let mut drop = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if seg.source != "mic" {
            continue;
        }
        let norm = normalize(&seg.text);
        if token_count(&norm) > MAX_HALLUC_TOKENS {
            continue;
        }
        if stock.contains(norm.as_str()) {
            drop.push(i);
        }
    }
    drop
}

fn compute_mic_echo_dups(segments: &[TranscriptSegment]) -> Vec<usize> {
    const WINDOW_PAD_SECS: f64 = 1.0;
    const CONTAINMENT_MIN: f64 = 0.6;
    const SHORT_UTT_TOKENS: usize = 5;
    const SHORT_UTT_CONTAINMENT_MIN: f64 = 0.5;
    const MIN_TOKENS: usize = 2;

    fn tok(s: &str) -> std::collections::HashSet<String> {
        s.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 2)
            .map(|w| w.to_string())
            .collect()
    }
    fn containment(
        needle: &std::collections::HashSet<String>,
        haystack: &std::collections::HashSet<String>,
    ) -> f64 {
        if needle.is_empty() {
            return 0.0;
        }
        let inter = needle.intersection(haystack).count();
        inter as f64 / needle.len() as f64
    }

    let toks: Vec<std::collections::HashSet<String>> =
        segments.iter().map(|s| tok(&s.text)).collect();
    let starts: Vec<f64> = segments
        .iter()
        .map(|s| s.audio_start_time.unwrap_or(0.0))
        .collect();
    let ends: Vec<f64> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            s.audio_end_time
                .or_else(|| s.duration.map(|d| starts[i] + d))
                .unwrap_or(starts[i] + 1.0)
        })
        .collect();

    let mut drop = Vec::new();
    for (i, mic) in segments.iter().enumerate() {
        if mic.source != "mic" || toks[i].len() < MIN_TOKENS {
            continue;
        }
        let mic_s = starts[i] - WINDOW_PAD_SECS;
        let mic_e = ends[i] + WINDOW_PAD_SECS;

        // Aggregate the token set of ALL system segments overlapping the
        // mic interval. Handles two cases that single-pair check misses:
        //   - one long system segment spanning many short mic fragments
        //     (each mic ⊆ system union)
        //   - one long mic segment spanning many short system fragments
        //     (mic ⊆ union of those systems)
        let mut sys_union: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (j, sys) in segments.iter().enumerate() {
            if j == i || sys.source != "system" {
                continue;
            }
            let sys_s = starts[j];
            let sys_e = ends[j];
            // Interval overlap with slack
            if mic_s < sys_e && sys_s < mic_e {
                sys_union.extend(toks[j].iter().cloned());
            }
        }

        if !sys_union.is_empty() {
            let c = containment(&toks[i], &sys_union);
            // Lower bar for short utterances — they're more likely to be
            // mic-echo fragments where Whisper missed a word or two.
            let threshold = if toks[i].len() <= SHORT_UTT_TOKENS {
                SHORT_UTT_CONTAINMENT_MIN
            } else {
                CONTAINMENT_MIN
            };
            if c >= threshold {
                drop.push(i);
            }
        }
    }
    drop
}

fn spawn_transcribe(
    whisper: Arc<WhisperContext>,
    chunk: StreamingChunk,
    recording_started: Instant,
) -> tokio::task::JoinHandle<Result<Vec<TranscriptSegment>>> {
    let StreamingChunk { source, utterance } = chunk;
    let source_tag = source.as_str().to_string();
    let label = match source {
        StreamSource::Mic => "YOU",
        StreamSource::System => "THEM",
    };
    let _ = recording_started;

    // Use utterance start_ms (relative to VAD session start) as the wall-clock
    // offset so timestamps stay monotonic per source.
    let offset_seconds = utterance.start_ms as f64 / 1000.0;
    tokio::task::spawn_blocking(move || {
        let segments = transcribe_chunk(&utterance.samples, &whisper, &source_tag, offset_seconds)?;
        // Print one line per actual transcript text — no placeholder
        // header (kept the terminal too noisy when speech was frequent).
        for seg in &segments {
            println!("[{}] [{}] {}", seg.timestamp, label, seg.text);
        }
        Ok(segments)
    })
}

/// Legacy batch pipeline (full WAV files, then transcribe at end).
async fn run_batch_session(
    mic: Option<String>,
    system: Option<String>,
    whisper: &WhisperContext,
) -> Result<Vec<TranscriptSegment>> {
    println!("\n>>> Recording started (batch mode). Press Ctrl+C to stop. <<<\n");
    let stop = CancellationToken::new();
    let recording = tokio::spawn(record_dual_stream(mic, system, stop.clone()));

    tokio::signal::ctrl_c().await?;
    println!("\n>>> Recording stopped. <<<\n");
    stop.cancel();

    let (mic_wav, system_wav) = recording.await??;

    println!("Transcribing microphone audio...");
    let mic_segments = transcribe_wav(&mic_wav, whisper, "mic")?;
    if mic_segments.is_empty() {
        println!("  (no speech detected from mic)");
    } else {
        println!("\n--- Mic Transcript ({} segments) ---", mic_segments.len());
        for seg in &mic_segments {
            println!("  [{}] {}", seg.timestamp, seg.text);
        }
        println!("---\n");
    }

    let system_segments = if system_wav.metadata().map(|m| m.len()).unwrap_or(0) > 44 {
        println!("Transcribing system audio...");
        let segs = transcribe_wav(&system_wav, whisper, "system")?;
        if segs.is_empty() {
            println!("  (no speech detected from system)");
        } else {
            println!("\n--- System Transcript ({} segments) ---", segs.len());
            for seg in &segs {
                println!("  [{}] {}", seg.timestamp, seg.text);
            }
            println!("---\n");
        }
        segs
    } else {
        println!("  (no system audio recorded)");
        Vec::new()
    };

    let segments = merge_segments(mic_segments, system_segments);
    if !segments.is_empty() {
        println!("\n=== Combined Transcript ({} segments) ===", segments.len());
        for seg in &segments {
            let label = if seg.source == "mic" { "YOU" } else { "THEM" };
            println!("  [{}] [{}] {}", seg.timestamp, label, seg.text);
        }
        println!("===\n");
    }

    delete_temp_wav(&mic_wav).await;
    delete_temp_wav(&system_wav).await;
    Ok(segments)
}

async fn delete_temp_wav(path: &std::path::Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("failed to delete temp WAV {}: {err}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(source: &str, start: f64, end: f64, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            id: format!("t-{}", start),
            timestamp: format!("00:00:{:.3}", start),
            text: text.to_string(),
            source: source.to_string(),
            confidence: 1.0,
            duration_ms: ((end - start) * 1000.0) as u32,
            audio_start_time: Some(start),
            audio_end_time: Some(end),
            duration: Some(end - start),
        }
    }

    #[test]
    fn dedup_drops_mic_echo_of_system() {
        let segs = vec![
            seg("system", 1.0, 4.0, "first test sentence apple banana cherry"),
            seg("mic", 1.5, 4.5, "first test sentence apple banana cherry"),
        ];
        let drops = compute_mic_echo_dups(&segs);
        assert_eq!(drops, vec![1], "mic echo should be dropped");
    }

    #[test]
    fn dedup_keeps_distinct_user_speech_during_system() {
        let segs = vec![
            seg("system", 1.0, 5.0, "the system talks about apples bananas and cherries"),
            seg("mic", 2.0, 4.0, "i am the user discussing quantum mechanics relativity"),
        ];
        let drops = compute_mic_echo_dups(&segs);
        assert!(drops.is_empty(), "distinct user speech must not be dropped, got {:?}", drops);
    }

    #[test]
    fn dedup_handles_fragmented_system_segments() {
        // Long mic utterance covering several short system fragments.
        let segs = vec![
            seg("system", 1.0, 2.0, "the true power"),
            seg("system", 2.0, 3.0, "move is not in saying"),
            seg("system", 3.0, 4.0, "something real quick"),
            seg("mic", 1.2, 4.2, "the true power move is not in saying something real quick"),
        ];
        let drops = compute_mic_echo_dups(&segs);
        assert_eq!(drops, vec![3], "long mic should match union of short system");
    }

    #[test]
    fn dedup_handles_fragmented_mic_segments() {
        // Long system utterance covering several short mic fragments.
        let segs = vec![
            seg("system", 1.0, 5.0, "when somebody jabs you typically want to jab back at them"),
            seg("mic", 1.5, 2.0, "when somebody jabs"),
            seg("mic", 2.5, 3.5, "you typically want to jab back"),
        ];
        let drops = compute_mic_echo_dups(&segs);
        assert!(drops.contains(&1) && drops.contains(&2), "both mic fragments should be dropped, got {:?}", drops);
    }

    #[test]
    fn dedup_keeps_short_unrelated_mic() {
        let segs = vec![
            seg("system", 1.0, 5.0, "the speaker is discussing a long topic about something unrelated"),
            seg("mic", 2.0, 2.5, "what time is it"),
        ];
        let drops = compute_mic_echo_dups(&segs);
        assert!(drops.is_empty(), "short distinct mic must not be dropped, got {:?}", drops);
    }

    #[test]
    fn dedup_keeps_mic_outside_window() {
        let segs = vec![
            seg("system", 1.0, 3.0, "apple banana cherry"),
            seg("mic", 10.0, 12.0, "apple banana cherry"),
        ];
        let drops = compute_mic_echo_dups(&segs);
        assert!(drops.is_empty(), "mic far from system in time must not be dropped, got {:?}", drops);
    }
}
