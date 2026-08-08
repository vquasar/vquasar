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

use thiserror::Error;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::task::JoinHandle;
use tracing::{info, warn};
use vquasar_client::{ApiClient, ChError, HypervisorState, TapBinding};
use vquasar_model::{DesiredPowerState, VirtualMachineSpec, VmId, VmPhase};

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
    Hypervisor(#[from] vquasar_client::ChError),

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
    recv: JoinHandle<vquasar_client::Result<()>>,
    /// The URL handed back to the controller. Kept so a repeated
    /// `prepare_receive` can return the *same* receiver instead of starting a
    /// second one (#45).
    return_url: String,
}

/// Whether tearing a VM down on this host means its shared-storage state is
/// finished with.
///
/// The two callers look almost identical and mean opposite things. `DeleteVm`
/// is "this VM no longer exists", so its seed on shared storage is garbage.
/// `DiscardVm` is "this VM lives on another host now" — the husk here is
/// finished, but the shared files are in use by the guest that moved (#41).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shared {
    /// The VM is gone; reclaim what it left on shared storage.
    Reclaim,
    /// The VM moved; leave shared storage alone.
    Keep,
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
    /// Whether this VM currently has a VMM under management (tests, #35).
    #[cfg(test)]
    pub async fn is_managed(&self, id: VmId) -> bool {
        self.vms.lock().await.contains_key(&id)
    }

    /// Whether a migration receiver is recorded for this VM (tests, #45).
    #[cfg(test)]
    pub async fn has_prepared_receiver(&self, id: VmId) -> bool {
        self.pending.lock().await.contains_key(&id)
    }

    /// Promote a prepared receiver as a successful finalise would, without a
    /// Cloud Hypervisor to receive from (tests, #45).
    #[cfg(test)]
    pub async fn adopt_pending_for_test(&self, id: VmId) {
        let entry = self
            .pending
            .lock()
            .await
            .remove(&id)
            .expect("no receiver prepared");
        let console = SerialHub::start(self.layout.serial_socket(id), self.layout.serial_log(id));
        self.vms.lock().await.insert(
            id,
            ManagedVm {
                record: entry.record,
                vmm: entry.vmm,
                console,
            },
        );
    }

    pub async fn ensure(
        &self,
        id: VmId,
        name: String,
        spec: VirtualMachineSpec,
        bindings: Vec<NicBinding>,
        network_config: Option<String>,
        phone_home_token: Option<String>,
    ) -> Result<ObservedVm> {
        spec.validate()
            .map_err(|e| ManagerError::InvalidSpec(e.to_string()))?;

        // Materialise host storage (clone volumes, generate the cloud-init seed)
        // and fold the seed disk into the spec before we launch (design M9).
        // Idempotent, so a repeated reconcile reuses existing files.
        let mut spec = self
            .storage
            .prepare(
                id,
                &name,
                spec,
                network_config.as_deref(),
                phone_home_token.as_deref(),
            )
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
                .prepare(
                    id,
                    &name,
                    spec,
                    network_config.as_deref(),
                    phone_home_token.as_deref(),
                )
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
            // Already running: reconcile each NIC's dataplane in place — move it
            // if its network changed (M13d) and re-apply the firewall (M13c),
            // without recreating the TAP.
            for (index, binding) in bindings.iter().enumerate() {
                self.network.rehome(id, index, binding).await?;
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
        if let Err(e) = managed.vmm.boot().await {
            // A VMM that will not boot is sometimes unrecoverable *in place*:
            // an attempt interrupted between create and boot can leave a TAP
            // and a disk lock that make every subsequent boot fail identically,
            // for ever (#35). Retrying the same VMM cannot clear that.
            //
            // Discarding it can, and is safe precisely when the VM has never
            // booted: `Created` means no guest instruction has executed, so
            // there is no guest state to lose. Anything Running or Paused is
            // left strictly alone — reclaiming those would be destroying a
            // working VM to fix a reporting problem.
            let never_ran = matches!(
                managed.vmm.info().await.map(|i| i.state),
                Ok(HypervisorState::Created)
            );
            if never_ran {
                warn!(vm = %id, error = %e,
                      "boot failed on a VM that never ran; discarding the VMM so the \
                       next reconcile starts from a clean host");
                if let Some(mut stale) = vms.remove(&id) {
                    let _ = stale.vmm.terminate().await;
                }
                for index in 0..bindings.len() {
                    if let Err(e) = self.network.release(id, index).await {
                        warn!(vm = %id, index, error = %e, "could not release a TAP while reclaiming");
                    }
                }
            }
            // Either way the caller hears about it: the control plane counts
            // the failure and gives up if it keeps happening (#35).
            return Err(e.into());
        }
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
        self.teardown(id, Shared::Reclaim).await
    }

    /// Tear a VM down on this host. `shared` says whether the VM is *gone* or
    /// merely *elsewhere* — see [`Shared`].
    async fn teardown(&self, id: VmId, shared: Shared) -> Result<()> {
        // A receiver prepared for this VM goes too. Otherwise a migration that
        // failed after `prepare_receive` would leave one behind, and — now that
        // `prepare_receive` is idempotent — every later migration of this VM to
        // this host would be handed that dead receiver's URL (#45).
        if self.pending.lock().await.remove(&id).is_some() {
            tracing::info!(vm = %id, "discarded a prepared migration receiver");
        }
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
        if shared == Shared::Reclaim {
            self.storage.release_seed(id).await;
        }
        Ok(())
    }

    /// Live resource metrics for one VM (design M15a). Returns a not-running
    /// sample if the VM isn't managed here or has no VMM process.
    pub async fn metrics(&self, id: VmId) -> crate::metrics::VmMetrics {
        // Fetch pid + CH counters under the lock (quick), then sample CPU/mem
        // outside it (the CPU window sleeps ~200ms).
        let (pid, counters) = {
            let vms = self.vms.lock().await;
            match vms.get(&id) {
                Some(m) => (
                    m.vmm.pid(),
                    m.vmm
                        .counters()
                        .await
                        .unwrap_or_else(|_| serde_json::json!({})),
                ),
                None => (None, serde_json::json!({})),
            }
        };
        match pid {
            Some(pid) => crate::metrics::sample(pid, &counters).await,
            None => crate::metrics::VmMetrics::default(),
        }
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

    // Both migration steps below run through `detach`. See its note: an RPC
    // handler is not a safe place to mutate migration state, because tonic
    // cancels one when its caller disconnects.

    /// Destination: launch an empty VMM and start receiving a migration for
    /// `id`. Returns the migration URL the source should send to.
    ///
    /// **At most once per VM.** A controller that dies between this returning
    /// and recording the URL will retry, and its successor is a legitimate
    /// leader carrying a higher lease epoch — so epoch fencing (ADR-022) admits
    /// it by design. Two receivers for one guest is corruption rather than a
    /// retryable error, so the at-most-once guarantee has to live here (#45).
    ///
    /// **Runs in a spawned task.** tonic drops a handler future when its client
    /// disconnects, so a control plane dying mid-call would otherwise cancel
    /// this between launching the VMM and recording it, leaving an untracked
    /// Cloud Hypervisor process that nothing will ever finalise or clean up.
    /// Spawning means the work completes and the retry finds it.
    pub async fn prepare_receive(
        self: &Arc<Self>,
        id: VmId,
        name: String,
        spec: VirtualMachineSpec,
    ) -> Result<String> {
        let this = self.clone();
        detach(async move { this.prepare_receive_inner(id, name, spec).await }).await
    }

    async fn prepare_receive_inner(
        &self,
        id: VmId,
        name: String,
        spec: VirtualMachineSpec,
    ) -> Result<String> {
        // Held across the whole setup, so two callers cannot both find nothing
        // and both launch a receiver.
        let mut pending = self.pending.lock().await;
        if let Some(existing) = pending.get(&id) {
            info!(
                vm = %id,
                url = %existing.return_url,
                "a receiver is already prepared for this VM; returning it rather than \
                 starting a second"
            );
            return Ok(existing.return_url.clone());
        }
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
                    ManagerError::Hypervisor(vquasar_client::ChError::Transport(
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

        pending.insert(
            id,
            PendingReceive {
                record,
                vmm,
                recv,
                return_url: return_url.clone(),
            },
        );
        Ok(return_url)
    }

    /// Destination: complete a received migration and register the now-running
    /// VM as a normal managed VM.
    ///
    /// Idempotent, and safe to interrupt. Both matter: this is the step past the
    /// point of no return, where the guest's state has already left its source,
    /// so a call that half-happens loses the VM outright (#45).
    pub async fn finalize_receive(self: &Arc<Self>, id: VmId) -> Result<ObservedVm> {
        let this = self.clone();
        detach(async move { this.finalize_receive_inner(id).await }).await
    }

    async fn finalize_receive_inner(&self, id: VmId) -> Result<ObservedVm> {
        // `pending` before `vms`, and held for the whole finalise. That
        // serialises finalises — acceptable, since the controller advances one
        // migration step per tick — and is what makes a retry correct: it
        // blocks until the first call is done, then sees the adopted VM instead
        // of an empty map.
        let mut pending = self.pending.lock().await;
        if let Some(managed) = self.vms.lock().await.get(&id) {
            // Already adopted, so a previous call succeeded — most likely one
            // whose caller went away before it could record the result.
            return Ok(observe(managed, &self.ipdiscovery).await);
        }
        let entry = pending.get_mut(&id).ok_or(ManagerError::NotFound(id))?;
        // Wait for the background receive to finish (the source has sent).
        // Borrowed rather than taken: the entry stays put until the receive has
        // actually succeeded, so nothing has been destroyed if it has not.
        let received = (&mut entry.recv).await;
        match received {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(ManagerError::Hypervisor(e)),
            Err(join) => {
                return Err(ManagerError::Hypervisor(ChError::Transport(
                    join.to_string(),
                )))
            }
        }
        let entry = pending.remove(&id).expect("checked above, lock still held");
        let console = SerialHub::start(self.layout.serial_socket(id), self.layout.serial_log(id));
        let mut vms = self.vms.lock().await;
        vms.insert(
            id,
            ManagedVm {
                record: entry.record,
                vmm: entry.vmm,
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
        // A receiver that never received is not a VM: dropping the entry tears
        // down its VMM, and there is nothing else to release. Reporting
        // `NotFound` here would make the controller's cleanup after a failed
        // migration look like a failure of its own (#45).
        let had_receiver = self.pending.lock().await.remove(&id).is_some();
        if had_receiver && !self.vms.lock().await.contains_key(&id) {
            info!(vm = %id, "discarded a prepared migration receiver");
            return Ok(());
        }
        // `Shared::Keep` is the whole difference between this and `delete`, and
        // it is not a detail: the VM is now running on another host off the
        // *same* shared storage. Reclaiming its cloud-init seed here would pull
        // a mounted disk out from under a live guest (#41).
        self.teardown(id, Shared::Keep).await
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
/// Run `work` somewhere a dropped caller cannot kill it, and report its result.
///
/// tonic drops a handler future when its client disconnects, which means a
/// control plane that dies mid-RPC cancels the agent's work at whatever await
/// it had reached. For anything that mutates state around an await that is not
/// a hazard in theory — it is what lost a guest in #45, where a cancelled
/// finalise had already emptied `pending` on its way out.
///
/// Spawning separates the two questions. The work runs to completion and leaves
/// the agent consistent whatever the caller does; the caller either learns the
/// outcome or does not, and a retry finds a state that makes sense either way.
async fn detach<T, F>(work: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    match tokio::spawn(work).await {
        Ok(result) => result,
        // The task panicked or the runtime is shutting down. Neither is
        // something the caller can retry into a better outcome, but it must not
        // be reported as success.
        Err(join) => Err(ManagerError::Hypervisor(ChError::Transport(
            join.to_string(),
        ))),
    }
}

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
            .unwrap_or_else(|| vquasar_model::allocate_mac(managed.record.id, index));
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
    use vquasar_model::{BootSpec, CpuSpec, MemorySpec, PlacementSpec};

    use crate::backend::FakeBackend;

    use super::*;

    pub(super) fn spec(power: DesiredPowerState) -> VirtualMachineSpec {
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
            machine_type: vquasar_model::MachineType::Standard,
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

    /// The migration paths take `self: &Arc<Self>` — they spawn, so they need
    /// an owned handle. Everything else keeps the plain constructor.
    pub(super) fn arc_manager(dir: &std::path::Path) -> (Arc<VmManager>, Arc<FakeBackend>) {
        let (m, b) = manager(dir);
        (Arc::new(m), b)
    }

    pub(super) fn manager(dir: &std::path::Path) -> (VmManager, Arc<FakeBackend>) {
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
            .ensure(
                id,
                "web-1".into(),
                spec(DesiredPowerState::Running),
                vec![],
                None,
                None,
            )
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
        mgr.ensure(id, "web-1".into(), s.clone(), vec![], None, None)
            .await
            .unwrap();
        mgr.ensure(id, "web-1".into(), s, vec![], None, None)
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
        mgr.ensure(
            id,
            "db".into(),
            spec(DesiredPowerState::Running),
            vec![],
            None,
            None,
        )
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
        mgr.ensure(
            id,
            "tmp".into(),
            spec(DesiredPowerState::Running),
            vec![],
            None,
            None,
        )
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

    /// An interrupted create can leave a host in a state where every boot fails
    /// the same way for ever (#35). Retrying the same VMM cannot clear that, so
    /// a VM that has never booted is discarded and the next reconcile starts
    /// from a clean host.
    #[tokio::test]
    async fn an_unbootable_vm_that_never_ran_is_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, backend) = manager(dir.path());
        let id = VmId::new();

        // First ensure gets as far as `Created` and then cannot boot — and will
        // not be able to on any later attempt either.
        backend.fail_boot_always_for(id);
        let err = mgr
            .ensure(
                id,
                "wedged".into(),
                spec(DesiredPowerState::Running),
                vec![],
                None,
                None,
            )
            .await;
        assert!(err.is_err(), "a boot that cannot succeed must be reported");

        // The VMM is gone rather than kept to fail again identically.
        assert!(
            !mgr.is_managed(id).await,
            "an unbootable, never-run VMM must be discarded so the next tick is clean"
        );

        // And the next reconcile — on a host where boot now works — succeeds
        // without any operator intervention.
        backend.clear_failures();
        let obs = mgr
            .ensure(
                id,
                "wedged".into(),
                spec(DesiredPowerState::Running),
                vec![],
                None,
                None,
            )
            .await
            .expect("a clean retry should succeed");
        assert_eq!(obs.phase, VmPhase::Running);
    }

    /// The reclaim must never touch a VM that is actually running: discarding
    /// one to fix a boot error would destroy a working guest.
    #[tokio::test]
    async fn a_running_vm_is_never_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, backend) = manager(dir.path());
        let id = VmId::new();
        mgr.ensure(
            id,
            "live".into(),
            spec(DesiredPowerState::Running),
            vec![],
            None,
            None,
        )
        .await
        .unwrap();
        assert!(mgr.is_managed(id).await);

        // Now make boot fail. The VM is Running, so this is not the interrupted
        // create case and the VMM must survive.
        backend.fail_boot_always_for(id);
        let _ = mgr
            .ensure(
                id,
                "live".into(),
                spec(DesiredPowerState::Running),
                vec![],
                None,
                None,
            )
            .await;
        assert!(
            mgr.is_managed(id).await,
            "a running VM must not be discarded because a boot call failed"
        );
    }
}

/// At-most-once migration receive (#45).
///
/// A control plane that dies mid-step retries from its successor, and epoch
/// fencing admits that retry by design — the successor holds a *higher* lease
/// epoch. So the guarantee that one guest gets one receiver has to live here.
#[cfg(test)]
mod migration_at_most_once {
    use super::tests::*;
    use super::*;

    fn record(id: VmId) -> (String, VirtualMachineSpec) {
        (format!("vm-{id}"), spec(DesiredPowerState::Running))
    }

    /// The failure from #45: a leader dies between `prepare_receive` returning
    /// and the write that records the URL, so its successor calls again.
    #[tokio::test]
    async fn preparing_twice_reuses_the_first_receiver() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, backend) = arc_manager(dir.path());
        let id = VmId::new();
        let (name, spec) = record(id);

        let first = mgr
            .prepare_receive(id, name.clone(), spec.clone())
            .await
            .unwrap();
        let second = mgr.prepare_receive(id, name, spec).await.unwrap();

        assert_eq!(
            first, second,
            "a retry must be sent to the receiver that already exists"
        );
        assert_eq!(
            backend.launch_count(id),
            1,
            "two receivers for one guest is the corruption this exists to prevent"
        );
    }

    /// tonic drops a handler future when its client disconnects. The work must
    /// still land, or the retry finds a half-built state — which is how #45
    /// lost a guest.
    ///
    /// The runtime is single-threaded here, so the spawned task provably cannot
    /// have run before the future is dropped: this is a real interruption, not
    /// a race that usually goes the right way.
    #[tokio::test]
    async fn a_prepare_whose_caller_vanishes_still_records_its_receiver() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, backend) = arc_manager(dir.path());
        let id = VmId::new();
        let (name, spec) = record(id);

        // Poll once, then drop. One poll is enough to start the work and — on
        // this single-threaded runtime — provably not enough to finish it, so
        // the caller goes away with the receiver half-built. That determinism
        // is the point: an earlier draft raced the detached task and passed
        // under `cargo test -p` while failing under load.
        let mut call = Box::pin(mgr.prepare_receive(id, name.clone(), spec.clone()));
        tokio::select! {
            biased;
            _ = &mut call => panic!("the call finished in one poll; nothing was interrupted"),
            _ = std::future::ready(()) => {}
        }
        drop(call);

        // The work has to land anyway. Otherwise it is a Cloud Hypervisor
        // process the agent has no handle on: invisible to finalise, invisible
        // to discard, never reclaimed — and the retry below starts another one
        // beside it.
        for _ in 0..1000 {
            if mgr.has_prepared_receiver(id).await {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            mgr.has_prepared_receiver(id).await,
            "the interrupted call was abandoned mid-way; its receiver is unaccounted for"
        );

        let url = mgr.prepare_receive(id, name, spec).await.unwrap();
        assert!(!url.is_empty());
        assert_eq!(
            backend.launch_count(id),
            1,
            "the retry started a second receiver beside the orphaned one"
        );
    }

    /// Finalise is past the point of no return — the guest's state has left its
    /// source — so a repeat has to succeed rather than report `NotFound` and
    /// send the controller down its failure path.
    #[tokio::test]
    async fn finalising_an_already_adopted_vm_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _backend) = arc_manager(dir.path());
        let id = VmId::new();
        let (name, spec) = record(id);

        mgr.prepare_receive(id, name, spec).await.unwrap();
        // Stand in for a completed receive: the fake has no Cloud Hypervisor to
        // connect to, so adopt the VM the way a successful finalise would.
        mgr.adopt_pending_for_test(id).await;

        let observed = mgr
            .finalize_receive(id)
            .await
            .expect("a second finalise must report the VM it already adopted");
        assert_eq!(observed.id, id);
        assert!(mgr.is_managed(id).await);
    }

    /// A migration that fails in `Pending` leaves a receiver behind. Because
    /// `prepare_receive` is now idempotent, one left lying around would be
    /// handed to the *next* migration of this VM to this host — a URL nothing
    /// is listening on. `DiscardVm` is how the controller clears it.
    #[tokio::test]
    async fn discarding_clears_a_prepared_receiver_and_frees_the_vm_for_a_retry() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, backend) = arc_manager(dir.path());
        let id = VmId::new();
        let (name, spec) = record(id);

        let first = mgr
            .prepare_receive(id, name.clone(), spec.clone())
            .await
            .unwrap();
        mgr.discard(id)
            .await
            .expect("discarding a receiver is not a failure");

        let second = mgr.prepare_receive(id, name, spec).await.unwrap();
        assert_eq!(
            backend.launch_count(id),
            2,
            "after a discard the next migration must get a fresh receiver"
        );
        // Nothing asserts the URLs differ — with `unix` transport the socket
        // path is derived from the VM id, so they legitimately match. What
        // matters is that a new receiver was launched behind it.
        let _ = (first, second);
    }
}

