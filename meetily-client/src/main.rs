use anyhow::Result;
use clap::{Parser, Subcommand};
use meetily_client::audio::capture::record_dual_stream;
use meetily_client::audio::devices::list_devices;
use meetily_client::transcribe::{
    download_model, get_model_path, load_model, merge_segments, transcribe_wav,
};
use meetily_client::upload::{
    create_meeting, end_meeting, trigger_summarize, upload_transcript,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

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
        } => {
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
            let whisper = load_model(&model_path)?;
            println!("Model loaded.");

            println!("\n>>> Recording started. Press Ctrl+C to stop. <<<\n");
            let stop = CancellationToken::new();
            let recording = tokio::spawn(record_dual_stream(mic, system, stop.clone()));

            tokio::signal::ctrl_c().await?;
            println!("\n>>> Recording stopped. <<<\n");
            stop.cancel();

            let (mic_wav, system_wav) = recording.await??;

            // Transcribe mic
            println!("Transcribing microphone audio...");
            let mic_segments = transcribe_wav(&mic_wav, &whisper, "mic")?;
            if mic_segments.is_empty() {
                println!("  (no speech detected from mic)");
            } else {
                println!("\n--- Mic Transcript ({} segments) ---", mic_segments.len());
                for seg in &mic_segments {
                    println!("  [{}] {}", seg.timestamp, seg.text);
                }
                println!("---\n");
            }

            // Transcribe system
            let system_segments = if system_wav.metadata().map(|m| m.len()).unwrap_or(0) > 44 {
                println!("Transcribing system audio...");
                let segs = transcribe_wav(&system_wav, &whisper, "system")?;
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

            // Merge and display combined
            let segments = merge_segments(mic_segments, system_segments);
            if !segments.is_empty() {
                println!("\n=== Combined Transcript ({} segments) ===", segments.len());
                for seg in &segments {
                    let label = if seg.source == "mic" { "YOU" } else { "THEM" };
                    println!("  [{}] [{}] {}", seg.timestamp, label, seg.text);
                }
                println!("===\n");
            }

            // Upload
            println!("Uploading transcript to server...");
            upload_transcript(&server, &meeting_id, &segments).await?;
            println!("Uploaded {} segments.", segments.len());

            // Cleanup temp files
            delete_temp_wav(&mic_wav).await;
            delete_temp_wav(&system_wav).await;

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

async fn delete_temp_wav(path: &std::path::Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => log::warn!("failed to delete temp WAV {}: {err}", path.display()),
    }
}
