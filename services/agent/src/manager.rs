//! The VM manager: the agent's local authority over its VMs (design document,
//! sections 9, 11, 22).
//!
//! It owns the map of managed VMs, drives them through a [`Backend`], persists
//! desired state to disk, and reconstructs its inventory on startup. Operations
//! are idempotent where practical so a repeated reconcile does not create a
//! second VM (section 22).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ch_client::{ApiClient, ChError, HypervisorState, TapBinding};
use ch_model::{DesiredPowerState, VirtualMachineSpec, VmId, VmPhase};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::backend::{Backend, ManagedVmm};
use crate::console::SerialHub;
use crate::ipdiscovery::IpDiscovery;
use crate::network::{NetworkBackend, NicBinding};
use crate::runtime::{RuntimeLayout, VmRecord};
use crate::storage::StorageProvisioner;

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

    #[error("storage error: {0}")]
    Storage(#[from] crate::storage::StorageError),

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
    /// Guest IP learned agentlessly via neighbor snooping (design M11).
    pub ip: Option<String>,
}

struct ManagedVm {
    record: VmRecord,
    vmm: Box<dyn ManagedVmm>,
    console: SerialHub,
}

/// A VMM launched to receive a migration, awaiting completion.
struct PendingReceive {
    record: VmRecord,
    vmm: Box<dyn ManagedVmm>,
    recv: JoinHandle<ch_client::Result<()>>,
}

/// How the agent exposes an incoming live migration (section 28).
#[derive(Debug, Clone)]
pub struct MigrationSettings {
    /// `tcp` (cross-host) or `unix` (single-host lab).
    pub transport: String,
    /// Address peers use to reach this host for TCP migration.
    pub advertise_host: String,
    pub port_min: u16,
    pub port_max: u16,
    pub socket_dir: PathBuf,
}

/// The agent's VM manager.
pub struct VmManager {
    backend: Arc<dyn Backend>,
    network: Arc<dyn NetworkBackend>,
    storage: StorageProvisioner,
    ipdiscovery: IpDiscovery,
    layout: RuntimeLayout,
    migration: MigrationSettings,
    vms: Mutex<HashMap<VmId, ManagedVm>>,
    pending: Mutex<HashMap<VmId, PendingReceive>>,
}

