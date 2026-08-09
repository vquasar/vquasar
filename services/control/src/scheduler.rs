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

use std::collections::{HashMap, HashSet};

use uuid::Uuid;
use vquasar_model::VirtualMachineSpec;

/// Which pools each host reports it can use (ADR-023).
pub type PoolsByHost = HashMap<Uuid, HashSet<Uuid>>;

/// Why nothing could be placed. Carried rather than inferred, because "no
/// capacity" and "nobody can reach that storage" want different actions from
/// an operator, and a single "no schedulable host" hides which one it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unschedulable {
    /// Hosts exist with room, but none reports every pool this VM's disks are
    /// in. This is the refusal a missing mount used to become a launch failure.
    UnreachableStorage,
    /// Nothing had room, or there were no hosts at all.
    NoCapacity,
}

impl Unschedulable {
    /// The message an operator reads on the open task.
    pub fn reason(&self) -> &'static str {
        match self {
            Unschedulable::UnreachableStorage => {
                "no schedulable host reports the storage pool this VM's disks are in"
            }
            Unschedulable::NoCapacity => "waiting for a schedulable host",
        }
    }
}

/// The host a VM's disks pin it to, when one of them is on storage only that
/// host has (ADR-025).
///
/// Stronger than a pool constraint and checked before one: a pool says "any
/// host reporting this", while a pinned disk says "that host, because the bytes
/// are on it".
pub fn pinned_host(spec: &VirtualMachineSpec) -> Option<Uuid> {
    spec.disks
        .iter()
        .find_map(|d| d.pinned_host.map(|h| h.as_uuid()))
}

