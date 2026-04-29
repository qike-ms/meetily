use anyhow::Result;
use clap::{Parser, Subcommand};
use meetily_client::audio::capture::record_dual_stream;
use meetily_client::audio::devices::list_devices;
use meetily_client::transcribe::{
    download_model, get_model_path, merge_segments, transcribe_wav,
};
use meetily_client::upload::{
    create_meeting, end_meeting, trigger_summarize, upload_transcript_and_get_meeting_id,
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
            if mic.is_none() || system.is_none() {
                println!("Available audio devices:");
                for device in list_devices() {
                    println!("  {device}");
                }
                println!();
            }

            let client_id = Uuid::new_v4().to_string();
            let meeting_id = create_meeting(&server, &title, &client_id).await?;
            let model_path = get_model_path(&model);
            if !model_path.exists() {
                anyhow::bail!(
                    "Whisper model not found at {}. Run `meetily-client download-model {}` first.",
                    model_path.display(),
                    model
                );
            }

            println!("Recording meeting {meeting_id}. Press Ctrl+C to stop.");
            let stop = CancellationToken::new();
            let recording = tokio::spawn(record_dual_stream(mic, system, stop.clone()));

            tokio::signal::ctrl_c().await?;
            println!("Stopping recording...");
            stop.cancel();

            let (mic_wav, system_wav) = recording.await??;
            println!("Transcribing microphone audio: {}", mic_wav.display());
            let mic_segments = transcribe_wav(&mic_wav, &model_path, "mic")?;

            let system_segments = if system_wav.metadata().map(|m| m.len()).unwrap_or(0) > 44 {
                println!("Transcribing system audio: {}", system_wav.display());
                transcribe_wav(&system_wav, &model_path, "system")?
            } else {
                Vec::new()
            };

            let segments = merge_segments(mic_segments, system_segments);
            let saved_meeting_id =
                upload_transcript_and_get_meeting_id(&server, &meeting_id, &segments).await?;
            trigger_summarize(&server, &saved_meeting_id).await?;
            end_meeting(&server, &saved_meeting_id).await?;

            println!("Meeting complete.");
            println!("  Meeting ID: {saved_meeting_id}");
            println!("  Mic WAV: {}", mic_wav.display());
            println!("  System WAV: {}", system_wav.display());
            println!("  Segments uploaded: {}", segments.len());
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
