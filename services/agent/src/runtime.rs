//! Per-VM runtime layout and persisted metadata (design document, sections 9
//! and 11).
//!
//! Each managed VM gets a directory under `<runtime_dir>/vms/<vm-uuid>/` holding
//! its API socket, serial log, and a `metadata.json` record of the desired
//! spec. Persisting the record (not just live process state) is what lets the
//! agent rebuild its inventory after a restart without killing running VMs.

use std::path::{Path, PathBuf};

use ch_model::{VirtualMachineSpec, VmId};
use serde::{Deserialize, Serialize};

/// Filesystem layout for VM runtime state, rooted at `<runtime_dir>/vms`.
#[derive(Debug, Clone)]
pub struct RuntimeLayout {
    root: PathBuf,
}

impl RuntimeLayout {
    /// Build a layout under `runtime_dir` (e.g. `/run/ch-orchestrator`).
    pub fn new(runtime_dir: impl AsRef<Path>) -> Self {
        Self {
            root: runtime_dir.as_ref().join("vms"),
        }
    }

    /// The per-VM directory.
    pub fn vm_dir(&self, id: VmId) -> PathBuf {
        self.root.join(id.to_string())
    }

    /// The Cloud Hypervisor API socket path for a VM.
    pub fn api_socket(&self, id: VmId) -> PathBuf {
        self.vm_dir(id).join("api.sock")
    }

    /// The serial-console log file for a VM.
    pub fn serial_log(&self, id: VmId) -> PathBuf {
        self.vm_dir(id).join("serial.log")
    }

    /// The serial-console Unix socket Cloud Hypervisor exposes for a VM.
    pub fn serial_socket(&self, id: VmId) -> PathBuf {
        self.vm_dir(id).join("serial.sock")
    }

    /// The VMM stdout/stderr log file for a VM.
    pub fn vmm_log(&self, id: VmId) -> PathBuf {
        self.vm_dir(id).join("ch.log")
    }

    /// The persisted metadata record path for a VM.
    pub fn metadata(&self, id: VmId) -> PathBuf {
        self.vm_dir(id).join("metadata.json")
    }

    /// Create the per-VM directory if absent.
    pub async fn ensure_vm_dir(&self, id: VmId) -> std::io::Result<()> {
        tokio::fs::create_dir_all(self.vm_dir(id)).await
    }

    /// Remove a VM's runtime directory and all its contents.
    ///
    /// The serial-console task creates `serial.log` shortly after boot; deleting
    /// a VM right after it starts can race that create against `remove_dir_all`,
    /// yielding `DirectoryNotEmpty` (the dir walk rmdir's after the new file
    /// appears). The console task creates at most that one file, so a couple of
    /// bounded retries reliably win the race.
    pub async fn remove_vm_dir(&self, id: VmId) -> std::io::Result<()> {
        let dir = self.vm_dir(id);
        for attempt in 0..5 {
            match tokio::fs::remove_dir_all(&dir).await {
                Ok(()) => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty && attempt < 4 => {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Enumerate VM ids that have a runtime directory (used on restart to
    /// rediscover managed VMs).
    pub fn discover_vm_ids(&self) -> Vec<VmId> {
        let mut ids = Vec::new();
        let Ok(entries) = std::fs::read_dir(&self.root) else {
            return ids;
        };
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(id) = name.parse::<VmId>() {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Load a persisted record for a VM.
    pub async fn load_record(&self, id: VmId) -> std::io::Result<VmRecord> {
        let bytes = tokio::fs::read(self.metadata(id)).await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Persist a record for a VM (creates the VM directory first).
    pub async fn store_record(&self, record: &VmRecord) -> std::io::Result<()> {
        self.ensure_vm_dir(record.id).await?;
        let bytes = serde_json::to_vec_pretty(record)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        tokio::fs::write(self.metadata(record.id), bytes).await
    }
}

/// The durable record for a managed VM: its identity and desired spec.
///
/// Observed state (the current phase) is *not* stored — it is derived live from
/// the hypervisor so it can never go stale (design sections 5 and 7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmRecord {
    pub id: VmId,
    pub name: String,
    pub spec: VirtualMachineSpec,
}

#[cfg(test)]
mod tests {
    use ch_model::{
        BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec, VirtualMachineSpec,
    };

    use super::*;

    fn spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
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
            machine_type: ch_model::MachineType::Standard,
        }
    }

    #[tokio::test]
    async fn record_roundtrips_and_is_discoverable() {
        let dir = tempfile::tempdir().unwrap();
        let layout = RuntimeLayout::new(dir.path());
        let id = VmId::new();
        let record = VmRecord {
            id,
            name: "db-01".into(),
            spec: spec(),
        };
        layout.store_record(&record).await.unwrap();

        assert_eq!(layout.discover_vm_ids(), vec![id]);
        assert_eq!(layout.load_record(id).await.unwrap(), record);

        layout.remove_vm_dir(id).await.unwrap();
        assert!(layout.discover_vm_ids().is_empty());
    }

    #[test]
    fn paths_are_derived_from_uuid() {
        let layout = RuntimeLayout::new("/run/ch-orchestrator");
        let id = VmId::new();
        assert!(layout.api_socket(id).ends_with(format!("{id}/api.sock")));
    }
}
