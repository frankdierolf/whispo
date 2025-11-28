use anyhow::{Context, Result};
use std::io::Write;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::sleep;

use crate::audio::{AudioRecorder, AudioResult};
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
    counter: Arc<Mutex<u32>>,
}

impl Service {
    pub fn new(config: Config) -> Result<Self> {
        Ok(Self {
            state: Arc::new(Mutex::new(ServiceState::Idle)),
            recorder: Arc::new(Mutex::new(None)),
            config,
            counter: Arc::new(Mutex::new(0)),
        })
    }

    /// Run the service main loop
    pub async fn run(&self, hotkey_rx: Option<Receiver<()>>) -> Result<()> {
        // Create IPC server
        let ipc_server = IpcServer::new().context("Failed to create IPC server")?;

        println!("whis listening. Ctrl+C to stop.");

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

            // Check for hotkey toggle signal (non-blocking)
            if let Some(ref rx) = hotkey_rx {
                if rx.try_recv().is_ok() {
                    self.handle_toggle().await;
                }
            }

            // Small sleep to prevent busy waiting
            sleep(Duration::from_millis(10)).await;
        }
    }

    /// Handle an IPC message
    async fn handle_message(&self, message: IpcMessage) -> IpcResponse {
        match message {
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
                // Increment counter and start recording
                let count = {
                    let mut c = self.counter.lock().unwrap();
                    *c += 1;
                    *c
                };
                match self.start_recording().await {
                    Ok(_) => {
                        print!("#{count} recording...");
                        let _ = std::io::stdout().flush();
                        IpcResponse::Recording
                    }
                    Err(e) => {
                        println!("#{count} error: {e}");
                        IpcResponse::Error(e.to_string())
                    }
                }
            }
            ServiceState::Recording => {
                // Stop recording and process
                *self.state.lock().unwrap() = ServiceState::Processing;
                let count = *self.counter.lock().unwrap();

                // Show processing state (overwrite recording line)
                print!("\r#{count} processing...");
                let _ = std::io::stdout().flush();

                match self.stop_and_transcribe().await {
                    Ok(_) => {
                        *self.state.lock().unwrap() = ServiceState::Idle;
                        println!("\r#{count} done            ");
                        IpcResponse::Ok
                    }
                    Err(e) => {
                        *self.state.lock().unwrap() = ServiceState::Idle;
                        println!("\r#{count} error: {e}");
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
        let mut recorder = self
            .recorder
            .lock()
            .unwrap()
            .take()
            .context("No active recording")?;

        // Stop and save audio (blocking operation, run in tokio blocking task)
        let audio_result = tokio::task::spawn_blocking(move || recorder.stop_and_save())
            .await
            .context("Failed to join task")??;

        // Transcribe based on result type
        let api_key = self.config.openai_api_key.clone();
        let transcription = match audio_result {
            AudioResult::Single(audio_data) => {
                // Small file - use simple blocking transcription
                tokio::task::spawn_blocking(move || {
                    transcribe::transcribe_audio(&api_key, audio_data)
                })
                .await
                .context("Failed to join task")??
            }
            AudioResult::Chunked(chunks) => {
                // Large file - use parallel async transcription
                transcribe::parallel_transcribe(&api_key, chunks, None).await?
            }
        };

        // Copy to clipboard (blocking operation)
        tokio::task::spawn_blocking(move || clipboard::copy_to_clipboard(&transcription))
            .await
            .context("Failed to join task")??;

        Ok(())
    }
}
