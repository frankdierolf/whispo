use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub enum IpcMessage {
    Stop,
    Status,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IpcResponse {
    Ok,
    Recording,
    Idle,
    Processing,
    Error(String),
}

/// Get the socket path for IPC communication
pub fn get_socket_path() -> PathBuf {
    // Use XDG_RUNTIME_DIR if available, otherwise fall back to /tmp
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("whis.sock")
}

/// Get the PID file path
pub fn get_pid_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(runtime_dir).join("whis.pid")
}

/// IPC Server for the background service
pub struct IpcServer {
    listener: UnixListener,
}

impl IpcServer {
    pub fn new() -> Result<Self> {
        let socket_path = get_socket_path();

        // Remove old socket if it exists
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).context("Failed to remove old socket file")?;
        }

        let listener = UnixListener::bind(&socket_path).context("Failed to bind Unix socket")?;

        // Set non-blocking mode for the listener
        listener
            .set_nonblocking(true)
            .context("Failed to set non-blocking mode")?;

        Ok(Self { listener })
    }

    /// Try to accept a new connection (non-blocking)
    pub fn try_accept(&self) -> Result<Option<IpcConnection>> {
        match self.listener.accept() {
            Ok((stream, _)) => Ok(Some(IpcConnection { stream })),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let socket_path = get_socket_path();
        let _ = std::fs::remove_file(socket_path);
    }
}

/// IPC Connection for handling individual client connections
pub struct IpcConnection {
    stream: UnixStream,
}

impl IpcConnection {
    /// Receive a message from the client
    pub fn receive(&mut self) -> Result<IpcMessage> {
        let mut reader = BufReader::new(&self.stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("Failed to read from socket")?;

        serde_json::from_str(line.trim()).context("Failed to deserialize message")
    }

    /// Send a response to the client
    pub fn send(&mut self, response: IpcResponse) -> Result<()> {
        let json = serde_json::to_string(&response)?;
        writeln!(self.stream, "{json}").context("Failed to write to socket")?;
        self.stream.flush().context("Failed to flush socket")?;
        Ok(())
    }
}

/// IPC Client for sending commands to the background service
pub struct IpcClient {
    stream: UnixStream,
}

impl IpcClient {
    pub fn connect() -> Result<Self> {
        let socket_path = get_socket_path();

        if !socket_path.exists() {
            anyhow::bail!(
                "whis service is not running.\n\
                Start it with: whis listen"
            );
        }

        let stream = UnixStream::connect(&socket_path).with_context(|| {
            // If socket exists but connection fails, it's likely stale
            "Failed to connect to whis service.\n\
                The service may have crashed. Try removing stale files:\n\
                  rm -f $XDG_RUNTIME_DIR/whis.*\n\
                Then start the service again with: whis listen"
        })?;

        Ok(Self { stream })
    }

    pub fn send_message(&mut self, message: IpcMessage) -> Result<IpcResponse> {
        // Send message
        let json = serde_json::to_string(&message)?;
        writeln!(self.stream, "{json}").context("Failed to send message")?;
        self.stream.flush().context("Failed to flush stream")?;

        // Receive response
        let mut reader = BufReader::new(&self.stream);
        let mut line = String::new();
        reader
            .read_line(&mut line)
            .context("Failed to read response")?;

        serde_json::from_str(line.trim()).context("Failed to deserialize response")
    }
}

/// Check if the service is already running
pub fn is_service_running() -> bool {
    let socket_path = get_socket_path();

    if !socket_path.exists() {
        return false;
    }

    // Socket exists, but check if it's actually connectable
    match UnixStream::connect(&socket_path) {
        Ok(_) => {
            // Successfully connected, service is running
            true
        }
        Err(_) => {
            // Socket exists but can't connect - it's stale
            // Clean up stale socket and PID files
            let _ = std::fs::remove_file(&socket_path);
            remove_pid_file();
            false
        }
    }
}

/// Write PID file
pub fn write_pid_file() -> Result<()> {
    let pid_path = get_pid_path();
    let pid = std::process::id();
    std::fs::write(&pid_path, pid.to_string()).context("Failed to write PID file")?;
    Ok(())
}

/// Remove PID file
pub fn remove_pid_file() {
    let pid_path = get_pid_path();
    let _ = std::fs::remove_file(pid_path);
}
