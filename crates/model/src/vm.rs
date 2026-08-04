//! The [`VirtualMachine`] resource and its spec/status types (design document,
//! sections 6 and 7).

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::{HostId, NetworkId, VmId};
use crate::meta::{Generation, Metadata};

/// A managed virtual machine: identity + desired state (`spec`) + observed
/// state (`status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualMachine {
    pub id: VmId,
    #[serde(flatten)]
    pub meta: Metadata,
    pub spec: VirtualMachineSpec,
    pub status: VirtualMachineStatus,
}

impl VirtualMachine {
    /// Create a new VM in the `Pending` phase with generation 1.
    pub fn new(name: impl Into<String>, spec: VirtualMachineSpec, now: DateTime<Utc>) -> Self {
        Self {
            id: VmId::new(),
            meta: Metadata::new(name, now),
            spec,
            status: VirtualMachineStatus::pending(),
        }
    }

    /// Whether the desired state has changed since the controller last acted,
    /// i.e. reconciliation is pending (section 7).
    pub fn needs_reconcile(&self) -> bool {
        self.meta.generation != self.status.observed_generation
    }
}

/// Desired state for a virtual machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualMachineSpec {
    pub desired_power_state: DesiredPowerState,
    pub cpu: CpuSpec,
    pub memory: MemorySpec,
    pub boot: BootSpec,
    #[serde(default)]
    pub disks: Vec<DiskSpec>,
    #[serde(default)]
    pub network_interfaces: Vec<NetworkInterfaceSpec>,
    #[serde(default)]
    pub placement: PlacementSpec,
    /// Optional cloud-init (NoCloud) configuration. When present, the agent
    /// generates a per-VM seed ISO and attaches it read-only (design M9).
    #[serde(default)]
    pub cloud_init: Option<CloudInitSpec>,
}

/// Observed state for a virtual machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualMachineStatus {
    pub phase: VmPhase,
    /// The host the VM is currently placed on, once scheduled.
    pub host_id: Option<HostId>,
    /// The `generation` the controller most recently reconciled.
    pub observed_generation: Generation,
    /// Human-readable detail for the current phase (errors, progress, ...).
    pub message: Option<String>,
    /// Primary IP once known (surfaced in the UI VM list, section 34).
    pub ip_address: Option<String>,
}

impl VirtualMachineStatus {
    /// The initial status of a freshly created VM.
    pub fn pending() -> Self {
        Self {
            phase: VmPhase::Pending,
            host_id: None,
            // No generation reconciled yet; 0 is strictly less than the
            // initial spec generation (1), so `needs_reconcile()` is true.
            observed_generation: Generation(0),
            message: None,
            ip_address: None,
        }
    }
}

/// The lifecycle phase of a VM (section 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum VmPhase {
    Pending,
    Scheduling,
    Creating,
    Stopped,
    Starting,
    Running,
    Stopping,
    Migrating,
    Failed,
    Deleting,
}

impl VmPhase {
    /// Phases from which no further automatic transition occurs without new
    /// desired state or operator action.
    pub fn is_settled(self) -> bool {
        matches!(self, VmPhase::Stopped | VmPhase::Running | VmPhase::Failed)
    }
}

/// Operator intent for whether a VM should be powered on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum DesiredPowerState {
    Running,
    Stopped,
}

/// vCPU sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuSpec {
    pub boot_vcpus: u32,
    pub max_vcpus: u32,
}

/// Memory sizing, in mebibytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemorySpec {
    pub size_mib: u64,
    /// Maximum memory (MiB) the VM can be grown to via hot-plug. When set and
    /// greater than `size_mib`, the VM boots with a resizable region so memory
    /// can be hot-plugged up to this cap; otherwise memory changes need a
    /// restart (design M10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_size_mib: Option<u64>,
}

impl MemorySpec {
    /// Size in bytes, as Cloud Hypervisor expects.
    pub fn size_bytes(&self) -> u64 {
        self.size_mib * 1024 * 1024
    }

    /// Bytes to reserve for the hot-plug region (`max - size`), or `None` when
    /// memory is not resizable.
    pub fn hotplug_bytes(&self) -> Option<u64> {
        match self.max_size_mib {
            Some(max) if max > self.size_mib => Some((max - self.size_mib) * 1024 * 1024),
            _ => None,
        }
    }
}

/// How the VM boots. MVP focuses on direct-kernel boot (section 24); firmware
/// boot is modelled so disk-image boot can follow without an API change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BootSpec {
    DirectKernel {
        kernel: PathBuf,
        #[serde(default)]
        initramfs: Option<PathBuf>,
        #[serde(default)]
        cmdline: Option<String>,
    },
    Firmware {
        firmware: PathBuf,
    },
}

/// A block device attached to the VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskSpec {
    pub path: PathBuf,
    #[serde(default)]
    pub readonly: bool,
    /// On-disk image format. Cloud Hypervisor v53+ supports raw and qcow2
    /// natively; specifying it explicitly avoids CH's deprecated auto-detection.
    #[serde(default)]
    pub image_type: DiskImageType,
    /// Base image to provision this disk from when `path` does not yet exist
    /// (design M9). For `qcow2` the base becomes a read-only backing file (thin,
    /// instant clone); for `raw` it is copied (reflink where supported). `None`
    /// means the disk already exists at `path` and is used as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<PathBuf>,
    /// Target virtual size in bytes for a provisioned disk. When larger than the
    /// base image the volume is grown; ignored if the disk already exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
}