impl VmManager {
    pub fn new(
        backend: Arc<dyn Backend>,
        network: Arc<dyn NetworkBackend>,
        storage: StorageProvisioner,
        ipdiscovery: IpDiscovery,
        layout: RuntimeLayout,
        migration: MigrationSettings,
    ) -> Self {
        Self {
            backend,
            network,
            storage,
            ipdiscovery,
            layout,
            migration,
            vms: Mutex::new(HashMap::new()),
            pending: Mutex::new(HashMap::new()),
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
        network_config: Option<String>,
    ) -> Result<ObservedVm> {
        spec.validate()
            .map_err(|e| ManagerError::InvalidSpec(e.to_string()))?;

        // Materialise host storage (clone volumes, generate the cloud-init seed)
        // and fold the seed disk into the spec before we launch (design M9).
        // Idempotent, so a repeated reconcile reuses existing files.
        let mut spec = self
            .storage
            .prepare(id, &name, spec, network_config.as_deref())
            .await?;
        // Persist the control-allocated NIC MACs in the record so agentless IP
        // discovery can match neighbor-table entries to this VM (design M11).
        for (nic, binding) in spec.network_interfaces.iter_mut().zip(bindings.iter()) {
            if nic.mac.is_none() {
                nic.mac = Some(binding.mac.clone());
            }
        }

        let record = VmRecord {
            id,
            name: name.clone(),
            spec: spec.clone(),
        };
        // Persist desired state first, then act (section 7).
        self.layout.store_record(&record).await?;

        let mut vms = self.vms.lock().await;

        // Power off (design M10): a stopped VM has its VMM *terminated*, not just
        // guest-shut-down — otherwise the VMM keeps an exclusive lock on the disk
        // file and offline changes (notably a disk grow) can never apply. The
        // record stays on disk so the VM can be started again later.
        if spec.desired_power_state == DesiredPowerState::Stopped {
            if let Some(mut managed) = vms.remove(&id) {
                let _ = managed.vmm.shutdown().await; // best-effort clean guest stop
                managed.vmm.terminate().await?; // kill the VMM -> release the disk
            }
            drop(vms);
            // Give a re-attached (unowned) VMM a moment to exit and release the
            // file lock, then apply any pending offline change (e.g. disk grow).
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            self.storage
                .prepare(id, &name, spec, network_config.as_deref())
                .await?;
            return Ok(ObservedVm {
                id,
                name,
                phase: VmPhase::Stopped,
                pid: None,
                message: None,
                ip: None,
            });
        }

        // desired == Running. Not the `entry` API: preparing TAPs and launching a
        // VMM are async and fallible, and must happen between the presence check
        // and the insert.
        let is_new = !vms.contains_key(&id);
        if is_new {
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
            let console =
                SerialHub::start(self.layout.serial_socket(id), self.layout.serial_log(id));
            vms.insert(
                id,
                ManagedVm {
                    record: record.clone(),
                    vmm,
                    console,
                },
            );
        } else {
            // Already running: re-apply the per-NIC security-group firewall so
            // rule changes take effect without recreating the TAP (design M13c).
            for (index, binding) in bindings.iter().enumerate() {
                self.network.refresh_firewall(id, index, binding).await?;
            }
        }
        let managed = vms.get(&id).expect("just inserted");

        managed.vmm.create(&spec).await?;
        // Apply live edits to an already-running VM (design M10): CPU/memory
        // hot-plug and hot-add of disks/NICs. Anything CH cannot apply live is
        // still persisted in the record below and takes effect on next restart.
        if !is_new {
            self.reconfigure(id, managed, &spec, &bindings).await?;
        }
        managed.vmm.boot().await?;
        let obs = observe(managed, &self.ipdiscovery).await;
        // Record the now-applied spec (and name) so the next reconcile diffs
        // against it rather than re-applying the same edits.
        if let Some(m) = vms.get_mut(&id) {
            m.record = record;
        }
        Ok(obs)
    }

    /// Apply the difference between a running VM's last-applied spec and
    /// `new_spec`, hot-plugging what Cloud Hypervisor supports (design M10).
    /// Non-live changes are left for the next restart (the caller persists the
    /// new spec regardless).
    async fn reconfigure(
        &self,
        id: VmId,
        managed: &ManagedVm,
        new_spec: &VirtualMachineSpec,
        bindings: &[NicBinding],
    ) -> Result<()> {
        let old = &managed.record.spec;

        // vCPUs: hot-resize within the boot-time maximum.
        if new_spec.cpu.boot_vcpus != old.cpu.boot_vcpus {
            if new_spec.cpu.boot_vcpus <= old.cpu.max_vcpus {
                managed
                    .vmm
                    .resize(Some(new_spec.cpu.boot_vcpus), None)
                    .await?;
                info!(vm = %id, vcpus = new_spec.cpu.boot_vcpus, "hot-resized vCPUs");
            } else {
                warn!(vm = %id, "vCPU target exceeds max_vcpus; takes effect after restart");
            }
        }

        // Memory: hot-resize only within the region reserved at boot.
        if new_spec.memory.size_mib != old.memory.size_mib {
            let within_cap = old.memory.hotplug_bytes().is_some()
                && new_spec.memory.size_mib <= old.memory.max_size_mib.unwrap_or(0);
            if within_cap {
                managed
                    .vmm
                    .resize(None, Some(new_spec.memory.size_bytes()))
                    .await?;
                info!(vm = %id, mib = new_spec.memory.size_mib, "hot-resized memory");
            } else {
                warn!(vm = %id, "memory change not hot-pluggable; takes effect after restart");
            }
        }

        // Disks: hot-add any new disk (matched by path). Its backing file was
        // already materialised by storage.prepare().
        for disk in &new_spec.disks {
            if !old.disks.iter().any(|o| o.path == disk.path) {
                managed.vmm.add_disk(disk).await?;
                info!(vm = %id, disk = %disk.path.display(), "hot-added disk");
            }
        }

        // NICs: hot-add any interface beyond the current count.
        for index in old.network_interfaces.len()..new_spec.network_interfaces.len() {
            if let Some(binding) = bindings.get(index) {
                let nic = self.network.prepare(id, index, binding).await?;
                let tap = TapBinding {
                    tap: nic.tap,
                    mac: Some(nic.mac),
                };
                managed.vmm.add_net(&tap).await?;
                info!(vm = %id, index, "hot-added NIC");
            }
        }
        Ok(())
    }

    /// Boot a known VM.
    pub async fn start(&self, id: VmId) -> Result<ObservedVm> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id).ok_or(ManagerError::NotFound(id))?;
        managed.vmm.boot().await?;
        Ok(observe(managed, &self.ipdiscovery).await)
    }

    /// Request an orderly shutdown of a known VM.
    pub async fn stop(&self, id: VmId) -> Result<ObservedVm> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id).ok_or(ManagerError::NotFound(id))?;
        managed.vmm.shutdown().await?;
        Ok(observe(managed, &self.ipdiscovery).await)
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
        Ok(observe(managed, &self.ipdiscovery).await)
    }

    /// Get a serial-console output subscription and input sender for a VM.
    pub async fn console(
        &self,
        id: VmId,
    ) -> Option<(broadcast::Receiver<Vec<u8>>, mpsc::Sender<Vec<u8>>)> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id)?;
        Some((managed.console.subscribe(), managed.console.input_sender()))
    }

    // ---- live migration (section 28) ------------------------------------

    /// Destination: launch an empty VMM and start receiving a migration for
    /// `id`. Returns the migration URL the source should send to.
    pub async fn prepare_receive(
        &self,
        id: VmId,
        name: String,
        spec: VirtualMachineSpec,
    ) -> Result<String> {
        let record = VmRecord {
            id,
            name,
            spec: spec.clone(),
        };
        self.layout.store_record(&record).await?;
        let vmm = self.backend.launch(id, &spec, vec![], &self.layout).await?;

        // Build the receiver URL (what CH binds) and the URL returned to the
        // source (what it connects to). For TCP they differ (bind 0.0.0.0,
        // advertise a reachable host); for unix they are the same path.
        let (recv_url, return_url) = if self.migration.transport == "unix" {
            tokio::fs::create_dir_all(&self.migration.socket_dir)
                .await
                .map_err(ManagerError::Io)?;
            let socket = self.migration.socket_dir.join(format!("{id}.sock"));
            let _ = tokio::fs::remove_file(&socket).await;
            let u = format!("unix:{}", socket.display());
            (u.clone(), u)
        } else {
            let port = pick_free_port(self.migration.port_min, self.migration.port_max)
                .ok_or_else(|| {
                    ManagerError::Hypervisor(ch_client::ChError::Transport(
                        "no free migration port in configured range".into(),
                    ))
                })?;
            let host = if self.migration.advertise_host.is_empty() {
                hostname()
            } else {
                self.migration.advertise_host.clone()
            };
            (format!("tcp:0.0.0.0:{port}"), format!("tcp:{host}:{port}"))
        };

        // The receive call blocks until the source connects and finishes, so
        // run it in the background and await it in `finalize_receive`.
        let api_socket = self.layout.api_socket(id);
        let recv = tokio::spawn(async move {
            let client = ApiClient::new(api_socket);
            client.receive_migration(&recv_url).await
        });

        self.pending
            .lock()
            .await
            .insert(id, PendingReceive { record, vmm, recv });
        Ok(return_url)
    }

    /// Destination: complete a received migration and register the now-running
    /// VM as a normal managed VM.
    pub async fn finalize_receive(&self, id: VmId) -> Result<ObservedVm> {
        let pending = self
            .pending
            .lock()
            .await
            .remove(&id)
            .ok_or(ManagerError::NotFound(id))?;
        // Wait for the background receive to finish (the source has sent).
        match pending.recv.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(ManagerError::Hypervisor(e)),
            Err(join) => {
                return Err(ManagerError::Hypervisor(ChError::Transport(
                    join.to_string(),
                )))
            }
        }
        let console = SerialHub::start(self.layout.serial_socket(id), self.layout.serial_log(id));
        let mut vms = self.vms.lock().await;
        vms.insert(
            id,
            ManagedVm {
                record: pending.record,
                vmm: pending.vmm,
                console,
            },
        );
        Ok(observe(vms.get(&id).expect("just inserted"), &self.ipdiscovery).await)
    }

    /// Source: send a running VM's live state to `destination_url`.
    pub async fn send_migration(&self, id: VmId, destination_url: &str) -> Result<()> {
        let vms = self.vms.lock().await;
        let managed = vms.get(&id).ok_or(ManagerError::NotFound(id))?;
        managed.vmm.send_migration(destination_url).await?;
        Ok(())
    }

    /// Source: discard a VM whose state has migrated away (tear down the VMM
    /// and release host resources, same as delete).
    pub async fn discard(&self, id: VmId) -> Result<()> {
        self.delete(id).await
    }

    /// Observed state of all managed VMs.
    pub async fn list(&self) -> Vec<ObservedVm> {
        let vms = self.vms.lock().await;
        let mut out = Vec::with_capacity(vms.len());
        for managed in vms.values() {
            out.push(observe(managed, &self.ipdiscovery).await);
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
                    // Only recover a VM whose VMM is actually alive. A leftover
                    // runtime dir from a crashed or deleted VM has a dead API
                    // socket; re-attaching would resurrect a phantom (inflated
                    // vm_count, no process). Probe it and discard the stale dir
                    // if there is nothing running — a VM the control plane still
                    // wants will be re-launched on the next reconcile (§11, §22).
                    if let Err(e) = vmm.info().await {
                        warn!(vm = %id, name = %record.name, error = %e,
                              "discarding stale VM (no live VMM)");
                        if let Err(e) = self.layout.remove_vm_dir(id).await {
                            warn!(vm = %id, error = %e, "failed to remove stale VM dir");
                        }
                        continue;
                    }
                    info!(vm = %id, name = %record.name, "recovered VM after restart");
                    let console =
                        SerialHub::start(self.layout.serial_socket(id), self.layout.serial_log(id));
                    vms.insert(
                        id,
                        ManagedVm {
                            record,
                            vmm,
                            console,
                        },
                    );
                }
                Err(e) => warn!(vm = %id, error = %e, "failed to re-attach to VM"),
            }
        }
    }
}

