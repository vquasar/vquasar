//! The hypervisor backend the [`VmManager`](crate::manager::VmManager) drives.
//!
//! [`ManagedVmm`] extends the per-VM operations of [`ch_client::Hypervisor`]
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

use ch_client::config::TranslateOptions;
use ch_client::{
    CloudHypervisor, FakeHypervisor, Hypervisor, HypervisorVmInfo, LaunchConfig, ProcessConfig,
    SerialTarget, TapBinding,
};
use ch_model::{VirtualMachineSpec, VmId};

use crate::runtime::RuntimeLayout;

type Result<T> = ch_client::Result<T>;

/// A per-VM hypervisor handle the manager can fully control, including tearing
/// down the underlying process.
#[async_trait]
pub trait ManagedVmm: Send + Sync {
    async fn create(&self, spec: &VirtualMachineSpec) -> Result<()>;
    async fn boot(&self) -> Result<()>;
    async fn shutdown(&self) -> Result<()>;
    async fn info(&self) -> Result<HypervisorVmInfo>;
    /// Terminate the underlying VMM process (no-op when detached or fake).
    async fn terminate(&mut self) -> Result<()>;
    fn pid(&self) -> Option<u32>;
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
    async fn terminate(&mut self) -> Result<()> {
        CloudHypervisor::terminate(self).await
    }
    fn pid(&self) -> Option<u32> {
        CloudHypervisor::pid(self)
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
    async fn terminate(&mut self) -> Result<()> {
        Ok(())
    }
    fn pid(&self) -> Option<u32> {
        None
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
}

impl CloudHypervisorBackend {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    fn translate(
        &self,
        id: VmId,
        layout: &RuntimeLayout,
        taps: Vec<TapBinding>,
    ) -> TranslateOptions {
        // Serial goes to a Unix socket so it can be driven interactively; the
        // serial hub connects to it and tees output to serial.log (section 25).
        TranslateOptions {
            serial: SerialTarget::Socket(layout.serial_socket(id).to_string_lossy().into_owned()),
            taps,
        }
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
            .map_err(ch_client::ChError::Io)?;
        let launch = LaunchConfig::new(
            ProcessConfig {
                binary: self.binary.clone(),
                api_socket: layout.api_socket(id),
                log_file: Some(layout.vmm_log(id)),
                extra_args: vec![],
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
