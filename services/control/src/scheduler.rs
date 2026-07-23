//! The initial scheduler (design document, section 17).
//!
//! Deliberately simple: filter hosts that cannot fit the VM, then score the
//! survivors and pick the best. The two concerns are kept as separate steps
//! (`passes_filters` / `score`) so a plugin framework can replace them later
//! without restructuring callers.

use ch_model::VirtualMachineSpec;
use uuid::Uuid;

use crate::store::Host;

/// Choose a host for `spec` from `hosts` (already restricted to Ready +
/// schedulable by the caller). Returns `None` when nothing fits.
pub fn schedule(spec: &VirtualMachineSpec, hosts: &[Host]) -> Option<Uuid> {
    hosts
        .iter()
        .filter(|h| passes_filters(spec, h))
        .max_by(|a, b| {
            score(a)
                .partial_cmp(&score(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|h| h.id)
}

/// Whether a host can satisfy the VM's CPU and memory requirements.
fn passes_filters(spec: &VirtualMachineSpec, host: &Host) -> bool {
    let Some(cpus) = host.logical_cpus else {
        return false;
    };
    let Some(available) = host.available_memory_bytes else {
        return false;
    };
    let cpu_ok = cpus as u32 >= spec.cpu.boot_vcpus;
    let mem_ok = available as u128 >= spec.memory.size_bytes() as u128;
    cpu_ok && mem_ok
}

/// Score a host: prefer the largest available-memory fraction (section 17,
/// "prefer host with largest available memory percentage").
fn score(host: &Host) -> f64 {
    match (host.available_memory_bytes, host.total_memory_bytes) {
        (Some(avail), Some(total)) if total > 0 => avail as f64 / total as f64,
        _ => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use ch_model::{BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec};
    use chrono::Utc;

    use super::*;

    fn host(name: &str, cpus: i32, avail_gib: i64, total_gib: i64) -> Host {
        let now = Utc::now();
        Host {
            id: Uuid::new_v4(),
            name: name.into(),
            endpoint: "http://x:9500".into(),
            schedulable: true,
            state: "Ready".into(),
            hostname: None,
            architecture: None,
            kernel_version: None,
            cloud_hypervisor_version: None,
            logical_cpus: Some(cpus),
            cpu_model: None,
            total_memory_bytes: Some(total_gib * 1024 * 1024 * 1024),
            available_memory_bytes: Some(avail_gib * 1024 * 1024 * 1024),
            vm_count: 0,
            last_heartbeat: None,
            created_at: now,
            updated_at: now,
            generation: 1,
        }
    }

    fn spec(vcpus: u32, mem_mib: u64) -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: vcpus,
                max_vcpus: vcpus,
            },
            memory: MemorySpec { size_mib: mem_mib },
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
    fn picks_host_with_most_free_memory_fraction() {
        let a = host("a", 8, 4, 16); // 25% free
        let b = host("b", 8, 12, 16); // 75% free
        let chosen = schedule(&spec(2, 2048), &[a.clone(), b.clone()]).unwrap();
        assert_eq!(chosen, b.id);
    }

    #[test]
    fn filters_hosts_without_enough_cpu() {
        let small = host("small", 1, 30, 32);
        assert!(schedule(&spec(4, 1024), &[small]).is_none());
    }

    #[test]
    fn filters_hosts_without_enough_memory() {
        let tight = host("tight", 16, 1, 32); // 1 GiB free
        assert!(schedule(&spec(2, 8192), &[tight]).is_none()); // needs 8 GiB
    }

    #[test]
    fn no_hosts_yields_none() {
        assert!(schedule(&spec(1, 512), &[]).is_none());
    }
}
