//! The hypervisor backend the [`VmManager`](crate::manager::VmManager) drives.
//!
//! [`ManagedVmm`] extends the per-VM operations of [`vquasar_client::Hypervisor`]
//! with process-lifecycle control (`terminate`, `pid`) that the manager needs
//! but the pure VM-API trait deliberately omits. A [`Backend`] launches or
//! re-attaches these handles; the real one drives Cloud Hypervisor, the fake
//! one is for tests without `/dev/kvm`.

use async_trait::async_trait;
use std::path::PathBuf;

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

use vquasar_client::config::TranslateOptions;
use vquasar_client::{
    CloudHypervisor, FakeHypervisor, Hypervisor, HypervisorVmInfo, LaunchConfig, ProcessConfig,
    SerialTarget, TapBinding,
};
use vquasar_model::{DiskSpec, VirtualMachineSpec, VmId};

use crate::runtime::RuntimeLayout;

type Result<T> = vquasar_client::Result<T>;

/// A per-VM hypervisor handle the manager can fully control, including tearing
/// down the underlying process.
#[async_trait]
pub trait ManagedVmm: Send + Sync {
    async fn create(&self, spec: &VirtualMachineSpec) -> Result<()>;
    async fn boot(&self) -> Result<()>;
    async fn shutdown(&self) -> Result<()>;
    async fn info(&self) -> Result<HypervisorVmInfo>;
    /// Send this running VM's live state to `destination_url` (section 28).
    async fn send_migration(&self, destination_url: &str) -> Result<()>;
    /// Hot-plug: resize vCPUs and/or guest RAM on a running VM (section M10).
    async fn resize(&self, desired_vcpus: Option<u32>, desired_ram: Option<u64>) -> Result<()>;
    /// Hot-add a disk to a running VM (section M10).
    async fn add_disk(&self, disk: &DiskSpec) -> Result<()>;
    /// Hot-add a NIC bound to a prepared host TAP (section M10).
    async fn add_net(&self, tap: &TapBinding) -> Result<()>;
    /// Terminate the underlying VMM process (no-op when detached or fake).
    async fn terminate(&mut self) -> Result<()>;
    fn pid(&self) -> Option<u32>;
    /// Cumulative CH I/O counters for live metrics (design M15a).
    async fn counters(&self) -> Result<serde_json::Value>;
}

#[async_trait]
impl ManagedVmm for CloudHypervisor {
    async fn create(&self, spec: &VirtualMachineSpec) -> Result<()> {
        Hypervisor::create(self, spec).await
    }
    async fn boot(&self) -> Result<()> {
        Hypervisor::boot(self).await
    }
    async fn shutdown(&self) -> Result<()> {
        Hypervisor::shutdown(self).await
    }
    async fn info(&self) -> Result<HypervisorVmInfo> {
        Hypervisor::info(self).await
    }
    async fn send_migration(&self, destination_url: &str) -> Result<()> {
        CloudHypervisor::send_migration(self, destination_url).await
    }
    async fn resize(&self, desired_vcpus: Option<u32>, desired_ram: Option<u64>) -> Result<()> {
        CloudHypervisor::resize(self, desired_vcpus, desired_ram).await
    }
    async fn add_disk(&self, disk: &DiskSpec) -> Result<()> {
        CloudHypervisor::add_disk(self, disk).await
    }
    async fn add_net(&self, tap: &TapBinding) -> Result<()> {
        CloudHypervisor::add_net(self, tap).await
    }
    async fn terminate(&mut self) -> Result<()> {
        CloudHypervisor::terminate(self).await
    }
    fn pid(&self) -> Option<u32> {
        CloudHypervisor::pid(self)
    }
    async fn counters(&self) -> Result<serde_json::Value> {
        CloudHypervisor::counters(self).await
    }
}

#[async_trait]
impl ManagedVmm for FakeHypervisor {
    async fn create(&self, spec: &VirtualMachineSpec) -> Result<()> {
        Hypervisor::create(self, spec).await
    }
    async fn boot(&self) -> Result<()> {
        Hypervisor::boot(self).await
    }
    async fn shutdown(&self) -> Result<()> {
        Hypervisor::shutdown(self).await
    }
    async fn info(&self) -> Result<HypervisorVmInfo> {
        Hypervisor::info(self).await
    }
    async fn send_migration(&self, _destination_url: &str) -> Result<()> {
        Ok(())
    }
    async fn resize(&self, _desired_vcpus: Option<u32>, _desired_ram: Option<u64>) -> Result<()> {
        Ok(())
    }
    async fn add_disk(&self, _disk: &DiskSpec) -> Result<()> {
        Ok(())
    }
    async fn add_net(&self, _tap: &TapBinding) -> Result<()> {
        Ok(())
    }
    async fn terminate(&mut self) -> Result<()> {
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        None
    }
    async fn counters(&self) -> Result<serde_json::Value> {
        Ok(serde_json::json!({}))
    }
}