/// Derive observed state from a managed VM's live hypervisor info.
async fn observe(managed: &ManagedVm, ipd: &IpDiscovery) -> ObservedVm {
    let (phase, message) = match managed.vmm.info().await {
        Ok(info) => (phase_of(info.state), None),
        Err(e) => (VmPhase::Failed, Some(e.to_string())),
    };
    // Agentless IP discovery: the first NIC MAC that currently resolves to an
    // address in the host neighbor table (design M11). Older records may not
    // have persisted the MAC, so re-derive it deterministically when absent.
    let mut ip = None;
    for (index, nic) in managed.record.spec.network_interfaces.iter().enumerate() {
        let mac = nic
            .mac
            .clone()
            .unwrap_or_else(|| ch_model::allocate_mac(managed.record.id, index));
        if let Some(addr) = ipd.ip_for_mac(&mac).await {
            ip = Some(addr);
            break;
        }
    }
    ObservedVm {
        id: managed.record.id,
        name: managed.record.name.clone(),
        phase,
        pid: managed.vmm.pid(),
        message,
        ip,
    }
}

/// Find a free TCP port in `[min, max]` by binding it briefly, so incoming
/// migrations use a firewall-opened, reachable port.
fn pick_free_port(min: u16, max: u16) -> Option<u16> {
    (min..=max).find(|&p| std::net::TcpListener::bind(("0.0.0.0", p)).is_ok())
}