impl DiskSpec {
    /// A writable raw disk at `path` (assumed to already exist).
    pub fn raw(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            readonly: false,
            image_type: DiskImageType::Raw,
            source: None,
            size_bytes: None,
        }
    }

    /// A writable disk provisioned from `source` at `path`, grown to `size_bytes`.
    pub fn provisioned(
        path: impl Into<PathBuf>,
        image_type: DiskImageType,
        source: impl Into<PathBuf>,
        size_bytes: Option<u64>,
    ) -> Self {
        Self {
            path: path.into(),
            readonly: false,
            image_type,
            source: Some(source.into()),
            size_bytes,
        }
    }

    /// Whether the agent must materialise this disk's backing file before the VM
    /// can launch: either cloned from a `source` image, or created blank at
    /// `size_bytes` (a data disk). A disk with neither is assumed to exist.
    pub fn needs_provisioning(&self) -> bool {
        self.source.is_some() || self.size_bytes.is_some()
    }

    /// A blank writable data disk to be created at `size_bytes` (design M10).
    pub fn blank(path: impl Into<PathBuf>, image_type: DiskImageType, size_bytes: u64) -> Self {
        Self {
            path: path.into(),
            readonly: false,
            image_type,
            source: None,
            size_bytes: Some(size_bytes),
        }
    }
}

/// The on-disk format of a volume. (Translation to Cloud Hypervisor's own
/// `image_type` enum lives in `ch-client`, per ADR-013.)
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiskImageType {
    #[default]
    Raw,
    Qcow2,
}

/// Cloud-init (NoCloud) configuration for a VM. The agent renders this into a
/// per-VM seed ISO (labelled `cidata`) with `meta-data` + `user-data` and
/// attaches it read-only (design M9).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudInitSpec {
    /// Guest hostname; defaults to the VM name when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// SSH public keys authorised for the default user.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ssh_authorized_keys: Vec<String>,
    /// Plain-text password for the default user (dev convenience; unlocks the
    /// account and does not expire).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Raw `#cloud-config` user-data. When set it fully replaces the generated
    /// user-data (the fields above are then ignored).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_data: Option<String>,
}

/// A virtual NIC attached to a network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterfaceSpec {
    pub network_id: NetworkId,
    /// Optional fixed MAC; the control plane allocates one when absent.
    #[serde(default)]
    pub mac: Option<String>,
    /// Operator-requested static IPs (IPv4 and/or IPv6) on a managed network
    /// (design M13a). Empty ⇒ the control plane auto-allocates from the
    /// network's configured families. Ignored on unmanaged (DHCP) networks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addresses: Vec<String>,
    /// Security groups applied to this NIC (design M13c). Empty ⇒ unfiltered.
    /// Otherwise: default-deny ingress + allow egress + stateful, with the
    /// union of the groups' ingress rules as the allow-list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub security_groups: Vec<uuid::Uuid>,
}

/// Deterministic, stable, locally-administered MAC for a VM's NIC, derived from
/// the VM id + NIC index (design section 23). Shared by the control plane (which
/// allocates it) and the agent (which re-derives it for IP discovery, M11), so
/// both always agree without persisting per-NIC state.
pub fn allocate_mac(vm_id: VmId, index: usize) -> String {
    let mixed = vm_id.as_uuid().as_u128()
        ^ (index as u128).wrapping_mul(0x9E37_79B9_7F4A_7C15_F39C_C060_5CED_C835);
    let b = mixed.to_le_bytes();
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        b[0], b[1], b[2], b[3], b[4]
    )
}

/// Placement constraints. Empty means "the scheduler decides" (section 17).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementSpec {
    /// Pin to a specific host. `None` lets the scheduler choose.
    #[serde(default)]
    pub host: Option<HostId>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 2,
                max_vcpus: 2,
            },
            memory: MemorySpec {
                size_mib: 2048,
                max_size_mib: None,
            },
            boot: BootSpec::DirectKernel {
                kernel: "/var/lib/ch/images/vmlinux".into(),
                initramfs: None,
                cmdline: Some("console=ttyS0".into()),
            },
            disks: vec![],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
            cloud_init: None,
        }
    }

    #[test]
    fn new_vm_needs_reconcile() {
        let now = DateTime::from_timestamp(0, 0).unwrap();
        let vm = VirtualMachine::new("test-vm", sample_spec(), now);
        assert_eq!(vm.status.phase, VmPhase::Pending);
        assert!(vm.needs_reconcile(), "a brand-new VM must be reconciled");
    }

    #[test]
    fn memory_bytes_conversion() {
        assert_eq!(
            MemorySpec {
                size_mib: 1,
                max_size_mib: None
            }
            .size_bytes(),
            1_048_576
        );
    }

    #[test]
    fn spec_json_roundtrips() {
        let spec = sample_spec();
        let json = serde_json::to_string(&spec).unwrap();
        let back: VirtualMachineSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn settled_phases() {
        assert!(VmPhase::Running.is_settled());
        assert!(VmPhase::Stopped.is_settled());
        assert!(!VmPhase::Creating.is_settled());
    }
}