/// The pools a VM's disks need a host to be able to reach.
///
/// Only disks the control plane placed carry a pool. A disk pointed at a raw
/// path constrains nothing here: the platform does not know which pool it is
/// in, and guessing from the path would be a claim it cannot back.
pub fn required_pools(spec: &VirtualMachineSpec) -> HashSet<Uuid> {
    spec.disks
        .iter()
        .filter_map(|d| d.pool.map(|p| p.as_uuid()))
        .collect()
}

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
    pools: &PoolsByHost,
) -> Result<Uuid, Unschedulable> {
    // Storage first, and separately, so the refusal can say which filter
    // emptied the list. A host that cannot reach a VM's disks is not a host
    // that is merely busy.
    // A disk on storage only one host has decides the answer before anything
    // else is considered: no other host can see those bytes at all.
    let pinned = pinned_host(spec);
    let needed = required_pools(spec);
    let reachable: Vec<&Host> = hosts
        .iter()
        .filter(|h| pinned.is_none_or(|p| p == h.id))
        .filter(|h| can_reach(&needed, pools.get(&h.id)))
        .collect();
    if reachable.is_empty() && !hosts.is_empty() && (!needed.is_empty() || pinned.is_some()) {
        return Err(Unschedulable::UnreachableStorage);
    }
    reachable
        .into_iter()
        .filter(|h| passes_filters(spec, h, commit_of(committed, h.id)))
        .max_by(|a, b| {
            let sa = score(a, commit_of(committed, a.id));
            let sb = score(b, commit_of(committed, b.id));
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|h| h.id)
        .ok_or(Unschedulable::NoCapacity)
}

/// Whether a host reports every pool the VM needs.
fn can_reach(needed: &HashSet<Uuid>, reported: Option<&HashSet<Uuid>>) -> bool {
    if needed.is_empty() {
        return true;
    }
    reported.is_some_and(|r| needed.is_subset(r))
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
    use chrono::Utc;
    use vquasar_model::{BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec};

    use super::*;

    fn host(name: &str, cpus: i32, total_gib: i64) -> Host {
        let now = Utc::now();
        Host {
            cert_cn: None,
            overlay_vnis: vec![],
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

    fn no_pools() -> PoolsByHost {
        PoolsByHost::new()
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
            machine_type: vquasar_model::MachineType::Standard,
        }
    }

    #[test]
    fn empty_hosts_get_equal_score_then_commit_spreads() {
        let a = host("a", 8, 16);
        let b = host("b", 8, 16);
        let hosts = [a.clone(), b.clone()];

        // With nothing committed, the first host wins the tie deterministically.
        let mut committed = HashMap::new();
        let first = schedule(&spec(2, 2048), &hosts, &committed, &no_pools()).unwrap();

        // Commit that VM to the chosen host; the next VM must go to the other.
        committed.insert(
            first,
            HostCommit {
                vcpus: 2,
                memory_bytes: gib(2),
            },
        );
        let second = schedule(&spec(2, 2048), &hosts, &committed, &no_pools()).unwrap();
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
        assert!(schedule(&spec(1, 4096), &[h], &committed, &no_pools()).is_err());
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
        // only 1 vCPU free
        assert!(schedule(&spec(2, 1024), &[h], &committed, &no_pools()).is_err());
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
        assert_eq!(
            schedule(&spec(2, 2048), &hosts, &committed, &no_pools()).unwrap(),
            b.id
        );
    }

    #[test]
    fn no_hosts_yields_none() {
        assert_eq!(
            schedule(&spec(1, 512), &[], &HashMap::new(), &no_pools()),
            Err(Unschedulable::NoCapacity)
        );
    }

    /// A host that does not report the pool a VM's disks are in is refused —
    /// and the refusal says so, rather than reading as a capacity problem
    /// (ADR-023). This is the failure that used to be a launch-time path error.
    #[test]
    fn a_host_that_cannot_reach_the_storage_is_refused_by_name() {
        let h = host("h", 32, 64);
        let h_id = h.id;
        let pool = Uuid::new_v4();
        let mut s = spec(2, 2048);
        s.disks = vec![vquasar_model::DiskSpec::raw("/pool/a.raw")
            .in_pool(Some(vquasar_model::StoragePoolId::from_uuid(pool)))];

        // Plenty of room, but the host has never reported the pool.
        assert_eq!(
            schedule(&s, std::slice::from_ref(&h), &HashMap::new(), &no_pools()),
            Err(Unschedulable::UnreachableStorage)
        );

        // Reporting some other pool is not reporting this one.
        let mut wrong = PoolsByHost::new();
        wrong.insert(h.id, HashSet::from([Uuid::new_v4()]));
        assert_eq!(
            schedule(&s, std::slice::from_ref(&h), &HashMap::new(), &wrong),
            Err(Unschedulable::UnreachableStorage)
        );

        // Once it reports the pool, it is a candidate again.
        let mut right = PoolsByHost::new();
        right.insert(h.id, HashSet::from([pool]));
        assert_eq!(schedule(&s, &[h], &HashMap::new(), &right).unwrap(), h_id);
    }

    /// Every pool, not any: a VM with a disk in two pools needs a host that
    /// reports both, or it lands somewhere half its storage is invisible.
    #[test]
    fn a_host_must_report_every_pool_the_disks_need() {
        let h = host("h", 32, 64);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = spec(2, 2048);
        s.disks = vec![
            vquasar_model::DiskSpec::raw("/a/x.raw")
                .in_pool(Some(vquasar_model::StoragePoolId::from_uuid(a))),
            vquasar_model::DiskSpec::raw("/b/y.raw")
                .in_pool(Some(vquasar_model::StoragePoolId::from_uuid(b))),
        ];
        let mut half = PoolsByHost::new();
        half.insert(h.id, HashSet::from([a]));
        assert_eq!(
            schedule(&s, std::slice::from_ref(&h), &HashMap::new(), &half),
            Err(Unschedulable::UnreachableStorage)
        );
        let mut both = PoolsByHost::new();
        both.insert(h.id, HashSet::from([a, b]));
        assert!(schedule(&s, &[h], &HashMap::new(), &both).is_ok());
    }

    /// A disk at a raw operator-supplied path has no pool, so it constrains
    /// nothing. Refusing it would be inventing a constraint from a path.
    #[test]
    fn a_disk_with_no_pool_places_anywhere() {
        let h = host("h", 32, 64);
        let mut s = spec(2, 2048);
        s.disks = vec![vquasar_model::DiskSpec::raw("/x/legacy.raw")];
        assert!(schedule(&s, &[h], &HashMap::new(), &no_pools()).is_ok());
    }

    /// A disk on storage only one host has decides placement outright — not a
    /// preference, and not merely "a host reporting the pool". Every host
    /// reporting a local pool has a disk by that name; only one has this
    /// volume on it (ADR-025).
    #[test]
    fn a_pinned_disk_decides_which_host_outright() {
        let a = host("a", 32, 64);
        let b = host("b", 32, 64);
        let (a_id, b_id) = (a.id, b.id);
        let pool = Uuid::new_v4();
        let mut s = spec(2, 2048);
        let mut disk = vquasar_model::DiskSpec::raw("/nvme/vol.raw")
            .in_pool(Some(vquasar_model::StoragePoolId::from_uuid(pool)));
        disk.pinned_host = Some(vquasar_model::HostId::from_uuid(b_id));
        s.disks = vec![disk];

        // Both hosts report the pool, and b is the emptier one anyway — so this
        // has to fail for the right reason when only a is offered.
        let mut both = PoolsByHost::new();
        both.insert(a_id, HashSet::from([pool]));
        both.insert(b_id, HashSet::from([pool]));
        assert_eq!(
            schedule(&s, &[a.clone(), b.clone()], &HashMap::new(), &both).unwrap(),
            b_id
        );

        // Offered only the other host, this is not "no capacity": the bytes are
        // somewhere it cannot reach.
        assert_eq!(
            schedule(&s, std::slice::from_ref(&a), &HashMap::new(), &both),
            Err(Unschedulable::UnreachableStorage)
        );
    }

    /// A pin with no pool still pins. The two constraints are independent, and
    /// a disk can name its host without the platform knowing the pool.
    #[test]
    fn a_pin_holds_without_a_pool() {
        let a = host("a", 32, 64);
        let b = host("b", 32, 64);
        let b_id = b.id;
        let mut s = spec(2, 2048);
        let mut disk = vquasar_model::DiskSpec::raw("/nvme/vol.raw");
        disk.pinned_host = Some(vquasar_model::HostId::from_uuid(b_id));
        s.disks = vec![disk];
        assert_eq!(
            schedule(&s, &[a.clone(), b], &HashMap::new(), &no_pools()).unwrap(),
            b_id
        );
        assert_eq!(
            schedule(&s, std::slice::from_ref(&a), &HashMap::new(), &no_pools()),
            Err(Unschedulable::UnreachableStorage)
        );
    }

    /// Running out of room and being unable to see the storage are different
    /// answers, and the message an operator reads says which.
    #[test]
    fn the_two_refusals_do_not_read_the_same() {
        assert!(Unschedulable::UnreachableStorage
            .reason()
            .contains("storage pool"));
        assert_ne!(
            Unschedulable::UnreachableStorage.reason(),
            Unschedulable::NoCapacity.reason()
        );
    }
}
