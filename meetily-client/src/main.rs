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
    println!("\n>>> Recording started (streaming VAD). Press Ctrl+C to stop. <<<\n");

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

    loop {
        tokio::select! {
            _ = stop.cancelled() => {
                println!("\n>>> Stop signal received, draining remaining utterances... <<<");
                break;
            }
            maybe_chunk = rx.recv() => {
                match maybe_chunk {
                    Some(chunk) => {
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
                    println!(
                        ">>> Transcribed {completed}/{total} ({} pending) <<<",
                        joinset.len()
                    );
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

fn spawn_transcribe(
    whisper: Arc<WhisperContext>,
    chunk: StreamingChunk,
    recording_started: Instant,
) -> tokio::task::JoinHandle<Result<Vec<TranscriptSegment>>> {
    let StreamingChunk { source, utterance } = chunk;
    let source_tag = source.as_str().to_string();
    // Print live header immediately so the user sees activity even before
    // whisper finishes; the actual text follows when transcription returns.
    let elapsed = recording_started.elapsed().as_secs();
    let mins = elapsed / 60;
    let secs = elapsed % 60;
    let label = match source {
        StreamSource::Mic => "YOU",
        StreamSource::System => "THEM",
    };
    println!(
        "  [{:02}:{:02}] [{}] ... ({} ms speech)",
        mins, secs, label, utterance.duration_ms()
    );

    // Use utterance start_ms (relative to VAD session start) as the wall-clock
    // offset so timestamps stay monotonic per source.
    let offset_seconds = utterance.start_ms as f64 / 1000.0;
    tokio::task::spawn_blocking(move || {
        let segments = transcribe_chunk(&utterance.samples, &whisper, &source_tag, offset_seconds)?;
        for seg in &segments {
            println!("  [{}] [{}] {}", seg.timestamp, label, seg.text);
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
