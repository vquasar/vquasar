//! The scheduler (design document, section 17).
//!
//! Filter hosts that cannot fit the VM, then score the survivors and pick the
//! best. Capacity is tracked as a *logical* model: a host's usable resources
//! are its reported totals minus the resources already committed to VMs placed
//! on it. This is what makes placement spread across hosts as load grows,
//! rather than depending on instantaneous free RAM (which is unstable and,
//! for co-located agents, indistinguishable).
//!
//! `filter` and `score` are kept as separate steps so a plugin framework can
//! replace them later without restructuring callers.

use std::collections::HashMap;

use ch_model::VirtualMachineSpec;
use uuid::Uuid;

/// Resources already committed to VMs on a host.
#[derive(Debug, Clone, Copy, Default)]
pub struct HostCommit {
    pub vcpus: i64,
    pub memory_bytes: i64,
}

/// Choose a host for `spec` from `hosts` (already restricted to Ready +
/// schedulable by the caller), given the resources already `committed` per
/// host id. Returns `None` when nothing fits.
pub fn schedule(
    spec: &VirtualMachineSpec,
    hosts: &[Host],
    committed: &HashMap<Uuid, HostCommit>,
) -> Option<Uuid> {
    hosts
        .iter()
        .filter(|h| passes_filters(spec, h, commit_of(committed, h.id)))
        .max_by(|a, b| {
            let sa = score(a, commit_of(committed, a.id));
            let sb = score(b, commit_of(committed, b.id));
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|h| h.id)
}

fn commit_of(committed: &HashMap<Uuid, HostCommit>, id: Uuid) -> HostCommit {
    committed.get(&id).copied().unwrap_or_default()
}

/// Whether a host has enough *uncommitted* CPU and memory for the VM.
fn passes_filters(spec: &VirtualMachineSpec, host: &Host, commit: HostCommit) -> bool {
    let Some(cpus) = host.logical_cpus else {
        return false;
    };
    let Some(total_mem) = host.total_memory_bytes else {
        return false;
    };
    let free_cpus = cpus as i64 - commit.vcpus;
    let free_mem = total_mem - commit.memory_bytes;
    free_cpus >= spec.cpu.boot_vcpus as i64 && free_mem >= spec.memory.size_bytes() as i64
}

/// Score a host: prefer the largest *uncommitted* memory fraction (section 17).
fn score(host: &Host, commit: HostCommit) -> f64 {
    match host.total_memory_bytes {
        Some(total) if total > 0 => (total - commit.memory_bytes) as f64 / total as f64,
        _ => 0.0,
    }
}

// Re-export the row the scheduler reads, to keep call sites terse.
pub use crate::store::Host;

#[cfg(test)]
mod tests {
    use ch_model::{BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec};
    use chrono::Utc;

    use super::*;

    fn host(name: &str, cpus: i32, total_gib: i64) -> Host {
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
            cpu_vendor: None,
            cpu_features: Vec::new(),
            total_memory_bytes: Some(total_gib * 1024 * 1024 * 1024),
            available_memory_bytes: Some(total_gib * 1024 * 1024 * 1024),
            vm_count: 0,
            last_heartbeat: None,
            created_at: now,
            updated_at: now,
            generation: 1,
        }
    }

    fn gib(n: i64) -> i64 {
        n * 1024 * 1024 * 1024
    }

    fn spec(vcpus: u32, mem_mib: u64) -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: vcpus,
                max_vcpus: vcpus,
            },
            memory: MemorySpec {
                size_mib: mem_mib,
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

    #[test]
    fn empty_hosts_get_equal_score_then_commit_spreads() {
        let a = host("a", 8, 16);
        let b = host("b", 8, 16);
        let hosts = [a.clone(), b.clone()];

        // With nothing committed, the first host wins the tie deterministically.
        let mut committed = HashMap::new();
        let first = schedule(&spec(2, 2048), &hosts, &committed).unwrap();

        // Commit that VM to the chosen host; the next VM must go to the other.
        committed.insert(
            first,
            HostCommit {
                vcpus: 2,
                memory_bytes: gib(2),
            },
        );
        let second = schedule(&spec(2, 2048), &hosts, &committed).unwrap();
        assert_ne!(
            first, second,
            "second VM spreads to the less-committed host"
        );
    }

    #[test]
    fn filters_hosts_without_enough_uncommitted_memory() {
        let h = host("h", 16, 8);
        let mut committed = HashMap::new();
        committed.insert(
            h.id,
            HostCommit {
                vcpus: 0,
                memory_bytes: gib(7),
            },
        );
        // 7 GiB committed of 8 -> only 1 GiB free; a 4 GiB VM cannot fit.
        assert!(schedule(&spec(1, 4096), &[h], &committed).is_none());
    }

    #[test]
    fn filters_on_cpu_commitment() {
        let h = host("h", 4, 64);
        let mut committed = HashMap::new();
        committed.insert(
            h.id,
            HostCommit {
                vcpus: 3,
                memory_bytes: 0,
            },
        );
        assert!(schedule(&spec(2, 1024), &[h], &committed).is_none()); // only 1 vCPU free
    }

    #[test]
    fn prefers_less_committed_host() {
        let a = host("a", 32, 64);
        let b = host("b", 32, 64);
        let hosts = [a.clone(), b.clone()];
        let mut committed = HashMap::new();
        committed.insert(
            a.id,
            HostCommit {
                vcpus: 4,
                memory_bytes: gib(48),
            },
        );
        // b is emptier -> chosen.
        assert_eq!(schedule(&spec(2, 2048), &hosts, &committed).unwrap(), b.id);
    }

    #[test]
    fn no_hosts_yields_none() {
        assert!(schedule(&spec(1, 512), &[], &HashMap::new()).is_none());
    }
}
