use anyhow::{Context, Result};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

use crate::audio::AudioRecorder;
use crate::clipboard;
use crate::config::Config;
use crate::ipc::{IpcMessage, IpcResponse, IpcServer};
use crate::transcribe;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ServiceState {
    Idle,
    Recording,
    Processing,
}

pub struct Service {
    state: Arc<Mutex<ServiceState>>,
    recorder: Arc<Mutex<Option<AudioRecorder>>>,
    config: Config,
}

impl Service {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(ServiceState::Idle)),
            recorder: Arc::new(Mutex::new(None)),
            config,
        })
    }

    /// Run the service main loop
    pub async fn run(&self) -> Result<()> {
        println!("Whispo service starting...");

        // Create IPC server
        let ipc_server = IpcServer::new()
            .context("Failed to create IPC server")?;

        println!("Service running. Press Ctrl+C to stop.");
        println!("Use 'whispo toggle' to start/stop recording.");

        loop {
            // Check for incoming IPC connections (non-blocking)
            if let Some(mut conn) = ipc_server.try_accept()? {
                match conn.receive() {
                    Ok(message) => {
                        let response = self.handle_message(message).await;
                        let _ = conn.send(response);
                    }
                    Err(e) => {
                        eprintln!("Error receiving message: {e}");
                        let _ = conn.send(IpcResponse::Error(e.to_string()));
                    }
                }
            }

            // Small sleep to prevent busy waiting
            sleep(Duration::from_millis(10)).await;
        }
    }

    /// Handle an IPC message
    async fn handle_message(&self, message: IpcMessage) -> IpcResponse {
        match message {
            IpcMessage::Toggle => self.handle_toggle().await,
            IpcMessage::Stop => {
                println!("Stop signal received");
                // Return Ok response before exiting
                tokio::spawn(async {
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                    std::process::exit(0);
                });
                IpcResponse::Ok
            }
            IpcMessage::Status => {
                let state = *self.state.lock().unwrap();
                match state {
                    ServiceState::Idle => IpcResponse::Idle,
                    ServiceState::Recording => IpcResponse::Recording,
                    ServiceState::Processing => IpcResponse::Processing,
                }
            }
        }
    }

    /// Handle toggle command (start/stop recording)
    async fn handle_toggle(&self) -> IpcResponse {
        let current_state = *self.state.lock().unwrap();

        match current_state {
            ServiceState::Idle => {
                // Start recording
                match self.start_recording().await {
                    Ok(_) => {
                        println!("🎙️  Recording started");
                        IpcResponse::Recording
                    }
                    Err(e) => {
                        eprintln!("Error starting recording: {e}");
                        IpcResponse::Error(e.to_string())
                    }
                }
            }
            ServiceState::Recording => {
                // Stop recording and process
                *self.state.lock().unwrap() = ServiceState::Processing;
                println!("⏹️  Recording stopped, processing...");

                match self.stop_and_transcribe().await {
                    Ok(_) => {
                        *self.state.lock().unwrap() = ServiceState::Idle;
                        println!("✓ Transcription copied to clipboard");
                        IpcResponse::Ok
                    }
                    Err(e) => {
                        *self.state.lock().unwrap() = ServiceState::Idle;
                        eprintln!("Error processing: {e}");
                        IpcResponse::Error(e.to_string())
                    }
                }
            }
            ServiceState::Processing => {
                // Already processing, ignore
                IpcResponse::Processing
            }
        }
    }

    /// Start recording audio
    async fn start_recording(&self) -> Result<()> {
        let mut recorder = AudioRecorder::new()?;
        recorder.start_recording()?;

        *self.recorder.lock().unwrap() = Some(recorder);
        *self.state.lock().unwrap() = ServiceState::Recording;

        Ok(())
    }

    /// Stop recording and transcribe
    async fn stop_and_transcribe(&self) -> Result<()> {
        // Get the recorder
        let recorder = self.recorder.lock().unwrap().take()
            .context("No active recording")?;

        // Stop and save audio (blocking operation, run in tokio blocking task)
        let audio_data = tokio::task::spawn_blocking(move || {
            recorder.stop_and_save()
        })
        .await
        .context("Failed to join task")??;

        // Transcribe (blocking operation)
        let api_key = self.config.openai_api_key.clone();
        let transcription = tokio::task::spawn_blocking(move || {
            transcribe::transcribe_audio(&api_key, audio_data)
        })
        .await
        .context("Failed to join task")??;

        // Copy to clipboard (blocking operation)
        tokio::task::spawn_blocking(move || {
            clipboard::copy_to_clipboard(&transcription)
        })
        .await
        .context("Failed to join task")??;

        Ok(())
    }
}
