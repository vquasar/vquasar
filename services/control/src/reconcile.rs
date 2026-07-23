//! Reconciliation controllers (design document, sections 22 and 32).
//!
//! A periodic loop repairs the gap between desired and observed state, so
//! correctness never depends on catching a single event. Two passes run each
//! tick: a host pass (poll agents, refresh inventory/availability) and a VM
//! pass (schedule, ensure on the agent, record observed state).

use std::time::Duration;

use ch_proto::agent::vm_observed_state::Phase;
use ch_proto::agent::NetworkBinding;
use tokio::time::sleep;
use tracing::{debug, warn};

use std::collections::HashMap;

use uuid::Uuid;

use crate::agent::Agent;
use crate::netalloc::allocate_mac;
use crate::scheduler::{schedule, HostCommit};
use crate::store::{HostInventory, Store, Vm};

/// Run the reconcile loop forever.
pub async fn run(store: Store, interval: Duration) {
    loop {
        if let Err(e) = reconcile_hosts(&store).await {
            warn!(error = %e, "host reconcile pass failed");
        }
        if let Err(e) = reconcile_vms(&store).await {
            warn!(error = %e, "vm reconcile pass failed");
        }
        sleep(interval).await;
    }
}

/// Poll every host's agent and refresh its availability + inventory.
pub async fn reconcile_hosts(store: &Store) -> anyhow::Result<()> {
    for host in store.list_hosts().await? {
        let agent = Agent::new(host.endpoint.clone());
        match agent.get_host_info().await {
            Ok(info) => {
                let inv = HostInventory {
                    hostname: none_if_empty(info.hostname),
                    architecture: none_if_empty(info.architecture),
                    kernel_version: none_if_empty(info.kernel_version),
                    cloud_hypervisor_version: none_if_empty(info.cloud_hypervisor_version),
                    logical_cpus: Some(info.logical_cpus as i32),
                    cpu_model: none_if_empty(info.cpu_model),
                    total_memory_bytes: Some(info.total_memory_bytes as i64),
                    available_memory_bytes: Some(info.available_memory_bytes as i64),
                    vm_count: info.vm_count as i32,
                };
                store.update_host_ready(host.id, &inv).await?;
                if host.state != "Ready" {
                    store
                        .insert_event("host", Some(host.id), "host.ready", "info", &host.name)
                        .await?;
                }
            }
            Err(e) => {
                if host.state == "Ready" {
                    store
                        .insert_event(
                            "host",
                            Some(host.id),
                            "host.unreachable",
                            "warning",
                            &e.to_string(),
                        )
                        .await?;
                }
                // Note: VMs are NOT relocated on unreachability (ADR-014).
                store.mark_host_not_ready(host.id).await?;
            }
        }
    }
    Ok(())
}

/// Reconcile every VM whose observed state trails its desired state.
pub async fn reconcile_vms(store: &Store) -> anyhow::Result<()> {
    for vm in store.list_vms_to_reconcile().await? {
        let result = if vm.phase == "Deleting" {
            reconcile_delete(store, &vm).await
        } else {
            reconcile_ensure(store, &vm).await
        };
        if let Err(e) = result {
            warn!(vm = %vm.id, error = %e, "vm reconcile failed; will retry");
        }
    }
    Ok(())
}

