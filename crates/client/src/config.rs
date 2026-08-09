//! Cloud Hypervisor API request/response types and translation from the
//! orchestration model.
//!
//! The field names and shapes here match the Cloud Hypervisor OpenAPI schema
//! verbatim (`VmConfig`, `CpusConfig`, `MemoryConfig`, `PayloadConfig`,
//! `DiskConfig`, `NetConfig`, `ConsoleConfig`, `VmInfo`). Per ADR-013 these
//! types are private to `vquasar-client` and must not leak into the domain model —
//! [`to_vm_config`] is the one-way bridge.

use serde::{Deserialize, Serialize};
use vquasar_model::{BootSpec, DiskImageType, VirtualMachineSpec};

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
    /// Platform tuning (design M15, microVMs). Only set for the microVM
    /// profile; omitted otherwise so standard VMs keep CH's defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<PlatformConfig>,
    /// Guest-panic device (design M15). When `true`, CH exposes a `pvpanic`
    /// device so a kernel panic in the guest is reported to the host.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pvpanic: bool,
}

/// Cloud Hypervisor `PlatformConfig` (subset). `num_pci_segments` bounds the
/// PCI topology; microVMs pin it to a single segment for a minimal bus.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlatformConfig {
    pub num_pci_segments: u16,
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
    /// Size in bytes of the resizable region reserved at boot for memory
    /// hot-plug. Without it `vm.resize` cannot grow guest RAM (design M10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotplug_size: Option<u64>,
}

/// Body of `PUT /api/v1/vm.resize` — hot-plug vCPUs and/or memory (design M10).
#[derive(Debug, Clone, Default, Serialize)]
pub struct VmResize {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_vcpus: Option<u32>,
    /// New total guest RAM in bytes (must be within `size + hotplug_size`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub desired_ram: Option<u64>,
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
    /// `O_DIRECT`: bypass the host page cache (design §20).
    ///
    /// Skipped when false so a VM with no storage policy serialises exactly the
    /// bytes it did before this field existed — the containment that makes this
    /// safe to add to a fleet mid-flight.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub direct: bool,
    /// Token-bucket I/O ceilings (design §20).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limiter_config: Option<RateLimiterConfig>,
}

/// Cloud Hypervisor `RateLimiterConfig`: one bucket for throughput, one for
/// operations. Either may be absent, meaning unlimited in that dimension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct RateLimiterConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bandwidth: Option<TokenBucketConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<TokenBucketConfig>,
}

/// Cloud Hypervisor `TokenBucketConfig`.
///
/// `size` tokens are granted every `refill_time` milliseconds, so a rate of
/// *n* per second is `size = n` with `refill_time = 1000`. `one_time_burst` is
/// a one-off allowance on top, left unset here: a burst that only applies once
/// per boot is a strange thing to express as policy, and the ceiling is what
/// an operator means.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TokenBucketConfig {
    pub size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_time_burst: Option<u64>,
    pub refill_time: u64,
}

/// The refill window every ceiling is expressed against: one second.
const REFILL_MS: u64 = 1000;

impl TokenBucketConfig {
    /// A ceiling of `per_second`, as a bucket.
    fn per_second(per_second: u64) -> Self {
        Self {
            size: per_second,
            one_time_burst: None,
            refill_time: REFILL_MS,
        }
    }
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
/// Translate one domain [`DiskSpec`] into a CH [`DiskConfig`]. Also used to
/// hot-add a disk to a running VM (design M10).
pub fn disk_config(d: &vquasar_model::DiskSpec) -> DiskConfig {
    let policy = d.policy.as_ref();
    let limits = policy.and_then(|p| {
        let bandwidth = p.bandwidth_bytes_per_sec.map(TokenBucketConfig::per_second);
        let ops = p.iops.map(TokenBucketConfig::per_second);
        // No ceiling in either dimension is no rate limiter at all, not an
        // empty one: an empty `rate_limiter_config` is a different thing to
        // send, and this path has to keep producing today's bytes.
        (bandwidth.is_some() || ops.is_some()).then_some(RateLimiterConfig { bandwidth, ops })
    });
    DiskConfig {
        path: d.path.to_string_lossy().into_owned(),
        readonly: d.readonly,
        image_type: Some(match d.image_type {
            DiskImageType::Raw => ImageType::Raw,
            DiskImageType::Qcow2 => ImageType::Qcow2,
        }),
        direct: policy.is_some_and(|p| p.cache == vquasar_model::DiskCache::Direct),
        rate_limiter_config: limits,
    }
}

/// Translate a resolved [`TapBinding`] into a CH [`NetConfig`]. Also used to
/// hot-add a NIC to a running VM (design M10).
pub fn net_config(t: &TapBinding) -> NetConfig {
    NetConfig {
        tap: Some(t.tap.clone()),
        mac: t.mac.clone(),
    }
}

/// This is the sole bridge from the stable domain model to CH's wire format.
pub fn to_vm_config(spec: &VirtualMachineSpec, opts: &TranslateOptions) -> VmConfig {
    let cpus = CpusConfig {
        boot_vcpus: spec.cpu.boot_vcpus,
        max_vcpus: spec.cpu.max_vcpus,
    };

    let memory = MemoryConfig {
        size: spec.memory.size_bytes(),
        // Reserve a resizable region when the spec allows growth, so memory can
        // be hot-plugged later up to `max_size_mib` (design M10).
        hotplug_size: spec.memory.hotplug_bytes(),
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

    let disks = spec.disks.iter().map(disk_config).collect();
    let net = opts.taps.iter().map(net_config).collect();

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

    // microVM profile (design M15): pin a single PCI segment and enable the
    // guest-panic device. Standard VMs keep CH's defaults (no platform block,
    // pvpanic off).
    let microvm = spec.machine_type.is_microvm();
    let platform = microvm.then_some(PlatformConfig {
        num_pci_segments: 1,
    });

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
        platform,
        pvpanic: microvm,
    }
}

#[cfg(test)]
mod tests {
    use vquasar_model::{CpuSpec, DesiredPowerState, DiskSpec, MemorySpec, PlacementSpec};

