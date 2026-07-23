//! [`CloudHypervisor`]: the production [`Hypervisor`] implementation.
//!
//! It composes the three separated concerns (ADR / section 43): process
//! management ([`crate::process`]), the API client ([`crate::socket`]), and
//! configuration translation ([`crate::config`]). It never shells out to
//! `ch-remote` (section 10).

use std::time::Duration;

use async_trait::async_trait;
use ch_model::VirtualMachineSpec;
use tracing::{debug, info};

use crate::config::{self, TranslateOptions, VmInfo, VmState};
use crate::error::Result;
use crate::hypervisor::{Hypervisor, HypervisorVmInfo};
use crate::process::{ChProcess, ProcessConfig};
use crate::socket::ApiClient;

/// How long to wait for a freshly launched VMM's API socket by default.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(10);
/// How often to poll the API socket while waiting.
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Inputs for launching a brand-new VMM instance.
#[derive(Debug, Clone)]
pub struct LaunchConfig {
    pub process: ProcessConfig,
    pub translate: TranslateOptions,
    pub ready_timeout: Duration,
    pub poll_interval: Duration,
}

impl LaunchConfig {
    /// Construct with default readiness timing.
    pub fn new(process: ProcessConfig, translate: TranslateOptions) -> Self {
        Self {
            process,
            translate,
            ready_timeout: DEFAULT_READY_TIMEOUT,
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }
}

/// A handle to one Cloud Hypervisor VM instance.
pub struct CloudHypervisor {
    api: ApiClient,
    /// Present when this handle launched the VMM; absent when re-attached to an
    /// already-running VMM after an agent restart (section 11).
    process: Option<ChProcess>,
    translate: TranslateOptions,
}

impl CloudHypervisor {
    /// Launch a new `cloud-hypervisor` process and wait for its API socket.
    pub async fn launch(config: LaunchConfig) -> Result<Self> {
        let process = ChProcess::spawn(&config.process).await?;
        info!(pid = process.pid(), socket = %config.process.api_socket.display(), "launched cloud-hypervisor");

        let api = ApiClient::new(config.process.api_socket.clone());
        api.wait_ready(config.ready_timeout, config.poll_interval)
            .await?;
        debug!("cloud-hypervisor API socket ready");

        Ok(Self {
            api,
            process: Some(process),
            translate: config.translate,
        })
    }

    /// Re-attach to an already-running VMM by its API socket (recovery after an
    /// agent restart, section 11). The VMM process is not owned by this handle.
    pub fn attach(api_socket: impl Into<std::path::PathBuf>, translate: TranslateOptions) -> Self {
        Self {
            api: ApiClient::new(api_socket),
            process: None,
            translate,
        }
    }

    /// The pid of the VMM, when this handle owns the process.
    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().map(ChProcess::pid)
    }

    /// Terminate the VMM process, if owned by this handle.
    pub async fn kill(&mut self) -> Result<()> {
        if let Some(proc) = self.process.as_mut() {
            proc.kill().await?;
        }
        Ok(())
    }

    /// Fetch raw CH VM info, or `None` when no VM exists / the VMM is
    /// unreachable. Used for idempotency checks.
    async fn current_state(&self) -> Option<VmState> {
        self.api
            .get_json::<VmInfo>("/vm.info")
            .await
            .ok()
            .map(|i| i.state)
    }
}

#[async_trait]
impl Hypervisor for CloudHypervisor {
    async fn create(&self, spec: &VirtualMachineSpec) -> Result<()> {
        // Idempotent: if a VM already exists on this VMM, do nothing.
        if self.current_state().await.is_some() {
            debug!("vm already created; skipping vm.create");
            return Ok(());
        }
        let cfg = config::to_vm_config(spec, &self.translate);
        self.api.put_json("/vm.create", &cfg).await
    }

    async fn boot(&self) -> Result<()> {
        if self.current_state().await == Some(VmState::Running) {
            debug!("vm already running; skipping vm.boot");
            return Ok(());
        }
        self.api.put_empty("/vm.boot").await
    }

    async fn shutdown(&self) -> Result<()> {
        match self.current_state().await {
            // Already down, or VMM/VM gone entirely.
            Some(VmState::Shutdown) | None => Ok(()),
            _ => self.api.put_empty("/vm.shutdown").await,
        }
    }

    async fn info(&self) -> Result<HypervisorVmInfo> {
        let info: VmInfo = self.api.get_json("/vm.info").await?;
        Ok(info.into())
    }
}
