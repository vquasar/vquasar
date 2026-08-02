//! Per-VM serial console hub (design document, sections 9 and 25).
//!
//! Cloud Hypervisor exposes each VM's serial port on a Unix socket that accepts
//! a single client. The agent owns that single connection and fans it out: a
//! background task connects to the socket, tees all output to `serial.log`
//! (preserving boot logs) and broadcasts it to any console subscribers, while
//! console input from subscribers is written back to the guest.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, warn};

/// A live serial console for one VM.
#[derive(Clone)]
pub struct SerialHub {
    output: broadcast::Sender<Vec<u8>>,
    input: mpsc::Sender<Vec<u8>>,
}

impl SerialHub {
    /// Start the hub: connect to `socket_path` (retrying until it appears),
    /// tee output to `log_path`, and pump console input back to the guest.
    pub fn start(socket_path: PathBuf, log_path: PathBuf) -> Self {
        let (output_tx, _) = broadcast::channel::<Vec<u8>>(2048);
        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>(256);

        let out = output_tx.clone();
        tokio::spawn(async move {
            run(socket_path, log_path, out, input_rx).await;
        });

        Self {
            output: output_tx,
            input: input_tx,
        }
    }

    /// Subscribe to serial output (from now on).
    pub fn subscribe(&self) -> broadcast::Receiver<Vec<u8>> {
        self.output.subscribe()
    }

    /// A sender for console input (keystrokes) to the guest.
    pub fn input_sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.input.clone()
    }
}

async fn run(
    socket_path: PathBuf,
    log_path: PathBuf,
    output: broadcast::Sender<Vec<u8>>,
    mut input_rx: mpsc::Receiver<Vec<u8>>,
) {
    // Open the log once and append across reconnects so boot logs are preserved.
    let mut log = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .await
        .ok();

    // Reconnect loop: Cloud Hypervisor recreates the serial socket on guest
    // reboot (and closes the old connection), so a single connect is not enough
    // — we must re-attach or the console goes permanently dead (section 25).
    loop {
        let Some(stream) = connect_retry(&socket_path).await else {
            warn!(socket = %socket_path.display(), "serial socket never became available");
            return;
        };
        debug!(socket = %socket_path.display(), "serial hub connected");
        let (mut reader, mut writer) = stream.into_split();
        let mut buf = vec![0u8; 4096];

        // Pump both directions on one connection; select so a dropped read tears
        // down this connection and returns us to the reconnect loop, while a
        // closed input channel (hub dropped when the VM is removed) ends the task.
        loop {
            tokio::select! {
                r = reader.read(&mut buf) => match r {
                    Ok(0) | Err(_) => break, // disconnected -> reconnect
                    Ok(n) => {
                        let chunk = buf[..n].to_vec();
                        if let Some(f) = log.as_mut() {
                            let _ = f.write_all(&chunk).await;
                        }
                        // Ignore send errors: there may simply be no subscribers.
                        let _ = output.send(chunk);
                    }
                },
                msg = input_rx.recv() => match msg {
                    None => return, // all senders dropped: VM gone, stop for good
                    Some(bytes) => {
                        let _ = writer.write_all(&bytes).await;
                    }
                },
            }
        }
        debug!(socket = %socket_path.display(), "serial hub disconnected; reconnecting");
        // Brief pause so CH can recreate the socket before we retry.
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Connect to the serial socket, retrying while Cloud Hypervisor creates it.
async fn connect_retry(path: &PathBuf) -> Option<UnixStream> {
    for _ in 0..300 {
        if let Ok(stream) = UnixStream::connect(path).await {
            return Some(stream);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}