/// This host's name (used as the default migration advertise address).
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "localhost".to_string())
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
            memory: MemorySpec {
                size_mib: 512,
                max_size_mib: None,
            },
            boot: BootSpec::DirectKernel {
                kernel: "/boot/vmlinux".into(),
                initramfs: None,
                cmdline: None,
            },
            disks: vec![],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
            cloud_init: None,
        }
    }

    fn test_migration(dir: &std::path::Path) -> MigrationSettings {
        MigrationSettings {
            transport: "unix".to_string(),
            advertise_host: String::new(),
            port_min: 9600,
            port_max: 9700,
            socket_dir: dir.join("migrations"),
        }
    }

    fn manager(dir: &std::path::Path) -> (VmManager, Arc<FakeBackend>) {
        let backend = Arc::new(FakeBackend::new());
        let network = Arc::new(crate::network::NoopNetworkBackend);
        let layout = RuntimeLayout::new(dir);
        (
            VmManager::new(
                backend.clone(),
                network,
                StorageProvisioner::new(dir.join("shared")),
                IpDiscovery::new("br-int"),
                layout,
                test_migration(dir),
            ),
            backend,
        )
    }

    #[tokio::test]
    async fn ensure_running_boots_the_vm() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        let obs = mgr
            .ensure(id, "web-1".into(), spec(DesiredPowerState::Running), vec![], None)
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
        mgr.ensure(id, "web-1".into(), s.clone(), vec![], None)
            .await
            .unwrap();
        mgr.ensure(id, "web-1".into(), s, vec![], None)
            .await
            .unwrap();
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
        mgr.ensure(id, "db".into(), spec(DesiredPowerState::Running), vec![], None)
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
        mgr.ensure(id, "tmp".into(), spec(DesiredPowerState::Running), vec![], None)
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
                None,
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
        let mgr = VmManager::new(
            backend,
            network,
            StorageProvisioner::new(dir.path().join("shared")),
            IpDiscovery::new("br-int"),
            RuntimeLayout::new(dir.path()),
            test_migration(dir.path()),
        );
        mgr.recover().await;
        let obs = mgr.get(id).await.unwrap();
        assert_eq!(obs.name, "survivor");
        assert_eq!(obs.phase, VmPhase::Running, "recovered VM stays running");
    }
}