    use super::*;

    fn spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 2,
                max_vcpus: 4,
            },
            memory: MemorySpec {
                size_mib: 2048,
                max_size_mib: None,
            },
            boot: BootSpec::DirectKernel {
                kernel: "/var/lib/ch/images/vmlinux".into(),
                initramfs: Some("/var/lib/ch/images/initramfs".into()),
                cmdline: Some("console=ttyS0".into()),
            },
            disks: vec![DiskSpec::raw("/var/lib/vquasar/volumes/root.raw")],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
            cloud_init: None,
            machine_type: vquasar_model::MachineType::Standard,
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
    fn standard_vm_has_no_microvm_devices() {
        let cfg = to_vm_config(&spec(), &TranslateOptions::default());
        assert!(!cfg.pvpanic);
        assert!(cfg.platform.is_none());
        // pvpanic must be omitted from the wire body when false.
        let value = serde_json::to_value(&cfg).unwrap();
        assert!(value.get("pvpanic").is_none());
        assert!(value.get("platform").is_none());
    }

    #[test]
    fn microvm_enables_pvpanic_and_single_pci_segment() {
        let mut s = spec();
        s.machine_type = vquasar_model::MachineType::MicroVm;
        let cfg = to_vm_config(&s, &TranslateOptions::default());
        assert!(cfg.pvpanic);
        assert_eq!(cfg.platform.as_ref().unwrap().num_pci_segments, 1);
        let value = serde_json::to_value(&cfg).unwrap();
        assert_eq!(value["pvpanic"], true);
        assert_eq!(value["platform"]["num_pci_segments"], 1);
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
    /// A disk with no policy must produce the same JSON Cloud Hypervisor was
    /// being sent before policy existed. This is the containment the whole
    /// feature rests on: an existing fleet's VMs are byte-identical.
    #[test]
    fn a_disk_without_a_policy_sends_what_it_always_did() {
        let d = vquasar_model::DiskSpec::raw("/x/a.raw");
        let value = serde_json::to_value(disk_config(&d)).unwrap();
        assert!(value.get("direct").is_none(), "{value}");
        assert!(value.get("rate_limiter_config").is_none(), "{value}");
        assert_eq!(value["path"], "/x/a.raw");
    }

    #[test]
    fn direct_cache_becomes_o_direct() {
        let mut d = vquasar_model::DiskSpec::raw("/x/a.raw");
        d.policy = Some(vquasar_model::StoragePolicy {
            cache: vquasar_model::DiskCache::Direct,
            ..Default::default()
        });
        let value = serde_json::to_value(disk_config(&d)).unwrap();
        assert_eq!(value["direct"], true);
        // Cache is not a rate limit; asking for one must not invent the other.
        assert!(value.get("rate_limiter_config").is_none(), "{value}");
    }

    /// A ceiling of *n* per second is a bucket of *n* refilled every 1000ms.
    #[test]
    fn ceilings_become_one_second_token_buckets() {
        let mut d = vquasar_model::DiskSpec::raw("/x/a.raw");
        d.policy = Some(vquasar_model::StoragePolicy {
            bandwidth_bytes_per_sec: Some(50 * 1024 * 1024),
            iops: Some(2000),
            ..Default::default()
        });
        let value = serde_json::to_value(disk_config(&d)).unwrap();
        let rl = &value["rate_limiter_config"];
        assert_eq!(rl["bandwidth"]["size"], 50 * 1024 * 1024);
        assert_eq!(rl["bandwidth"]["refill_time"], 1000);
        assert_eq!(rl["ops"]["size"], 2000);
        assert_eq!(rl["ops"]["refill_time"], 1000);
        // A burst that only applies once per boot is not what an operator
        // means by a ceiling, so it is left unset rather than guessed.
        assert!(rl["bandwidth"].get("one_time_burst").is_none(), "{rl}");
    }

    /// One dimension limited, the other not: the unlimited one is absent, not
    /// present as a zero bucket, which would stop the disk.
    #[test]
    fn limiting_one_dimension_leaves_the_other_alone() {
        let mut d = vquasar_model::DiskSpec::raw("/x/a.raw");
        d.policy = Some(vquasar_model::StoragePolicy {
            iops: Some(2000),
            ..Default::default()
        });
        let value = serde_json::to_value(disk_config(&d)).unwrap();
        let rl = &value["rate_limiter_config"];
        assert_eq!(rl["ops"]["size"], 2000);
        assert!(rl.get("bandwidth").is_none(), "{rl}");
    }

    /// A policy that sets only non-limiting fields must not produce an empty
    /// rate limiter — an empty one is a different thing to send.
    #[test]
    fn a_policy_with_no_ceilings_sends_no_rate_limiter() {
        let mut d = vquasar_model::DiskSpec::raw("/x/a.raw");
        d.policy = Some(vquasar_model::StoragePolicy {
            allocation: vquasar_model::Allocation::Thick,
            ..Default::default()
        });
        let value = serde_json::to_value(disk_config(&d)).unwrap();
        assert!(value.get("rate_limiter_config").is_none(), "{value}");
        // Allocation is a provisioning concern; it must not leak onto the wire
        // as a runtime option Cloud Hypervisor has no field for.
        assert!(value.get("allocation").is_none(), "{value}");
    }
}
