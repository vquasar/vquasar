//! Management of the `cloud-hypervisor` child process.
//!
//! One process backs one VM. Crucially, dropping a [`ChProcess`] does **not**
//! kill the VMM: `tokio`'s `Child` only kills on drop when `kill_on_drop` is
//! set, and we deliberately leave it unset so that VMs survive `ch-agent`
//! restarts (design document, section 11, ADR-002). The process is only
//! terminated through the explicit [`ChProcess::kill`] call.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};

use crate::error::{ChError, Result};

/// Inputs for launching a `cloud-hypervisor` process.
#[derive(Debug, Clone)]
pub struct ProcessConfig {
    /// Path to the `cloud-hypervisor` binary.
    pub binary: PathBuf,
    /// Path the VMM should create its API socket at (passed as `--api-socket`).
    pub api_socket: PathBuf,
    /// Optional file to redirect the VMM's stdout/stderr into.
    pub log_file: Option<PathBuf>,
    /// Additional raw arguments (reserved for later; empty for the MVP).
    pub extra_args: Vec<String>,
}

/// A launched `cloud-hypervisor` process.
#[derive(Debug)]
pub struct ChProcess {
    pid: u32,
    api_socket: PathBuf,
    child: Child,
}

impl ChProcess {
    /// Spawn a new `cloud-hypervisor` process.
    ///
    /// The VMM is started with only its API socket; the actual VM is created
    /// afterwards through the API (`vm.create`). The API socket will not exist
    /// immediately — callers should wait for it via
    /// [`ApiClient::wait_ready`](crate::socket::ApiClient::wait_ready).
    pub async fn spawn(config: &ProcessConfig) -> Result<Self> {
        let mut command = Command::new(&config.binary);
        command
            .arg("--api-socket")
            .arg(&config.api_socket)
            .args(&config.extra_args)
            .kill_on_drop(false)
            .stdin(Stdio::null());

        match &config.log_file {
            Some(path) => {
                let stdout = std::fs::File::create(path).map_err(ChError::Io)?;
                let stderr = stdout.try_clone().map_err(ChError::Io)?;
                command
                    .stdout(Stdio::from(stdout))
                    .stderr(Stdio::from(stderr));
            }
            None => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }

        let child = command.spawn().map_err(ChError::Spawn)?;
        let pid = child.id().ok_or_else(|| {
            ChError::Spawn(std::io::Error::other("child exited before reporting a pid"))
        })?;

        Ok(Self {
            pid,
            api_socket: config.api_socket.clone(),
            child,
        })
    }

    /// The OS process id of the running VMM.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// The API socket path this process was launched with.
    pub fn api_socket(&self) -> &Path {
        &self.api_socket
    }

    /// Forcibly terminate the VMM process and reap it.
    pub async fn kill(&mut self) -> Result<()> {
        self.child.kill().await.map_err(ChError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn spawn_reports_pid_and_socket() {
        // Use `/bin/sleep` as a stand-in VMM: it stays alive so we can observe
        // a pid, and it ignores the extra arguments.
        let cfg = ProcessConfig {
            binary: "/bin/sleep".into(),
            api_socket: "/tmp/ch-orchestrator-test.sock".into(),
            log_file: None,
            extra_args: vec!["30".into()],
        };
        // Skip when the stand-in binary is unavailable (unusual, but possible
        // in minimal build sandboxes).
        if !Path::new(&cfg.binary).exists() {
            return;
        }
        let mut proc = ChProcess::spawn(&cfg).await.expect("spawn");
        assert!(proc.pid() > 0);
        assert_eq!(
            proc.api_socket(),
            Path::new("/tmp/ch-orchestrator-test.sock")
        );
        proc.kill().await.expect("kill");
    }
}
