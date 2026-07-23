//! Cloud Hypervisor API request/response types and translation from the
//! orchestration model.
//!
//! The field names and shapes here match the Cloud Hypervisor OpenAPI schema
//! verbatim (`VmConfig`, `CpusConfig`, `MemoryConfig`, `PayloadConfig`,
//! `DiskConfig`, `NetConfig`, `ConsoleConfig`, `VmInfo`). Per ADR-013 these
//! types are private to `ch-client` and must not leak into the domain model —
//! [`to_vm_config`] is the one-way bridge.

use ch_model::{BootSpec, DiskImageType, VirtualMachineSpec};
use serde::{Deserialize, Serialize};

use crate::hypervisor::{HypervisorState, HypervisorVmInfo};

/// Cloud Hypervisor `VmConfig` (the body of `PUT /api/v1/vm.create`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VmConfig {
    pub cpus: CpusConfig,
    pub memory: MemoryConfig,
    pub payload: PayloadConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disks: Vec<DiskConfig>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub net: Vec<NetConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<ConsoleConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub console: Option<ConsoleConfig>,
}

/// Cloud Hypervisor `CpusConfig`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CpusConfig {
    pub boot_vcpus: u32,
    pub max_vcpus: u32,
}

/// Cloud Hypervisor `MemoryConfig`. `size` is in **bytes**.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryConfig {
    pub size: u64,
}

/// Cloud Hypervisor `PayloadConfig`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PayloadConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub firmware: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initramfs: Option<String>,
}

/// Cloud Hypervisor `DiskConfig` (subset used by the MVP).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskConfig {
    pub path: String,
    #[serde(default)]
    pub readonly: bool,
    /// Set explicitly to avoid CH's deprecated image-type auto-detection.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_type: Option<ImageType>,
}

/// Cloud Hypervisor `ImageType`. The JSON API uses PascalCase variant names
/// (note: distinct from the CLI's lowercase `image_type=raw`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ImageType {
    Raw,
    Qcow2,
    Vhdx,
    FixedVhd,
    Unknown,
}

/// Cloud Hypervisor `NetConfig` (subset used by the MVP).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NetConfig {
    /// Name of a pre-created host TAP device (created by the network backend).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tap: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mac: Option<String>,
}

/// Cloud Hypervisor `ConsoleConfig`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConsoleConfig {
    pub mode: ConsoleMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socket: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
}

/// Cloud Hypervisor `ConsoleMode`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConsoleMode {
    Off,
    Pty,
    Tty,
    File,
    Socket,
    Null,
}

/// Cloud Hypervisor `VmInfo` (response of `GET /api/v1/vm.info`).
#[derive(Debug, Clone, Deserialize)]
pub struct VmInfo {
    pub state: VmState,
}

/// Cloud Hypervisor `VmState`.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum VmState {
    Created,
    Running,
    Shutdown,
    Paused,
}

impl From<VmInfo> for HypervisorVmInfo {
    fn from(info: VmInfo) -> Self {
        HypervisorVmInfo {
            state: match info.state {
                VmState::Created => HypervisorState::Created,
                VmState::Running => HypervisorState::Running,
                VmState::Shutdown => HypervisorState::Shutdown,
                VmState::Paused => HypervisorState::Paused,
            },
        }
    }
}

/// Body of `PUT /api/v1/vm.receive-migration`.
#[derive(Debug, Serialize)]
pub struct ReceiveMigrationData<'a> {
    pub receiver_url: &'a str,
}

/// Body of `PUT /api/v1/vm.send-migration`.
#[derive(Debug, Serialize)]
pub struct SendMigrationData<'a> {
    pub destination_url: &'a str,
    pub local: bool,
}

/// A host TAP device bound to a VM NIC, resolved by the network backend before
/// VM creation.
#[derive(Debug, Clone, Default)]
pub struct TapBinding {
    pub tap: String,
    pub mac: Option<String>,
}

