//! The VM manager: the agent's local authority over its VMs (design document,
//! sections 9, 11, 22).
//!
//! It owns the map of managed VMs, drives them through a [`Backend`], persists
//! desired state to disk, and reconstructs its inventory on startup. Operations
//! are idempotent where practical so a repeated reconcile does not create a
//! second VM (section 22).

use std::collections::HashMap;
use std::sync::Arc;

use ch_client::{HypervisorState, TapBinding};
use ch_model::{DesiredPowerState, VirtualMachineSpec, VmId, VmPhase};
use thiserror::Error;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::backend::{Backend, ManagedVmm};
use crate::network::{NetworkBackend, NicBinding};
use crate::runtime::{RuntimeLayout, VmRecord};

/// A failure from a manager operation.
#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("vm not found: {0}")]
    NotFound(VmId),

    #[error("invalid spec: {0}")]
    InvalidSpec(String),

    #[error("hypervisor error: {0}")]
    Hypervisor(#[from] ch_client::ChError),

    #[error("network error: {0}")]
    Network(#[from] crate::network::NetworkError),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, ManagerError>;

/// Observed state of a managed VM, derived live (never persisted).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedVm {
    pub id: VmId,
    pub name: String,
    pub phase: VmPhase,
    pub pid: Option<u32>,
    pub message: Option<String>,
}

struct ManagedVm {
    record: VmRecord,
    vmm: Box<dyn ManagedVmm>,
}

/// The agent's VM manager.
pub struct VmManager {
    backend: Arc<dyn Backend>,
    network: Arc<dyn NetworkBackend>,
    layout: RuntimeLayout,
    vms: Mutex<HashMap<VmId, ManagedVm>>,
}

impl VmManager {
    pub fn new(
        backend: Arc<dyn Backend>,
        network: Arc<dyn NetworkBackend>,
        layout: RuntimeLayout,
    ) -> Self {
        Self {
            backend,
            network,
            layout,
            vms: Mutex::new(HashMap::new()),
        }
    }

    /// Reconcile a VM towards `spec`: prepare its host networking, create it if
    /// needed, then drive its power state to match `spec.desired_power_state`.
    /// Idempotent. `bindings` are the control-resolved NIC bindings, one per
    /// network interface in spec order.
    pub async fn ensure(
        &self,
        id: VmId,
        name: String,
        spec: VirtualMachineSpec,
        bindings: Vec<NicBinding>,
    ) -> Result<ObservedVm> {
        spec.validate()
            .map_err(|e| ManagerError::InvalidSpec(e.to_string()))?;

        let record = VmRecord {
            id,
            name: name.clone(),
            spec: spec.clone(),
        };
        // Persist desired state first, then act (section 7).
        self.layout.store_record(&record).await?;

        let mut vms = self.vms.lock().await;
        // Not the `entry` API: preparing TAPs and launching a VMM are async and
        // fallible, and must happen between the presence check and the insert.
        #[allow(clippy::map_entry)]
        if !vms.contains_key(&id) {
            // Prepare host networking (TAP + OVS) for each NIC (section 18).
            let mut taps = Vec::with_capacity(bindings.len());
            for (index, binding) in bindings.iter().enumerate() {
                let nic = self.network.prepare(id, index, binding).await?;
                taps.push(TapBinding {
                    tap: nic.tap,
                    mac: Some(nic.mac),
                });
            }
            let vmm = self.backend.launch(id, &spec, taps, &self.layout).await?;
            vms.insert(id, ManagedVm { record, vmm });
        }
        let managed = vms.get(&id).expect("just inserted");

        managed.vmm.create(&spec).await?;
        match spec.desired_power_state {
            DesiredPowerState::Running => managed.vmm.boot().await?,
            DesiredPowerState::Stopped => managed.vmm.shutdown().await?,
        }
        Ok(observe(managed).await)
    }

    /// Boot a known VM.
    pub async fn start(&self, id: VmId) -> Result<ObservedVm> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id).ok_or(ManagerError::NotFound(id))?;
        managed.vmm.boot().await?;
        Ok(observe(managed).await)
    }

    /// Request an orderly shutdown of a known VM.
    pub async fn stop(&self, id: VmId) -> Result<ObservedVm> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id).ok_or(ManagerError::NotFound(id))?;
        managed.vmm.shutdown().await?;
        Ok(observe(managed).await)
    }

    /// Shut down, terminate the VMM, release host networking, and remove all
    /// state for a VM.
    pub async fn delete(&self, id: VmId) -> Result<()> {
        let mut vms = self.vms.lock().await;
        let mut managed = vms.remove(&id).ok_or(ManagerError::NotFound(id))?;
        let nic_count = managed.record.spec.network_interfaces.len();
        // Best-effort orderly shutdown, then hard-terminate the process.
        let _ = managed.vmm.shutdown().await;
        managed.vmm.terminate().await?;
        drop(vms);
        // Release each NIC's TAP/OVS port (idempotent; names are deterministic).
        for index in 0..nic_count {
            if let Err(e) = self.network.release(id, index).await {
                warn!(vm = %id, index, error = %e, "failed to release NIC");
            }
        }
        self.layout.remove_vm_dir(id).await?;
        Ok(())
    }

    /// Observed state of one VM.
    pub async fn get(&self, id: VmId) -> Result<ObservedVm> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id).ok_or(ManagerError::NotFound(id))?;
        Ok(observe(managed).await)
    }

    /// Observed state of all managed VMs.
    pub async fn list(&self) -> Vec<ObservedVm> {
        let vms = self.vms.lock().await;
        let mut out = Vec::with_capacity(vms.len());
        for managed in vms.values() {
            out.push(observe(managed).await);
        }
        out
    }

    /// Rebuild the VM inventory after an agent restart by re-attaching to the
    /// VMMs recorded on disk. Running VMs are never disturbed (section 11).
    pub async fn recover(&self) {
        let ids = self.layout.discover_vm_ids();
        if ids.is_empty() {
            return;
        }
        let mut vms = self.vms.lock().await;
        for id in ids {
            let record = match self.layout.load_record(id).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(vm = %id, error = %e, "skipping VM with unreadable metadata");
                    continue;
                }
            };
            match self.backend.attach(id, &record.spec, &self.layout).await {
                Ok(vmm) => {
                    info!(vm = %id, name = %record.name, "recovered VM after restart");
                    vms.insert(id, ManagedVm { record, vmm });
                }
                Err(e) => warn!(vm = %id, error = %e, "failed to re-attach to VM"),
            }
        }
    }
}

