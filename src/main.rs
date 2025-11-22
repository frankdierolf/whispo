mod audio;
mod clipboard;
mod config;
mod ipc;
mod service;
mod transcribe;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::io::{self, Write};

#[derive(Parser)]
#[command(name = "whispo")]
#[command(about = "Voice-to-text CLI using OpenAI Whisper API", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the background service that listens for hotkey triggers
    Listen,

    /// Toggle recording on/off (used by hotkey)
    Toggle,

    /// Stop the background service
    Stop,

    /// Set up GNOME hotkey for toggle command
    SetupHotkey,

    /// Check service status
    Status,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Listen) => run_listen().await,
        Some(Commands::Toggle) => run_toggle(),
        Some(Commands::Stop) => run_stop(),
        Some(Commands::SetupHotkey) => run_setup_hotkey(),
        Some(Commands::Status) => run_status(),
        None => run_record_once().await,
    }
}

/// Run the background service
async fn run_listen() -> Result<()> {
    // Check if FFmpeg is available
    check_ffmpeg()?;

    // Check if service is already running
    if ipc::is_service_running() {
        eprintln!("Error: Whispo service is already running.");
        eprintln!("Use 'whispo stop' to stop the existing service first.");
        std::process::exit(1);
    }

    // Load configuration
    let config = load_config()?;

    // Write PID file
    ipc::write_pid_file()?;

    // Set up cleanup on exit
    let _cleanup = CleanupGuard;

    // Create and run service
    let service = service::Service::new(config)?;

    // Set up Ctrl+C handler
    let service_task = tokio::spawn(async move {
        service.run().await
    });

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

/// Toggle recording (send command to service)
fn run_toggle() -> Result<()> {
    let mut client = ipc::IpcClient::connect()?;
    let response = client.send_message(ipc::IpcMessage::Toggle)?;

    match response {
        ipc::IpcResponse::Recording => {
            println!("🎙️  Recording started");
        }
        ipc::IpcResponse::Ok => {
            println!("✓ Transcription copied to clipboard");
        }
        ipc::IpcResponse::Processing => {
            println!("⏳ Still processing previous recording...");
        }
        ipc::IpcResponse::Error(e) => {
            eprintln!("Error: {e}");
            std::process::exit(1);
        }
        _ => {}
    }

    Ok(())
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
        println!("Start with: whispo listen");
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

/// Set up GNOME hotkey
fn run_setup_hotkey() -> Result<()> {
    println!("Setting up GNOME hotkey for Whispo...\n");

    // Check if we're on GNOME
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    if !desktop.contains("GNOME") {
        println!("Warning: This setup is designed for GNOME desktop environment.");
        println!("You appear to be using: {desktop}");
        println!("\nFor other desktop environments, please configure hotkeys manually:");
        println!("  Command: whispo toggle");
        println!("  Suggested hotkey: Ctrl+Shift+R\n");
        return Ok(());
    }

    // Ask for hotkey preference
    println!("What hotkey would you like to use?");
    println!("1. Ctrl+Shift+R (recommended)");
    println!("2. Ctrl+Alt+R");
    println!("3. Super+R");
    println!("4. Custom");
    print!("\nChoice (1-4): ");
    io::stdout().flush()?;

    let mut choice = String::new();
    io::stdin().read_line(&mut choice)?;

    let binding: String = match choice.trim() {
        "1" | "" => "<Primary><Shift>r".to_string(),
        "2" => "<Primary><Alt>r".to_string(),
        "3" => "<Super>r".to_string(),
        "4" => {
            println!("\nEnter your custom hotkey (e.g., <Primary><Shift>r):");
            print!("> ");
            io::stdout().flush()?;
            let mut custom = String::new();
            io::stdin().read_line(&mut custom)?;
            custom.trim().to_string()
        }
        _ => {
            eprintln!("Invalid choice, using default (Ctrl+Shift+R)");
            "<Primary><Shift>r".to_string()
        }
    };

    // Get whispo binary path
    let whispo_path = std::env::current_exe()?;
    let whispo_cmd = format!("{} toggle", whispo_path.display());

    // Find available custom keybinding slot
    let custom_path = find_available_keybinding_slot()?;

    println!("\nConfiguring GNOME keybinding...");

    // Set the custom keybinding
    let commands = vec![
        format!("gsettings set org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{} name 'Whispo Voice Recording'", custom_path),
        format!("gsettings set org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{} command '{}'", custom_path, whispo_cmd),
        format!("gsettings set org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{} binding '{}'", custom_path, binding),
    ];

    for cmd in &commands {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()?;

        if !output.status.success() {
            eprintln!("Error setting keybinding: {}", String::from_utf8_lossy(&output.stderr));
            std::process::exit(1);
        }
    }

    // Add the custom keybinding to the list
    add_custom_keybinding_to_list(&custom_path)?;

    println!("\n✓ Hotkey configured successfully!");
    println!("\nYour hotkey: {}", binding.replace("<Primary>", "Ctrl+").replace("<Shift>", "Shift+").replace("<Alt>", "Alt+").replace("<Super>", "Super+").replace("r", "R"));
    println!("\nTo use:");
    println!("1. Start the service: whispo listen");
    println!("2. Press your hotkey to start recording");
    println!("3. Press again to stop and transcribe");
    println!("\nYou can change this in GNOME Settings → Keyboard → Keyboard Shortcuts → Custom Shortcuts");

    Ok(())
}

/// Find an available custom keybinding slot
fn find_available_keybinding_slot() -> Result<String> {
    // First, get the list of currently used keybindings
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.settings-daemon.plugins.media-keys", "custom-keybindings"])
        .output()?;

    let current_list = String::from_utf8_lossy(&output.stdout);

    // Try to find an available slot
    for i in 0..20 {
        let path = format!("/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/custom{i}/");

        // Check if this path is in the current list
        if current_list.contains(&format!("custom{i}")) {
            // Slot is in use, check if it's a Whispo binding we can reuse
            let name_output = std::process::Command::new("gsettings")
                .args([
                    "get",
                    &format!("org.gnome.settings-daemon.plugins.media-keys.custom-keybinding:{path}"),
                    "name",
                ])
                .output()?;

            let name = String::from_utf8_lossy(&name_output.stdout);
            if name.contains("Whispo") {
                // Reuse existing Whispo binding
                return Ok(path);
            }
        } else {
            // Slot is not in the list, so it's available
            return Ok(path);
        }
    }

    anyhow::bail!("No available custom keybinding slots found (all 20 slots are occupied)")
}

/// Add custom keybinding to the list
fn add_custom_keybinding_to_list(path: &str) -> Result<()> {
    // Get current list
    let output = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.settings-daemon.plugins.media-keys", "custom-keybindings"])
        .output()?;

    let current_list = String::from_utf8_lossy(&output.stdout);
    let current_list = current_list.trim();

    // Check if our path is already in the list
    if current_list.contains(path) {
        return Ok(());
    }

    // Parse the list and add our path
    let new_list = if current_list == "@as []" || current_list.is_empty() {
        format!("['{path}']")
    } else {
        // Remove the brackets and add our path
        let without_brackets = current_list.trim_start_matches('[').trim_end_matches(']');
        format!("[{without_brackets}, '{path}']")
    };

    // Set the new list
    let output = std::process::Command::new("gsettings")
        .args(["set", "org.gnome.settings-daemon.plugins.media-keys", "custom-keybindings", &new_list])
        .output()?;

    if !output.status.success() {
        anyhow::bail!("Failed to update custom keybindings list: {}", String::from_utf8_lossy(&output.stderr));
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

    // Stop recording and get audio data
    let audio_data = recorder.stop_and_save()?;

    print!("\rTranscribing...                        \n");
    io::stdout().flush()?;

    // Transcribe
    let transcription = match transcribe::transcribe_audio(&config.openai_api_key, audio_data) {
        Ok(text) => text,
        Err(e) => {
            eprintln!("Transcription error: {e}");
            std::process::exit(1);
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
        eprintln!("\nWhispo requires FFmpeg for audio compression.");
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