async fn reconcile_ensure(store: &Store, vm: &Vm) -> anyhow::Result<()> {
    // 1. Ensure the VM is scheduled onto a host.
    let host_id = match vm.host_id {
        Some(h) => h,
        None => {
            let hosts = store.list_schedulable_hosts().await?;
            let committed = committed_by_host(store).await?;
            match schedule(&vm.spec, &hosts, &committed) {
                Some(h) => {
                    store.assign_vm_host(vm.id, h).await?;
                    store
                        .insert_event("vm", Some(vm.id), "vm.scheduled", "info", &vm.name)
                        .await?;
                    h
                }
                None => {
                    // No capacity right now; keep the task open and retry.
                    if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                        store
                            .update_task(
                                task.id,
                                "Running",
                                10,
                                Some("waiting for a schedulable host"),
                            )
                            .await?;
                    }
                    debug!(vm = %vm.id, "no schedulable host; deferring");
                    return Ok(());
                }
            }
        }
    };

    // 2. Resolve the host endpoint (skip if it is not currently Ready).
    let Some(host) = store.get_host(host_id).await? else {
        return Ok(());
    };
    if host.state != "Ready" {
        debug!(vm = %vm.id, host = %host.id, "assigned host not ready; deferring");
        return Ok(());
    }

    // 3. Resolve per-NIC dataplane bindings (MACs + VLANs).
    let bindings = match resolve_bindings(store, vm).await? {
        Some(b) => b,
        None => {
            // A referenced network does not exist yet; defer.
            if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                store
                    .update_task(
                        task.id,
                        "Running",
                        20,
                        Some("waiting for referenced network"),
                    )
                    .await?;
            }
            return Ok(());
        }
    };

    // 4. Drive the agent to the desired state.
    let agent = Agent::new(host.endpoint.clone());
    let spec_json = serde_json::to_vec(&*vm.spec)?;
    match agent
        .ensure_vm(vm.id.to_string(), vm.name.clone(), spec_json, bindings)
        .await
    {
        Ok(state) => {
            let phase = phase_string(state.phase);
            let msg = none_if_empty(state.message);
            store
                .update_vm_observed(vm.id, phase, vm.generation, msg.as_deref())
                .await?;
            if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                store.update_task(task.id, "Succeeded", 100, None).await?;
            }
            store
                .insert_event("vm", Some(vm.id), event_for_phase(phase), "info", &vm.name)
                .await?;
        }
        Err(e) => {
            // Transient: leave the phase for the next tick to retry.
            if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                store
                    .update_task(task.id, "Running", 50, Some(&e.to_string()))
                    .await?;
            }
            warn!(vm = %vm.id, error = %e, "ensure_vm failed on agent");
        }
    }
    Ok(())
}

async fn reconcile_delete(store: &Store, vm: &Vm) -> anyhow::Result<()> {
    if let Some(host_id) = vm.host_id {
        if let Some(host) = store.get_host(host_id).await? {
            let agent = Agent::new(host.endpoint);
            // Best-effort: proceed with removal even if the agent is unreachable.
            if let Err(e) = agent.delete_vm(vm.id.to_string()).await {
                warn!(vm = %vm.id, error = %e, "agent delete_vm failed; removing record anyway");
            }
        }
    }
    store.delete_vm_row(vm.id).await?;
    if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
        store.update_task(task.id, "Succeeded", 100, None).await?;
    }
    store
        .insert_event("vm", Some(vm.id), "vm.deleted", "info", &vm.name)
        .await?;
    Ok(())
}

/// Sum the CPU + memory already committed to VMs on each host, so the scheduler
/// can spread new VMs by remaining logical capacity (section 17).
async fn committed_by_host(store: &Store) -> anyhow::Result<HashMap<Uuid, HostCommit>> {
    let mut committed: HashMap<Uuid, HostCommit> = HashMap::new();
    for vm in store.list_vms().await? {
        // A VM being torn down no longer holds its host's capacity.
        if vm.phase == "Deleting" {
            continue;
        }
        if let Some(host_id) = vm.host_id {
            let entry = committed.entry(host_id).or_default();
            entry.vcpus += vm.spec.cpu.boot_vcpus as i64;
            entry.memory_bytes += vm.spec.memory.size_bytes() as i64;
        }
    }
    Ok(committed)
}

/// Resolve a VM's NICs into agent dataplane bindings (MAC + VLAN), allocating
/// MACs deterministically. Returns `None` if a referenced network is missing.
async fn resolve_bindings(store: &Store, vm: &Vm) -> anyhow::Result<Option<Vec<NetworkBinding>>> {
    let mut bindings = Vec::with_capacity(vm.spec.network_interfaces.len());
    for (index, nic) in vm.spec.network_interfaces.iter().enumerate() {
        let Some(network) = store.get_network(nic.network_id.as_uuid()).await? else {
            return Ok(None);
        };
        let mac = nic
            .mac
            .clone()
            .unwrap_or_else(|| allocate_mac(vm.id, index));
        bindings.push(NetworkBinding {
            mac,
            vlan: network.vlan.unwrap_or(0) as u32,
        });
    }
    Ok(Some(bindings))
}

fn phase_string(proto_phase: i32) -> &'static str {
    match Phase::try_from(proto_phase).unwrap_or(Phase::Unspecified) {
        Phase::Running => "Running",
        Phase::Stopped => "Stopped",
        Phase::Failed => "Failed",
        Phase::Starting => "Starting",
        Phase::Stopping => "Stopping",
        Phase::Creating => "Creating",
        Phase::Deleting => "Deleting",
        Phase::Pending | Phase::Unspecified => "Pending",
    }
}

fn event_for_phase(phase: &str) -> &'static str {
    match phase {
        "Running" => "vm.started",
        "Stopped" => "vm.stopped",
        "Failed" => "vm.failed",
        _ => "vm.updated",
    }
}

fn none_if_empty(s: String) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}