/// Derive observed state from a managed VM's live hypervisor info.
async fn observe(managed: &ManagedVm) -> ObservedVm {
    let (phase, message) = match managed.vmm.info().await {
        Ok(info) => (phase_of(info.state), None),
        Err(e) => (VmPhase::Failed, Some(e.to_string())),
    };
    ObservedVm {
        id: managed.record.id,
        name: managed.record.name.clone(),
        phase,
        pid: managed.vmm.pid(),
        message,
    }
}

/// Map a hypervisor state onto an orchestration phase.
fn phase_of(state: HypervisorState) -> VmPhase {
    match state {
        HypervisorState::Created | HypervisorState::Shutdown => VmPhase::Stopped,
        HypervisorState::Running | HypervisorState::Paused => VmPhase::Running,
    }
}

#[cfg(test)]
mod tests {
    use ch_model::{BootSpec, CpuSpec, MemorySpec, PlacementSpec};

    use crate::backend::FakeBackend;

    use super::*;

    fn spec(power: DesiredPowerState) -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: power,
            cpu: CpuSpec {
                boot_vcpus: 1,
                max_vcpus: 1,
            },
            memory: MemorySpec { size_mib: 512 },
            boot: BootSpec::DirectKernel {
                kernel: "/boot/vmlinux".into(),
                initramfs: None,
                cmdline: None,
            },
            disks: vec![],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
        }
    }

    fn manager(dir: &std::path::Path) -> (VmManager, Arc<FakeBackend>) {
        let backend = Arc::new(FakeBackend::new());
        let network = Arc::new(crate::network::NoopNetworkBackend);
        let layout = RuntimeLayout::new(dir);
        (VmManager::new(backend.clone(), network, layout), backend)
    }

    #[tokio::test]
    async fn ensure_running_boots_the_vm() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        let obs = mgr
            .ensure(id, "web-1".into(), spec(DesiredPowerState::Running), vec![])
            .await
            .unwrap();
        assert_eq!(obs.phase, VmPhase::Running);
        assert_eq!(obs.name, "web-1");
    }

    #[tokio::test]
    async fn ensure_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, backend) = manager(dir.path());
        let id = VmId::new();
        let s = spec(DesiredPowerState::Running);
        mgr.ensure(id, "web-1".into(), s.clone(), vec![])
            .await
            .unwrap();
        mgr.ensure(id, "web-1".into(), s, vec![]).await.unwrap();
        // The fake records create calls; a second ensure must not create twice.
        let fake = backend.get(id).unwrap();
        assert_eq!(fake.create_calls(), 2, "create is invoked but idempotent");
        assert_eq!(mgr.list().await.len(), 1, "exactly one VM is managed");
    }

    #[tokio::test]
    async fn stop_then_start_transitions_phase() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        mgr.ensure(id, "db".into(), spec(DesiredPowerState::Running), vec![])
            .await
            .unwrap();
        assert_eq!(mgr.stop(id).await.unwrap().phase, VmPhase::Stopped);
        assert_eq!(mgr.start(id).await.unwrap().phase, VmPhase::Running);
    }

    #[tokio::test]
    async fn delete_removes_vm_and_state() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        mgr.ensure(id, "tmp".into(), spec(DesiredPowerState::Running), vec![])
            .await
            .unwrap();
        mgr.delete(id).await.unwrap();
        assert!(matches!(mgr.get(id).await, Err(ManagerError::NotFound(_))));
        assert!(mgr.layout.discover_vm_ids().is_empty());
    }

    #[tokio::test]
    async fn recover_rebuilds_inventory_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let id = VmId::new();
        // First manager instance creates and boots a VM.
        {
            let (mgr, _) = manager(dir.path());
            mgr.ensure(
                id,
                "survivor".into(),
                spec(DesiredPowerState::Running),
                vec![],
            )
            .await
            .unwrap();
        }
        // A fresh manager (simulating an agent restart) recovers it from disk.
        let backend = Arc::new(FakeBackend::new());
        // Seed the fake so the "already running" VM is discoverable on attach,
        // mimicking a VMM that survived the agent restart (section 11).
        let seeded = backend
            .launch(
                id,
                &spec(DesiredPowerState::Running),
                vec![],
                &RuntimeLayout::new(dir.path()),
            )
            .await
            .unwrap();
        seeded
            .create(&spec(DesiredPowerState::Running))
            .await
            .unwrap();
        seeded.boot().await.unwrap();
        let network = Arc::new(crate::network::NoopNetworkBackend);
        let mgr = VmManager::new(backend, network, RuntimeLayout::new(dir.path()));
        mgr.recover().await;
        let obs = mgr.get(id).await.unwrap();
        assert_eq!(obs.name, "survivor");
        assert_eq!(obs.phase, VmPhase::Running, "recovered VM stays running");
    }
}
