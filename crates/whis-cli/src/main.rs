// Background service modules - only available on Linux
#[cfg(target_os = "linux")]
mod hotkey;
#[cfg(target_os = "linux")]
mod ipc;
#[cfg(target_os = "linux")]
mod service;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, Write};
use whis_core::{
    AudioRecorder, RecordingOutput, ApiConfig, copy_to_clipboard, parallel_transcribe, transcribe_audio,
};

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
    #[cfg(target_os = "linux")]
    Listen {
        /// Hotkey to trigger recording (e.g., "ctrl+shift+r")
        #[arg(short = 'k', long, default_value = "ctrl+shift+r")]
        hotkey: String,
    },

    /// Stop the background service
    #[cfg(target_os = "linux")]
    Stop,

    /// Check service status
    #[cfg(target_os = "linux")]
    Status,

    /// Configure settings (API key, etc.)
    Config {
        /// Set your OpenAI API key
        #[arg(long)]
        api_key: Option<String>,

        /// Show current configuration
        #[arg(long)]
        show: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        #[cfg(target_os = "linux")]
        Some(Commands::Listen { hotkey }) => run_listen(hotkey).await,
        #[cfg(target_os = "linux")]
        Some(Commands::Stop) => run_stop(),
        #[cfg(target_os = "linux")]
        Some(Commands::Status) => run_status(),
        Some(Commands::Config { api_key, show }) => run_config(api_key, show),
        None => run_record_once().await,
    }
}

/// Run the background service
#[cfg(target_os = "linux")]
async fn run_listen(hotkey_str: String) -> Result<()> {
    // Check if FFmpeg is available
    ensure_ffmpeg_installed()?;

    // Check if service is already running
    if ipc::is_service_running() {
        eprintln!("Error: whis service is already running.");
        eprintln!("Use 'whis stop' to stop the existing service first.");
        std::process::exit(1);
    }

    // Parse and validate hotkey
    let hotkey = hotkey::Hotkey::parse(&hotkey_str)?;

    // Load API configuration
    let config = load_api_config()?;

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
#[cfg(target_os = "linux")]
fn run_stop() -> Result<()> {
    let mut client = ipc::IpcClient::connect()?;
    let _ = client.send_message(ipc::IpcMessage::Stop)?;
    println!("Service stopped");
    Ok(())
}

/// Check service status
#[cfg(target_os = "linux")]
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
        ipc::IpcResponse::Transcribing => println!("Status: Running (transcribing)"),
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
    ensure_ffmpeg_installed()?;

    // Load API configuration
    let config = load_api_config()?;

    // Create recorder and start recording
    let mut recorder = AudioRecorder::new()?;
    recorder.start_recording()?;

    print!("Recording... (press Enter to stop)");
    io::stdout().flush()?;
    wait_for_enter()?;

    // Finalize recording and get output
    let audio_result = recorder.finalize_recording()?;

    // Transcribe based on output type
    let transcription = match audio_result {
        RecordingOutput::Single(audio_data) => {
            // Small file - simple transcription
            print!("\rTranscribing...                        \n");
            io::stdout().flush()?;

            match transcribe_audio(&config.openai_api_key, audio_data) {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("Transcription error: {e}");
                    std::process::exit(1);
                }
            }
        }
        RecordingOutput::Chunked(chunks) => {
            // Large file - parallel transcription
            print!("\rTranscribing...                        \n");
            io::stdout().flush()?;

            match parallel_transcribe(&config.openai_api_key, chunks, None).await {
                Ok(text) => text,
                Err(e) => {
                    eprintln!("Transcription error: {e}");
                    std::process::exit(1);
                }
            }
        }
    };

    // Copy to clipboard
    copy_to_clipboard(&transcription)?;

    println!("Copied to clipboard");

    Ok(())
}

fn ensure_ffmpeg_installed() -> Result<()> {
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

fn load_api_config() -> Result<ApiConfig> {
    use whis_core::Settings;

    // Priority: settings file > environment variable
    let settings = Settings::load();
    if let Some(key) = settings.openai_api_key {
        return Ok(ApiConfig { openai_api_key: key });
    }

    // Fallback to environment
    match ApiConfig::from_env() {
        Ok(cfg) => Ok(cfg),
        Err(_) => {
            eprintln!("Error: No API key configured.");
            eprintln!("\nSet your key with:");
            eprintln!("  whis config --api-key YOUR_KEY\n");
            eprintln!("Or set the OPENAI_API_KEY environment variable.");
            std::process::exit(1);
        }
    }
}

/// Configure settings
fn run_config(api_key: Option<String>, show: bool) -> Result<()> {
    use whis_core::Settings;

    if let Some(key) = api_key {
        // Validate format
        if !key.starts_with("sk-") {
            eprintln!("Invalid key format. OpenAI keys start with 'sk-'");
            std::process::exit(1);
        }

        let mut settings = Settings::load();
        settings.openai_api_key = Some(key);
        settings.save()?;
        println!("API key saved to {}", Settings::path().display());
        return Ok(());
    }

    if show {
        let settings = Settings::load();
        println!("Config file: {}", Settings::path().display());
        println!("Shortcut: {}", settings.shortcut);
        if let Some(key) = &settings.openai_api_key {
            let masked = if key.len() > 10 {
                format!("{}...{}", &key[..6], &key[key.len() - 4..])
            } else {
                "***".to_string()
            };
            println!("API key: {masked}");
        } else {
            println!("API key: (not set, using $OPENAI_API_KEY)");
        }
        return Ok(());
    }

    // No flags - show help
    eprintln!("Usage: whis config --api-key <KEY>");
    eprintln!("       whis config --show");
    std::process::exit(1);
}

fn wait_for_enter() -> Result<()> {
    let mut input = String::new();
    io::stdout().flush()?;
    io::stdin().read_line(&mut input)?;
    Ok(())
}

/// Guard to clean up PID and socket files on exit
#[cfg(target_os = "linux")]
struct CleanupGuard;

#[cfg(target_os = "linux")]
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        ipc::remove_pid_file();
    }
}