/// Reclaiming shared storage on VM deletion (#41).
#[cfg(test)]
mod seed_reclaim {
    use super::tests::*;
    use super::*;

    /// The seed lives on shared storage under a path the agent chose, so the
    /// agent is what has to clean it up.
    fn seed_of(dir: &std::path::Path, id: VmId) -> PathBuf {
        dir.join("shared").join("seeds").join(format!("{id}.iso"))
    }

    /// Stand in for a seed the agent generated. Writing the file directly keeps
    /// the test off `xorriso`, which is about generating an ISO rather than
    /// about who owns it afterwards.
    async fn plant_seed(dir: &std::path::Path, id: VmId) -> PathBuf {
        let path = seed_of(dir, id);
        tokio::fs::create_dir_all(path.parent().unwrap())
            .await
            .unwrap();
        tokio::fs::write(&path, b"fake seed").await.unwrap();
        path
    }

    #[tokio::test]
    async fn deleting_a_vm_reclaims_its_cloud_init_seed() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        mgr.ensure(
            id,
            "web-1".into(),
            spec(DesiredPowerState::Running),
            vec![],
            None,
            None,
        )
        .await
        .unwrap();
        let seed = plant_seed(dir.path(), id).await;

        mgr.delete(id).await.unwrap();
        assert!(
            !seed.exists(),
            "the VM is gone but its seed is still on shared storage"
        );
    }

    /// The case that makes this more than a `remove_file` call. `DiscardVm` runs
    /// on the *source* after a live migration, and the guest is now running on
    /// another host off the same shared storage — with this very file mounted.
    /// Reclaiming it here would pull a disk out from under a live VM.
    #[tokio::test]
    async fn discarding_a_migrated_vm_leaves_its_seed_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        mgr.ensure(
            id,
            "web-1".into(),
            spec(DesiredPowerState::Running),
            vec![],
            None,
            None,
        )
        .await
        .unwrap();
        let seed = plant_seed(dir.path(), id).await;

        mgr.discard(id).await.unwrap();
        assert!(
            seed.exists(),
            "discard reclaimed a seed the migrated guest is still mounting"
        );
        assert!(
            !mgr.is_managed(id).await,
            "the local husk is still torn down"
        );
    }

    /// A VM with no cloud-init never had a seed, and deleting one twice is
    /// normal on a retried reconcile. Neither is an error.
    #[tokio::test]
    async fn reclaiming_a_seed_that_is_not_there_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let (mgr, _) = manager(dir.path());
        let id = VmId::new();
        mgr.ensure(
            id,
            "no-cloud-init".into(),
            spec(DesiredPowerState::Running),
            vec![],
            None,
            None,
        )
        .await
        .unwrap();
        mgr.delete(id).await.expect("delete without a seed");
    }
}