/// Where the guest serial port is directed.
#[derive(Debug, Clone, Default)]
pub enum SerialTarget {
    /// Serial disabled.
    #[default]
    Off,
    /// Serial written to a file (simplest for capturing boot output).
    File(String),
    /// Serial exposed on a Unix socket (used by the web console, section 25).
    Socket(String),
}

/// Extra inputs needed to translate a spec that are not part of the domain
/// spec itself (they come from the agent/host layer).
#[derive(Debug, Clone, Default)]
pub struct TranslateOptions {
    /// Where the guest serial console is directed.
    pub serial: SerialTarget,
    /// Resolved host TAP devices, one per NIC. Empty in the pre-networking MVP.
    pub taps: Vec<TapBinding>,
}

/// Translate an orchestration [`VirtualMachineSpec`] into a Cloud Hypervisor
/// [`VmConfig`].
///
/// This is the sole bridge from the stable domain model to CH's wire format.
pub fn to_vm_config(spec: &VirtualMachineSpec, opts: &TranslateOptions) -> VmConfig {
    let cpus = CpusConfig {
        boot_vcpus: spec.cpu.boot_vcpus,
        max_vcpus: spec.cpu.max_vcpus,
    };

    let memory = MemoryConfig {
        size: spec.memory.size_bytes(),
    };

    let payload = match &spec.boot {
        BootSpec::DirectKernel {
            kernel,
            initramfs,
            cmdline,
        } => PayloadConfig {
            firmware: None,
            kernel: Some(kernel.to_string_lossy().into_owned()),
            cmdline: cmdline.clone(),
            initramfs: initramfs.as_ref().map(|p| p.to_string_lossy().into_owned()),
        },
        BootSpec::Firmware { firmware } => PayloadConfig {
            firmware: Some(firmware.to_string_lossy().into_owned()),
            ..PayloadConfig::default()
        },
    };

    let disks = spec
        .disks
        .iter()
        .map(|d| DiskConfig {
            path: d.path.to_string_lossy().into_owned(),
            readonly: d.readonly,
            image_type: Some(match d.image_type {
                DiskImageType::Raw => ImageType::Raw,
                DiskImageType::Qcow2 => ImageType::Qcow2,
            }),
        })
        .collect();

    let net = opts
        .taps
        .iter()
        .map(|t| NetConfig {
            tap: Some(t.tap.clone()),
            mac: t.mac.clone(),
        })
        .collect();

    let serial = match &opts.serial {
        SerialTarget::Off => ConsoleConfig {
            mode: ConsoleMode::Off,
            socket: None,
            file: None,
        },
        SerialTarget::File(path) => ConsoleConfig {
            mode: ConsoleMode::File,
            socket: None,
            file: Some(path.clone()),
        },
        SerialTarget::Socket(path) => ConsoleConfig {
            mode: ConsoleMode::Socket,
            socket: Some(path.clone()),
            file: None,
        },
    };

    VmConfig {
        cpus,
        memory,
        payload,
        disks,
        net,
        serial: Some(serial),
        // Direct-kernel guests use the serial console; the virtio console is
        // left off for the MVP.
        console: Some(ConsoleConfig {
            mode: ConsoleMode::Off,
            socket: None,
            file: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use ch_model::{CpuSpec, DesiredPowerState, DiskSpec, MemorySpec, PlacementSpec};

    use super::*;

    fn spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 2,
                max_vcpus: 4,
            },
            memory: MemorySpec { size_mib: 2048 },
            boot: BootSpec::DirectKernel {
                kernel: "/var/lib/ch/images/vmlinux".into(),
                initramfs: Some("/var/lib/ch/images/initramfs".into()),
                cmdline: Some("console=ttyS0".into()),
            },
            disks: vec![DiskSpec::raw("/var/lib/ch-orchestrator/volumes/root.raw")],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
        }
    }

    #[test]
    fn translates_cpu_memory_and_payload() {
        let cfg = to_vm_config(&spec(), &TranslateOptions::default());
        assert_eq!(cfg.cpus.boot_vcpus, 2);
        assert_eq!(cfg.cpus.max_vcpus, 4);
        assert_eq!(cfg.memory.size, 2048 * 1024 * 1024);
        assert_eq!(
            cfg.payload.kernel.as_deref(),
            Some("/var/lib/ch/images/vmlinux")
        );
        assert_eq!(cfg.payload.cmdline.as_deref(), Some("console=ttyS0"));
        assert_eq!(
            cfg.payload.initramfs.as_deref(),
            Some("/var/lib/ch/images/initramfs")
        );
        assert_eq!(cfg.disks.len(), 1);
        assert_eq!(cfg.disks[0].image_type, Some(ImageType::Raw));
    }

    #[test]
    fn serialized_json_uses_cloud_hypervisor_field_names() {
        let cfg = to_vm_config(&spec(), &TranslateOptions::default());
        let value: serde_json::Value = serde_json::to_value(&cfg).unwrap();

        // Field names must match the CH API exactly.
        assert!(value["cpus"]["boot_vcpus"].is_number());
        assert!(value["cpus"]["max_vcpus"].is_number());
        assert_eq!(value["memory"]["size"], 2048u64 * 1024 * 1024);
        assert_eq!(value["payload"]["kernel"], "/var/lib/ch/images/vmlinux");
        // `firmware` must be absent for a direct-kernel boot.
        assert!(value["payload"].get("firmware").is_none());
        // image_type must serialize as CH's PascalCase enum, not lowercase.
        assert_eq!(value["disks"][0]["image_type"], "Raw");
    }

    #[test]
    fn firmware_boot_sets_firmware_only() {
        let mut s = spec();
        s.boot = BootSpec::Firmware {
            firmware: "/usr/share/cloud-hypervisor/CLOUDHV.fd".into(),
        };
        let cfg = to_vm_config(&s, &TranslateOptions::default());
        assert!(cfg.payload.kernel.is_none());
        assert_eq!(
            cfg.payload.firmware.as_deref(),
            Some("/usr/share/cloud-hypervisor/CLOUDHV.fd")
        );
    }

    #[test]
    fn serial_socket_becomes_socket_console() {
        let opts = TranslateOptions {
            serial: SerialTarget::Socket("/run/ch/vm/serial.sock".into()),
            taps: vec![],
        };
        let cfg = to_vm_config(&spec(), &opts);
        let serial = cfg.serial.expect("serial console configured");
        assert_eq!(serial.mode, ConsoleMode::Socket);
        assert_eq!(serial.socket.as_deref(), Some("/run/ch/vm/serial.sock"));
    }

    #[test]
    fn serial_file_becomes_file_console() {
        let opts = TranslateOptions {
            serial: SerialTarget::File("/run/ch/vm/serial.log".into()),
            taps: vec![],
        };
        let cfg = to_vm_config(&spec(), &opts);
        let serial = cfg.serial.expect("serial console configured");
        assert_eq!(serial.mode, ConsoleMode::File);
        assert_eq!(serial.file.as_deref(), Some("/run/ch/vm/serial.log"));
    }

    #[test]
    fn taps_become_net_configs() {
        let opts = TranslateOptions {
            serial: SerialTarget::Off,
            taps: vec![TapBinding {
                tap: "tap40340f77".into(),
                mac: Some("02:00:00:00:00:01".into()),
            }],
        };
        let cfg = to_vm_config(&spec(), &opts);
        assert_eq!(cfg.net.len(), 1);
        assert_eq!(cfg.net[0].tap.as_deref(), Some("tap40340f77"));
        assert_eq!(cfg.net[0].mac.as_deref(), Some("02:00:00:00:00:01"));
    }

    #[test]
    fn vm_state_maps_to_hypervisor_state() {
        let info: HypervisorVmInfo = VmInfo {
            state: VmState::Running,
        }
        .into();
        assert_eq!(info.state, HypervisorState::Running);
    }
}
