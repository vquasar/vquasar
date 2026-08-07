//! Validation of desired state before it is persisted.
//!
//! Validation lives in the domain model (not in the API or agent layers) so the
//! same rules apply no matter how a spec enters the system.

use thiserror::Error;
use vquasar_common::DomainError;

use crate::vm::{BootSpec, VirtualMachineSpec};

/// A specific reason a spec was rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ValidationError {
    #[error("boot_vcpus must be at least 1")]
    ZeroBootVcpus,

    #[error("max_vcpus ({max}) must be >= boot_vcpus ({boot})")]
    MaxVcpusBelowBoot { boot: u32, max: u32 },

    #[error("memory size must be at least {min} MiB, got {got}")]
    MemoryTooSmall { min: u64, got: u64 },

    #[error("boot kernel path must not be empty")]
    EmptyKernelPath,

    #[error("boot firmware path must not be empty")]
    EmptyFirmwarePath,

    #[error("disk path must not be empty")]
    EmptyDiskPath,

    #[error("a microVM must use direct-kernel boot (firmware boot is not supported)")]
    MicrovmRequiresDirectKernel,

    #[error("a microVM cannot use a cloud-init seed disk; configure it via the kernel cmdline or initramfs instead")]
    MicrovmForbidsCloudInit,

    #[error("max_vcpus ({got}) exceeds the limit of {max}")]
    TooManyVcpus { got: u32, max: u32 },

    #[error("memory size {got} MiB exceeds the limit of {max} MiB")]
    MemoryTooLarge { got: u64, max: u64 },

    #[error("{got} disks exceeds the limit of {max}")]
    TooManyDisks { got: usize, max: usize },

    #[error("{got} network interfaces exceeds the limit of {max}")]
    TooManyNics { got: usize, max: usize },

    #[error("disk {index} is {got} bytes, which exceeds the limit of {max}")]
    DiskTooLarge { index: usize, got: u64, max: u64 },
}

impl From<ValidationError> for DomainError {
    fn from(e: ValidationError) -> Self {
        DomainError::InvalidConfiguration(e.to_string())
    }
}

/// Minimum memory Cloud Hypervisor will realistically boot a guest with.
const MIN_MEMORY_MIB: u64 = 64;

// Upper bounds. Validation previously enforced only minimums, so a single
// request could ask for 4096 vCPUs or 64 TiB of memory. A VM like that never
// schedules — but it is admitted into desired state, and the reconcile loop
// retries it forever; the disk and image paths are worse, because those do the
// work immediately on shared storage.
//
// These are deliberately generous: they exist to stop absurd requests, not to
// express policy. Per-tenant quotas are the mechanism for policy, and they are
// a separate milestone.

/// More vCPUs than any host in a sane fleet has threads.
const MAX_VCPUS: u32 = 512;
/// 4 TiB.
const MAX_MEMORY_MIB: u64 = 4 * 1024 * 1024;
/// Cloud Hypervisor's PCI topology runs out long before this.
const MAX_DISKS: usize = 64;
const MAX_NICS: usize = 16;
/// 64 TiB — beyond any single volume a lab or a normal deployment provisions.
pub const MAX_DISK_BYTES: u64 = 64 * 1024 * 1024 * 1024 * 1024;