/// Creates and re-attaches [`ManagedVmm`] handles for VMs.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Launch a fresh VMM for `id` with its prepared host TAPs (does not
    /// create/boot the VM yet).
    async fn launch(
        &self,
        id: VmId,
        spec: &VirtualMachineSpec,
        taps: Vec<TapBinding>,
        layout: &RuntimeLayout,
    ) -> Result<Box<dyn ManagedVmm>>;

    /// Re-attach to an already-running VMM after an agent restart (section 11).
    async fn attach(
        &self,
        id: VmId,
        spec: &VirtualMachineSpec,
        layout: &RuntimeLayout,
    ) -> Result<Box<dyn ManagedVmm>>;
}

/// Production backend: launches real `cloud-hypervisor` processes.
pub struct CloudHypervisorBackend {
    binary: PathBuf,
    serial_file: bool,
    seccomp: String,
}

impl CloudHypervisorBackend {
    pub fn new(binary: impl Into<PathBuf>, serial_mode: &str, seccomp: &str) -> Self {
        Self {
            binary: binary.into(),
            serial_file: serial_mode == "file",
            seccomp: seccomp.to_string(),
        }
    }

    fn translate(
        &self,
        id: VmId,
        layout: &RuntimeLayout,
        taps: Vec<TapBinding>,
    ) -> TranslateOptions {
        // Serial to a Unix socket lets the console drive it interactively; the
        // serial hub connects and tees output to serial.log (section 25). File
        // mode avoids the socket-path conflict for co-located migration tests.
        let serial = if self.serial_file {
            SerialTarget::File(layout.serial_log(id).to_string_lossy().into_owned())
        } else {
            SerialTarget::Socket(layout.serial_socket(id).to_string_lossy().into_owned())
        };
        TranslateOptions { serial, taps }
    }
}

#[async_trait]
impl Backend for CloudHypervisorBackend {
    async fn launch(
        &self,
        id: VmId,
        _spec: &VirtualMachineSpec,
        taps: Vec<TapBinding>,
        layout: &RuntimeLayout,
    ) -> Result<Box<dyn ManagedVmm>> {
        layout
            .ensure_vm_dir(id)
            .await
            .map_err(vquasar_client::ChError::Io)?;
        let launch = LaunchConfig::new(
            ProcessConfig {
                binary: self.binary.clone(),
                api_socket: layout.api_socket(id),
                log_file: Some(layout.vmm_log(id)),
                extra_args: vec!["--seccomp".to_string(), self.seccomp.clone()],
            },
            self.translate(id, layout, taps),
        );
        let hv = CloudHypervisor::launch(launch).await?;
        Ok(Box::new(hv))
    }

    async fn attach(
        &self,
        id: VmId,
        _spec: &VirtualMachineSpec,
        layout: &RuntimeLayout,
    ) -> Result<Box<dyn ManagedVmm>> {
        let hv = CloudHypervisor::attach(layout.api_socket(id), self.translate(id, layout, vec![]));
        Ok(Box::new(hv))
    }
}

/// Test backend: in-memory fakes, no processes. Handles for a given VM id are
/// shared so that `attach` recovers the same state `launch` created (letting
/// tests exercise the restart-recovery path).
#[cfg(test)]
#[derive(Default)]
pub struct FakeBackend {
    states: Mutex<HashMap<VmId, FakeHypervisor>>,
}

#[cfg(test)]
impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    /// Inspect the fake for a VM (test assertions).
    pub fn get(&self, id: VmId) -> Option<FakeHypervisor> {
        self.states.lock().unwrap().get(&id).cloned()
    }
}

#[cfg(test)]
#[async_trait]
impl Backend for FakeBackend {
    async fn launch(
        &self,
        id: VmId,
        _spec: &VirtualMachineSpec,
        _taps: Vec<TapBinding>,
        _layout: &RuntimeLayout,
    ) -> Result<Box<dyn ManagedVmm>> {
        let hv = self.states.lock().unwrap().entry(id).or_default().clone();
        Ok(Box::new(hv))
    }

    async fn attach(
        &self,
        id: VmId,
        _spec: &VirtualMachineSpec,
        _layout: &RuntimeLayout,
    ) -> Result<Box<dyn ManagedVmm>> {
        let hv = self.states.lock().unwrap().entry(id).or_default().clone();
        Ok(Box::new(hv))
    }
}
