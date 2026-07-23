//! Validation of desired state before it is persisted.
//!
//! Validation lives in the domain model (not in the API or agent layers) so the
//! same rules apply no matter how a spec enters the system.

use ch_common::DomainError;
use thiserror::Error;

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
}

impl From<ValidationError> for DomainError {
    fn from(e: ValidationError) -> Self {
        DomainError::InvalidConfiguration(e.to_string())
    }
}

/// Minimum memory Cloud Hypervisor will realistically boot a guest with.
const MIN_MEMORY_MIB: u64 = 64;

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
        if self.memory.size_mib < MIN_MEMORY_MIB {
            return Err(ValidationError::MemoryTooSmall {
                min: MIN_MEMORY_MIB,
                got: self.memory.size_mib,
            });
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
            memory: MemorySpec { size_mib: mem },
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
    fn validation_error_maps_to_invalid_configuration() {
        let err: DomainError = ValidationError::ZeroBootVcpus.into();
        assert_eq!(err.code(), ch_common::ErrorCode::InvalidConfiguration);
    }
}