impl VirtualMachineSpec {
    /// Validate desired VM state. Returns the *first* violation found.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.cpu.boot_vcpus < 1 {
            return Err(ValidationError::ZeroBootVcpus);
        }
        if self.cpu.max_vcpus < self.cpu.boot_vcpus {
            return Err(ValidationError::MaxVcpusBelowBoot {
                boot: self.cpu.boot_vcpus,
                max: self.cpu.max_vcpus,
            });
        }
        if self.cpu.max_vcpus > MAX_VCPUS {
            return Err(ValidationError::TooManyVcpus {
                got: self.cpu.max_vcpus,
                max: MAX_VCPUS,
            });
        }
        if self.memory.size_mib < MIN_MEMORY_MIB {
            return Err(ValidationError::MemoryTooSmall {
                min: MIN_MEMORY_MIB,
                got: self.memory.size_mib,
            });
        }
        // Check the hot-plug ceiling too: it is what the VM can grow to, and it
        // is what CH reserves address space for at boot.
        let peak_memory = self.memory.max_size_mib.unwrap_or(self.memory.size_mib);
        if peak_memory > MAX_MEMORY_MIB {
            return Err(ValidationError::MemoryTooLarge {
                got: peak_memory,
                max: MAX_MEMORY_MIB,
            });
        }
        if self.disks.len() > MAX_DISKS {
            return Err(ValidationError::TooManyDisks {
                got: self.disks.len(),
                max: MAX_DISKS,
            });
        }
        if self.network_interfaces.len() > MAX_NICS {
            return Err(ValidationError::TooManyNics {
                got: self.network_interfaces.len(),
                max: MAX_NICS,
            });
        }
        for (index, disk) in self.disks.iter().enumerate() {
            if let Some(size) = disk.size_bytes {
                if size > MAX_DISK_BYTES {
                    return Err(ValidationError::DiskTooLarge {
                        index,
                        got: size,
                        max: MAX_DISK_BYTES,
                    });
                }
            }
        }
        match &self.boot {
            BootSpec::DirectKernel { kernel, .. } => {
                if kernel.as_os_str().is_empty() {
                    return Err(ValidationError::EmptyKernelPath);
                }
            }
            BootSpec::Firmware { firmware } => {
                if firmware.as_os_str().is_empty() {
                    return Err(ValidationError::EmptyFirmwarePath);
                }
            }
        }
        for disk in &self.disks {
            if disk.path.as_os_str().is_empty() {
                return Err(ValidationError::EmptyDiskPath);
            }
        }
        // microVM profile (design M15): a minimal, fast-booting shape. Firmware
        // boot (BIOS/UEFI) and the cloud-init seed disk both defeat that, so
        // they're rejected rather than silently downgrading the profile.
        if self.machine_type.is_microvm() {
            if !matches!(self.boot, BootSpec::DirectKernel { .. }) {
                return Err(ValidationError::MicrovmRequiresDirectKernel);
            }
            if self.cloud_init.is_some() {
                return Err(ValidationError::MicrovmForbidsCloudInit);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::vm::{
        BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec, VirtualMachineSpec,
    };

    use super::*;

    fn spec(boot_vcpus: u32, max_vcpus: u32, mem: u64) -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus,
                max_vcpus,
            },
            memory: MemorySpec {
                size_mib: mem,
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
            machine_type: crate::vm::MachineType::Standard,
        }
    }

    #[test]
    fn valid_spec_passes() {
        assert!(spec(2, 4, 2048).validate().is_ok());
    }

    #[test]
    fn zero_vcpus_rejected() {
        assert_eq!(
            spec(0, 1, 2048).validate(),
            Err(ValidationError::ZeroBootVcpus)
        );
    }

    #[test]
    fn max_below_boot_rejected() {
        assert_eq!(
            spec(4, 2, 2048).validate(),
            Err(ValidationError::MaxVcpusBelowBoot { boot: 4, max: 2 })
        );
    }

    #[test]
    fn tiny_memory_rejected() {
        assert_eq!(
            spec(1, 1, 16).validate(),
            Err(ValidationError::MemoryTooSmall { min: 64, got: 16 })
        );
    }

    #[test]
    fn empty_kernel_rejected() {
        let mut s = spec(1, 1, 128);
        s.boot = BootSpec::DirectKernel {
            kernel: "".into(),
            initramfs: None,
            cmdline: None,
        };
        assert_eq!(s.validate(), Err(ValidationError::EmptyKernelPath));
    }

    #[test]
    fn microvm_diskless_directkernel_passes() {
        let mut s = spec(1, 1, 128);
        s.machine_type = crate::vm::MachineType::MicroVm;
        assert!(s.validate().is_ok());
    }

    #[test]
    fn microvm_rejects_firmware_boot() {
        let mut s = spec(1, 1, 128);
        s.machine_type = crate::vm::MachineType::MicroVm;
        s.boot = BootSpec::Firmware {
            firmware: "/usr/share/ovmf/OVMF.fd".into(),
        };
        assert_eq!(
            s.validate(),
            Err(ValidationError::MicrovmRequiresDirectKernel)
        );
    }

    #[test]
    fn microvm_rejects_cloud_init() {
        let mut s = spec(1, 1, 128);
        s.machine_type = crate::vm::MachineType::MicroVm;
        s.cloud_init = Some(crate::vm::CloudInitSpec {
            hostname: Some("m".into()),
            ssh_authorized_keys: vec![],
            password: None,
            user_data: None,
        });
        assert_eq!(s.validate(), Err(ValidationError::MicrovmForbidsCloudInit));
    }

    #[test]
    fn validation_error_maps_to_invalid_configuration() {
        let err: DomainError = ValidationError::ZeroBootVcpus.into();
        assert_eq!(err.code(), vquasar_common::ErrorCode::InvalidConfiguration);
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;
    use crate::vm::*;
    use std::path::PathBuf;

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
            boot: BootSpec::Firmware {
                firmware: PathBuf::from("/var/lib/vquasar/f.fd"),
            },
            disks: vec![],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
            cloud_init: None,
            machine_type: MachineType::Standard,
        }
    }

    /// A spec far beyond any real host was accepted into desired state, where
    /// the reconcile loop then retried it forever.
    #[test]
    fn absurd_cpu_and_memory_are_refused() {
        let mut s = spec();
        s.cpu.max_vcpus = 4096;
        s.cpu.boot_vcpus = 4096;
        assert!(matches!(
            s.validate(),
            Err(ValidationError::TooManyVcpus { .. })
        ));

        let mut s = spec();
        s.memory.size_mib = 64 * 1024 * 1024; // 64 TiB
        assert!(matches!(
            s.validate(),
            Err(ValidationError::MemoryTooLarge { .. })
        ));
    }

    /// The hot-plug ceiling counts too: it is what CH reserves address space
    /// for, so a small boot size with an absurd max is still absurd.
    #[test]
    fn the_hotplug_ceiling_is_bounded_as_well() {
        let mut s = spec();
        s.memory.size_mib = 512;
        s.memory.max_size_mib = Some(64 * 1024 * 1024);
        assert!(matches!(
            s.validate(),
            Err(ValidationError::MemoryTooLarge { .. })
        ));
    }

    #[test]
    fn device_counts_are_bounded() {
        let mut s = spec();
        s.disks = (0..100)
            .map(|i| DiskSpec::raw(format!("/var/lib/vquasar/d{i}.raw")))
            .collect();
        assert!(matches!(
            s.validate(),
            Err(ValidationError::TooManyDisks { .. })
        ));
    }

    #[test]
    fn a_single_disk_cannot_ask_for_the_whole_datacentre() {
        let mut s = spec();
        let mut d = DiskSpec::raw("/var/lib/vquasar/big.raw");
        d.size_bytes = Some(MAX_DISK_BYTES + 1);
        s.disks = vec![d];
        assert!(matches!(
            s.validate(),
            Err(ValidationError::DiskTooLarge { .. })
        ));
    }

    /// The bounds must not reject anything a real deployment would ask for.
    #[test]
    fn ordinary_specs_still_pass() {
        let mut s = spec();
        s.cpu = CpuSpec {
            boot_vcpus: 8,
            max_vcpus: 32,
        };
        s.memory = MemorySpec {
            size_mib: 32768,
            max_size_mib: Some(131072),
        };
        s.disks = vec![DiskSpec::raw("/var/lib/vquasar/a.raw")];
        assert!(s.validate().is_ok(), "{:?}", s.validate());
    }
}
