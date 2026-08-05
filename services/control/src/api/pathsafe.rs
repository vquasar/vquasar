//! Confinement of caller-supplied host paths (design §30).
//!
//! A VM spec names files the agent opens with privilege: disk images, a kernel,
//! a firmware blob. Those names arrive from an API caller, and the agent trusts
//! the control plane, so the control plane is the only thing standing between
//! `vm:create` and "attach `/etc/vquasar/agent.key` as a raw disk and read it
//! from inside the guest". Every such path must therefore resolve inside an
//! allow-listed root before it is persisted.
//!
//! This is a confinement check, not a existence check: volumes are frequently
//! created after the spec is written. It rejects relative paths and any `..`
//! component, then requires the result to sit under a configured root. It does
//! not resolve symlinks — the roots hold platform-managed storage, and a
//! symlink planted there already implies host access.

use std::path::{Component, Path};

use crate::api::error::ApiError;

/// Check that `path` is absolute, traversal-free, and under one of `roots`.
///
/// `what` names the field in the error, so a rejection says which path was
/// refused without echoing anything the caller did not already send.
pub fn ensure_within(path: &Path, roots: &[String], what: &str) -> Result<(), ApiError> {
    if !path.is_absolute() {
        return Err(ApiError::invalid(format!(
            "{what} must be an absolute path"
        )));
    }
    // `..` is rejected outright rather than normalized away: with symlinks in
    // play, lexical normalization does not mean what it appears to mean.
    if path
        .components()
        .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return Err(ApiError::invalid(format!(
            "{what} must not contain '.' or '..'"
        )));
    }
    // `starts_with` compares whole components, so /var/lib/vquasar does not
    // admit /var/lib/vquasar-evil.
    if roots.iter().any(|r| path.starts_with(r)) {
        return Ok(());
    }
    Err(ApiError::invalid(format!(
        "{what} must be under one of the permitted storage roots: {}",
        roots.join(", ")
    )))
}

/// Check every host path a VM spec can carry.
pub fn ensure_spec_within(
    spec: &vquasar_model::VirtualMachineSpec,
    roots: &[String],
) -> Result<(), ApiError> {
    match &spec.boot {
        vquasar_model::BootSpec::DirectKernel {
            kernel, initramfs, ..
        } => {
            ensure_within(kernel, roots, "boot.kernel")?;
            if let Some(initramfs) = initramfs {
                ensure_within(initramfs, roots, "boot.initramfs")?;
            }
        }
        vquasar_model::BootSpec::Firmware { firmware } => {
            ensure_within(firmware, roots, "boot.firmware")?;
        }
    }
    for (i, disk) in spec.disks.iter().enumerate() {
        ensure_within(&disk.path, roots, &format!("disks[{i}].path"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn roots() -> Vec<String> {
        vec!["/var/lib/vquasar".to_string()]
    }

    #[test]
    fn a_path_under_the_root_is_allowed() {
        let p = PathBuf::from("/var/lib/vquasar/shared/volumes/vm.qcow2");
        assert!(ensure_within(&p, &roots(), "disk").is_ok());
    }

    /// The finding this exists for: the agent's private key is not a disk.
    #[test]
    fn the_agent_key_is_rejected() {
        let p = PathBuf::from("/etc/vquasar/tls/agent.key");
        let err = ensure_within(&p, &roots(), "disks[0].path").unwrap_err();
        assert!(format!("{err:?}").contains("permitted storage roots"));
    }

    #[test]
    fn traversal_out_of_the_root_is_rejected() {
        let p = PathBuf::from("/var/lib/vquasar/../../etc/vquasar/tls/agent.key");
        assert!(ensure_within(&p, &roots(), "disk").is_err());
    }

    /// Component-wise prefixing: a sibling directory sharing a name prefix is
    /// not inside the root.
    #[test]
    fn a_sibling_with_the_same_prefix_is_rejected() {
        let p = PathBuf::from("/var/lib/vquasar-evil/disk.qcow2");
        assert!(ensure_within(&p, &roots(), "disk").is_err());
    }

    #[test]
    fn relative_paths_are_rejected() {
        let p = PathBuf::from("shared/volumes/vm.qcow2");
        assert!(ensure_within(&p, &roots(), "disk").is_err());
    }

    #[test]
    fn multiple_roots_are_honoured() {
        let roots = vec!["/var/lib/vquasar".to_string(), "/srv/images".to_string()];
        assert!(ensure_within(&PathBuf::from("/srv/images/u.raw"), &roots, "d").is_ok());
        assert!(ensure_within(&PathBuf::from("/srv/other/u.raw"), &roots, "d").is_err());
    }

    #[test]
    fn a_spec_is_checked_across_boot_and_every_disk() {
        use vquasar_model::*;
        let mut spec = VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 2,
                max_vcpus: 2,
            },
            memory: MemorySpec {
                size_mib: 2048,
                max_size_mib: None,
            },
            boot: BootSpec::Firmware {
                firmware: PathBuf::from("/var/lib/vquasar/shared/firmware/CLOUDHV.fd"),
            },
            disks: vec![
                DiskSpec::raw("/var/lib/vquasar/shared/volumes/a.raw"),
                DiskSpec::raw("/etc/shadow"),
            ],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
            cloud_init: None,
            machine_type: MachineType::Standard,
        };
        let err = ensure_spec_within(&spec, &roots()).unwrap_err();
        assert!(format!("{err:?}").contains("disks[1].path"));

        spec.disks.remove(1);
        assert!(ensure_spec_within(&spec, &roots()).is_ok());

        spec.boot = BootSpec::Firmware {
            firmware: PathBuf::from("/etc/vquasar/tls/agent.key"),
        };
        let err = ensure_spec_within(&spec, &roots()).unwrap_err();
        assert!(format!("{err:?}").contains("boot.firmware"));
    }
}
