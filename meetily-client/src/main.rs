use anyhow::Result;
use clap::{Parser, Subcommand};

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
            mic,
            system,
            model,
        } => {
            println!(
                "record server={server} mic={:?} system={:?} model={model}",
                mic, system
            );
        }
        Commands::Devices => {
            println!("Audio device listing is not implemented yet.");
        }
        Commands::DownloadModel { model_name } => {
            println!("Model download is not implemented yet: {model_name}");
        }
    }

    Ok(())
}
