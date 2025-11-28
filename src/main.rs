mod audio;
mod clipboard;
mod config;
mod hotkey;
mod ipc;
mod service;
mod transcribe;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "whis")]
#[command(version)]
#[command(about = "Voice-to-text CLI using OpenAI Whisper API")]
#[command(after_help = "Run 'whis' without arguments to record once (press Enter to stop).")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the background service that listens for hotkey triggers
    Listen {
        /// Hotkey to trigger recording (e.g., "ctrl+shift+r")
        #[arg(short = 'k', long, default_value = "ctrl+shift+r")]
        hotkey: String,
    },

    /// Stop the background service
    Stop,

    /// Check service status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Listen { hotkey }) => run_listen(hotkey).await,
        Some(Commands::Stop) => run_stop(),
        Some(Commands::Status) => run_status(),
        None => run_record_once().await,
    }
}

/// Run the background service
async fn run_listen(hotkey_str: String) -> Result<()> {
    // Check if FFmpeg is available
    check_ffmpeg()?;

    // Check if service is already running
    if ipc::is_service_running() {
        eprintln!("Error: whis service is already running.");
        eprintln!("Use 'whis stop' to stop the existing service first.");
        std::process::exit(1);
    }

    // Parse and validate hotkey
    let hotkey = hotkey::Hotkey::parse(&hotkey_str)?;

    // Load configuration
    let config = load_config()?;

    // Write PID file
    ipc::write_pid_file()?;

    // Set up cleanup on exit
    let _cleanup = CleanupGuard;

    // Create channel for hotkey signals
    let (hotkey_tx, hotkey_rx) = std::sync::mpsc::channel();

    // Spawn hotkey listener thread
    std::thread::spawn(move || {
        if let Err(e) = hotkey::listen_for_hotkey(hotkey, move || {
            let _ = hotkey_tx.send(());
        }) {
            eprintln!("Hotkey error: {e}");
        }
    });

    // Create and run service
    let service = service::Service::new(config)?;

    // Set up Ctrl+C handler
    let service_task = tokio::spawn(async move { service.run(Some(hotkey_rx)).await });

    // Wait for Ctrl+C
    tokio::select! {
        result = service_task => {
            // Service exited on its own
            result?
        }
        _ = tokio::signal::ctrl_c() => {
            println!("\nShutting down...");
            Ok(())
        }
    }
}

/// Stop the service
fn run_stop() -> Result<()> {
    let mut client = ipc::IpcClient::connect()?;
    let _ = client.send_message(ipc::IpcMessage::Stop)?;
    println!("Service stopped");
    Ok(())
}

/// Check service status
fn run_status() -> Result<()> {
    if !ipc::is_service_running() {
        println!("Status: Not running");
        println!("Start with: whis listen");
        return Ok(());
    }

    let mut client = ipc::IpcClient::connect()?;
    let response = client.send_message(ipc::IpcMessage::Status)?;

    match response {
        ipc::IpcResponse::Idle => println!("Status: Running (idle)"),
        ipc::IpcResponse::Recording => println!("Status: Running (recording)"),
        ipc::IpcResponse::Processing => println!("Status: Running (processing)"),
        ipc::IpcResponse::Error(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        _ => println!("Status: Running"),
    }

    Ok(())
}

/// Run the original one-time recording mode
async fn run_record_once() -> Result<()> {
    // Check if FFmpeg is available
    check_ffmpeg()?;

    // Load configuration
    let config = load_config()?;

    // Create recorder and start recording
    let mut recorder = audio::AudioRecorder::new()?;
    recorder.start_recording()?;

    print!("Recording... (press Enter to stop)");
    io::stdout().flush()?;
    wait_for_enter()?;

    // Stop recording and get audio result
    let audio_result = recorder.stop_and_save()?;

    // Transcribe based on result type
    let transcription = match audio_result {
        audio::AudioResult::Single(audio_data) => {
            // Small file - simple transcription
            print!("\rTranscribing...                        \n");
            io::stdout().flush()?;

            match transcribe::transcribe_audio(&config.openai_api_key, audio_data) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("Transcription error: {e}");
                    std::process::exit(1);
                }
            }
        }
        audio::AudioResult::Chunked(chunks) => {
            // Large file - parallel transcription
            print!("\rTranscribing...                        \n");
            io::stdout().flush()?;

            match transcribe::parallel_transcribe(&config.openai_api_key, chunks, None).await {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("Transcription error: {e}");
                    std::process::exit(1);
                }
            }
        }
    };

    // Copy to clipboard
    clipboard::copy_to_clipboard(&transcription)?;

    println!("Copied to clipboard");

    Ok(())
}

fn check_ffmpeg() -> Result<()> {
    if std::process::Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("Error: FFmpeg is not installed or not in PATH.");
        eprintln!("\nwhis requires FFmpeg for audio compression.");
        eprintln!("Please install FFmpeg:");
        eprintln!("  - Ubuntu/Debian: sudo apt install ffmpeg");
        eprintln!("  - macOS: brew install ffmpeg");
        eprintln!("  - Or visit: https://ffmpeg.org/download.html\n");
        std::process::exit(1);
    }
    Ok(())
}

fn load_config() -> Result<config::Config> {
    match config::Config::from_env() {
        Ok(cfg) => Ok(cfg),
        Err(e) => {
            eprintln!("Error loading configuration: {e}");
            eprintln!("\nPlease create a .env file with your OpenAI API key:");
            eprintln!("  OPENAI_API_KEY=your-api-key-here\n");
            std::process::exit(1);
        }
    }
}

fn wait_for_enter() -> Result<()> {
    let mut input = String::new();
    io::stdout().flush()?;
    io::stdin().read_line(&mut input)?;
    Ok(())
}

/// Guard to clean up PID and socket files on exit
struct CleanupGuard;

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        ipc::remove_pid_file();
    }
}
