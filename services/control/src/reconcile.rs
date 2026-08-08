//! Reconciliation controllers (design document, sections 22 and 32).
//!
//! A periodic loop repairs the gap between desired and observed state, so
//! correctness never depends on catching a single event. Two passes run each
//! tick: a host pass (poll agents, refresh inventory/availability) and a VM
//! pass (schedule, ensure on the agent, record observed state).

use std::time::Duration;

use chrono::Utc;

use tokio::time::sleep;
use tracing::{debug, warn};
use vquasar_proto::agent::vm_observed_state::Phase;
use vquasar_proto::agent::NetworkBinding;

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

use crate::agent::Agent;
use std::sync::Arc;

use crate::netalloc::allocate_mac;
use crate::scheduler::{schedule, HostCommit};
use crate::store::{HostInventory, Store, Vm};

/// Run the reconcile loop forever, acting only while this instance holds the
/// controller lease (ADR-021).
///
/// A standby keeps ticking rather than parking: the tick is where it notices it
/// has become the leader, and a loop that has to be started on promotion is a
/// loop that can fail to start.
pub async fn run(store: Store, interval: Duration, lease: Arc<crate::lease::Lease>) {
    loop {
        if !lease.is_fresh() {
            metrics::gauge!("vquasar_controller_is_leader").set(0.0);
            sleep(interval).await;
            continue;
        }
        metrics::gauge!("vquasar_controller_is_leader").set(1.0);
        metrics::counter!("vquasar_reconcile_passes_total").increment(1);
        if let Err(e) = reconcile_hosts(&store).await {
            warn!(error = %e, "host reconcile pass failed");
            metrics::counter!("vquasar_reconcile_errors_total", "pass" => "hosts").increment(1);
        }
        if let Err(e) = reconcile_migrations(&store, &lease).await {
            warn!(error = %e, "migration reconcile pass failed");
            metrics::counter!("vquasar_reconcile_errors_total", "pass" => "migrations")
                .increment(1);
        }
        if let Err(e) = reconcile_vms(&store).await {
            warn!(error = %e, "vm reconcile pass failed");
            metrics::counter!("vquasar_reconcile_errors_total", "pass" => "vms").increment(1);
        }
        if let Err(e) = recover_running_vms(&store).await {
            warn!(error = %e, "vm recovery pass failed");
            metrics::counter!("vquasar_reconcile_errors_total", "pass" => "recovery").increment(1);
        }
        if let Err(e) = refresh_vm_ips(&store).await {
            warn!(error = %e, "vm ip refresh pass failed");
            metrics::counter!("vquasar_reconcile_errors_total", "pass" => "ip_refresh")
                .increment(1);
        }
        // Free segments whose quarantine has elapsed (ADR-016).
        match crate::segments::sweep_quarantine(store.pool(), &store.network_policy().segments())
            .await
        {
            Ok(n) if n > 0 => tracing::info!(freed = n, "released quarantined network segments"),
            Ok(_) => {
                // A segment that never clears is a host that still carries the
                // bridge — say which, rather than leaving a VNI mysteriously
                // unavailable.
                if let Ok(held) = crate::segments::held_by_hosts(store.pool()).await {
                    for (segment, host) in held {
                        tracing::debug!(
                            %segment, %host,
                            "segment stays quarantined: host still reports the overlay bridge"
                        );
                    }
                }
            }
            Err(e) => warn!(error = %e, "segment quarantine sweep failed"),
        }
        // Refresh inventory gauges from the current DB state (design M17).
        if let Err(e) = crate::metrics::update_from_store(&store).await {
            warn!(error = %e, "metrics refresh failed");
        }
        sleep(interval).await;
    }
}

/// Refresh each VM's agentlessly-discovered IP from its host, independent of the
/// reconcile generation so settled (Running) VMs still get an up-to-date address
/// (design M11).
#[tracing::instrument(skip_all)]
pub async fn refresh_vm_ips(store: &Store) -> anyhow::Result<()> {
    for host in store.list_hosts().await? {
        if host.state != "Ready" {
            continue;
        }
        let Ok(vms) = Agent::new(host.endpoint.clone()).list_vms().await else {
            continue; // transient; try again next tick
        };
        for st in vms {
            if st.ip_address.is_empty() {
                continue;
            }
            if let Ok(id) = uuid::Uuid::parse_str(&st.vm_id) {
                let _ = store.set_vm_ip(id, &st.ip_address).await;
            }
        }
    }
    Ok(())
}

/// Bring back `Running` VMs whose backing process died with their host — the
/// host-reboot recovery path (design M16). For each Ready host we compare the
/// VMs the control plane believes are Running there against what the agent
/// actually reports live; any that are missing or not live are re-driven through
/// reconcile (its idempotent storage/network rebuild + relaunch recreates them
/// from the persisted spec). We act only once the host is Ready again (agent
/// reachable) — ADR-014 still forbids relocating VMs while a host is NotReady.
#[tracing::instrument(skip_all)]
pub async fn recover_running_vms(store: &Store) -> anyhow::Result<()> {
    let all_vms = store.list_vms().await?;
    for host in store.list_hosts().await? {
        if host.state != "Ready" {
            continue;
        }
        // Authoritative because the agent runs recovery before it serves gRPC;
        // an unreachable agent means the host isn't really Ready — skip it.
        let Ok(observed) = Agent::new(host.endpoint.clone()).list_vms().await else {
            continue;
        };
        let live: HashSet<Uuid> = observed
            .iter()
            .filter(|st| is_live_phase(st.phase))
            .filter_map(|st| Uuid::parse_str(&st.vm_id).ok())
            .collect();

        for vm in all_vms.iter().filter(|v| v.host_id == Some(host.id)) {
            // Only settled VMs we intend to keep Running.
            if vm.phase != "Running"
                || !matches!(
                    vm.spec.desired_power_state,
                    vquasar_model::DesiredPowerState::Running
                )
            {
                continue;
            }
            if !live.contains(&vm.id) {
                // Re-drive: a non-settled phase puts it back on the reconcile
                // work-list, which recreates + boots it on its (Ready) host.
                store.set_vm_phase(vm.id, "Scheduling").await?;
                store
                    .insert_event(
                        "vm",
                        Some(vm.id),
                        "vm.recovering",
                        "warning",
                        &format!(
                            "not running on {} — relaunching after host recovery",
                            host.name
                        ),
                    )
                    .await?;
                metrics::counter!("vquasar_vm_recoveries_total").increment(1);
                warn!(vm = %vm.id, host = %host.name, "VM down on its host; re-launching");
            }
        }
    }
    Ok(())
}

/// Whether an agent-observed phase means the VM is actually up (so it doesn't
/// need recovery).
fn is_live_phase(proto_phase: i32) -> bool {
    matches!(
        phase_string(proto_phase),
        "Running" | "Starting" | "Creating" | "Stopping"
    )
}

/// Poll every host's agent and refresh its availability + inventory.
#[tracing::instrument(skip_all)]
pub async fn reconcile_hosts(store: &Store) -> anyhow::Result<()> {
    for host in store.list_hosts().await? {
        let agent = Agent::new(host.endpoint.clone());
        match agent.get_host_info().await {
            Ok(info) => {
                let inv = HostInventory {
                    overlay_vnis: info.overlay_vnis.iter().map(|v| *v as i32).collect(),
                    hostname: none_if_empty(info.hostname),
                    architecture: none_if_empty(info.architecture),
                    kernel_version: none_if_empty(info.kernel_version),
                    cloud_hypervisor_version: none_if_empty(info.cloud_hypervisor_version),
                    logical_cpus: Some(info.logical_cpus as i32),
                    cpu_model: none_if_empty(info.cpu_model),
                    cpu_vendor: none_if_empty(info.cpu_vendor),
                    cpu_features: info.cpu_features,
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
#[tracing::instrument(skip_all)]
pub async fn reconcile_vms(store: &Store) -> anyhow::Result<()> {
    for vm in store.list_vms_to_reconcile().await? {
        // Migrating VMs are driven by the migration controller, not here.
        if vm.phase == "Migrating" {
            continue;
        }
        // A VM that is failing gets progressively more room between attempts.
        if !due(&vm) {
            continue;
        }
        let result = if vm.phase == "Deleting" {
            reconcile_delete(store, &vm).await
        } else {
            reconcile_ensure(store, &vm).await
        };
        match result {
            Ok(()) => {
                if vm.reconcile_failures > 0 {
                    if let Err(e) = store.clear_reconcile_failures(vm.id).await {
                        warn!(vm = %vm.id, error = %e, "could not clear reconcile failures");
                    }
                }
            }
            Err(e) => note_reconcile_failure(store, &vm, &e).await,
        }
    }
    Ok(())
}

/// How many consecutive failures before a VM is called `Failed`.
///
/// Retrying is right for a transient agent hiccup and wrong for a create whose
/// residue makes every attempt fail identically (#35). This is the line between
/// them, and with the backoff below it puts the whole budget at roughly half a
/// minute: long enough to ride out an agent restart, short enough that an
/// operator sees the problem rather than never.
const MAX_RECONCILE_FAILURES: i32 = 5;

/// Back off between attempts once a VM has started failing, so a permanently
/// broken one does not hammer its agent every tick.
fn backoff(failures: i32) -> Duration {
    // 2s, 4s, 8s, then a 10s ceiling — ~30s of retries before giving up.
    let secs = 2u64.saturating_mul(1 << failures.clamp(0, 3) as u64);
    Duration::from_secs(secs.min(10))
}

#[cfg(test)]
mod backoff_tests {
    use super::*;

    /// The budget has to fit inside the time an operator would wait before
    /// concluding the platform is broken — and inside the e2e wait window,
    /// which is the same question asked mechanically.
    #[test]
    fn giving_up_takes_about_half_a_minute() {
        let total: u64 = (1..MAX_RECONCILE_FAILURES)
            .map(|n| backoff(n).as_secs())
            .sum();
        assert!(
            (20..=40).contains(&total),
            "retry budget drifted to {total}s; the comment above promises ~30s"
        );
    }

    #[test]
    fn backoff_is_capped() {
        assert_eq!(backoff(99), Duration::from_secs(10));
    }
}

/// Whether enough time has passed to try this VM again.
fn due(vm: &Vm) -> bool {
    if vm.reconcile_failures == 0 {
        return true;
    }
    match vm.last_reconcile_at {
        None => true,
        Some(last) => {
            Utc::now()
                .signed_duration_since(last)
                .to_std()
                .unwrap_or_default()
                >= backoff(vm.reconcile_failures)
        }
    }
}

/// Count a failure, and stop pretending a VM is on its way once it plainly is
/// not: at the limit it becomes `Failed` and carries the agent's own error, so
/// the API says what happened instead of showing `Scheduling` for ever (#35).
async fn note_reconcile_failure(store: &Store, vm: &Vm, e: &anyhow::Error) {
    let msg = format!("{e:#}");
    let failures = match store.record_reconcile_failure(vm.id, &msg).await {
        Ok(n) => n,
        Err(db) => {
            warn!(vm = %vm.id, error = %db, "could not record a reconcile failure");
            return;
        }
    };
    if failures < MAX_RECONCILE_FAILURES {
        warn!(vm = %vm.id, error = %msg, attempt = failures, "vm reconcile failed; will retry");
        return;
    }
    warn!(vm = %vm.id, error = %msg, attempts = failures, "vm reconcile giving up; marking Failed");
    let summary = format!("reconcile failed {failures} times: {msg}");
    if let Err(db) = store.fail_vm(vm.id, &summary).await {
        warn!(vm = %vm.id, error = %db, "could not mark the vm Failed");
        return;
    }
    let _ = store
        .insert_event("vm", Some(vm.id), "vm.reconcile_failed", "error", &summary)
        .await;
}

/// Advance each in-flight live migration by one step (design section 28). The
/// migration is a persisted state machine, so it resumes after a control-plane
/// restart. One step runs per tick; failures move it to `Failed` and leave the
/// VM on its source host.
#[tracing::instrument(skip_all)]
pub async fn reconcile_migrations(
    store: &Store,
    lease: &crate::lease::Lease,
) -> anyhow::Result<()> {
    for m in store.list_active_migrations().await? {
        // The one place a duplicate is not idempotent: two `prepare_receive`
        // calls mean two receivers for one guest. Every other pass converges
        // however many times it runs, so this is the only pass that pays for a
        // round trip to re-confirm the lease immediately before acting
        // (ADR-021).
        if !lease.confirm().await {
            return Ok(());
        }
        if let Err(e) = advance_migration(store, &m).await {
            warn!(migration = %m.id, vm = %m.vm_id, error = %e, "migration failed");
            metrics::counter!("vquasar_migrations_total", "result" => "failed").increment(1);
            store
                .update_migration(m.id, "Failed", None, Some(&e.to_string()))
                .await?;
            // The VM never left its source host; return it to Running.
            store.set_vm_phase(m.vm_id, "Running").await?;
            if let Some(task_id) = m.task_id {
                store
                    .update_task(task_id, "Failed", 100, Some(&e.to_string()))
                    .await?;
            }
            store
                .insert_event(
                    "vm",
                    Some(m.vm_id),
                    "migration.failed",
                    "warning",
                    &e.to_string(),
                )
                .await?;
        }
    }
    Ok(())
}

async fn advance_migration(store: &Store, m: &crate::store::Migration) -> anyhow::Result<()> {
    let vm = store
        .get_vm(m.vm_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("vm gone"))?;
    let target = store
        .get_host(m.target_host_id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("target host gone"))?;

    match m.state.as_str() {
        "Pending" => {
            // Destination: launch a receiver and learn the migration URL.
            let agent = Agent::new(target.endpoint.clone());
            let spec_json = serde_json::to_vec(&*vm.spec)?;
            let url = with_timeout(
                60,
                "prepare_receive",
                agent.prepare_receive(vm.id.to_string(), vm.name.clone(), spec_json),
            )
            .await?;
            store
                .update_migration(m.id, "Sending", Some(&url), None)
                .await?;
            store
                .insert_event("vm", Some(vm.id), "migration.started", "info", &vm.name)
                .await?;
        }
        "Sending" => {
            // Source: send the live state to the destination.
            let source_id = m
                .source_host_id
                .ok_or_else(|| anyhow::anyhow!("migration has no source host"))?;
            let source = store
                .get_host(source_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("source host gone"))?;
            let url = m
                .migration_url
                .clone()
                .ok_or_else(|| anyhow::anyhow!("no migration url"))?;
            with_timeout(
                300,
                "send_migration",
                Agent::new(source.endpoint).send_migration(vm.id.to_string(), url),
            )
            .await?;
            store
                .update_migration(m.id, "Finalizing", None, None)
                .await?;
        }
        "Finalizing" => {
            // Destination: adopt the running VM. Source: discard the husk.
            with_timeout(
                120,
                "finalize_receive",
                Agent::new(target.endpoint.clone()).finalize_receive(vm.id.to_string()),
            )
            .await?;
            if let Some(source_id) = m.source_host_id {
                if let Some(source) = store.get_host(source_id).await? {
                    let discard = with_timeout(
                        60,
                        "discard_vm",
                        Agent::new(source.endpoint).discard_vm(vm.id.to_string()),
                    )
                    .await;
                    if let Err(e) = discard {
                        warn!(vm = %vm.id, error = %e, "source discard failed (continuing)");
                    }
                }
            }
            store.set_vm_host_running(vm.id, m.target_host_id).await?;
            metrics::counter!("vquasar_migrations_total", "result" => "completed").increment(1);
            store
                .update_migration(m.id, "Completed", None, None)
                .await?;
            if let Some(task_id) = m.task_id {
                store.update_task(task_id, "Succeeded", 100, None).await?;
            }
            store
                .insert_event(
                    "vm",
                    Some(vm.id),
                    "migration.completed",
                    "info",
                    &target.name,
                )
                .await?;
        }
        other => {
            debug!(migration = %m.id, state = other, "unexpected migration state");
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
    let bindings = match resolve_bindings(store, vm, host_id).await? {
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
    let network_config = build_network_config(store, vm).await?;
    // The guest cannot hold an operator credential, so it gets a secret of its
    // own to present back on the phone_home callback (design M13e).
    let phone_home_token = store
        .ensure_phone_home_token(vm.id)
        .await
        .unwrap_or_default();
    match agent
        .ensure_vm(
            vm.id.to_string(),
            vm.name.clone(),
            spec_json,
            bindings,
            network_config,
            phone_home_token,
        )
        .await
    {
        Ok(state) => {
            let phase = phase_string(state.phase);
            let msg = none_if_empty(state.message);
            // For managed (static IPAM) NICs the control plane already knows the
            // address, so prefer the authoritative allocation over the agent's
            // best-effort ARP discovery (design M13a); fall back to it otherwise.
            let ip = match store
                .allocations_for_vm(vm.id)
                .await?
                .into_iter()
                .find(|a| a.family == 4)
            {
                Some(a) => Some(a.ip),
                None => none_if_empty(state.ip_address),
            };
            store
                .update_vm_observed(vm.id, phase, vm.generation, msg.as_deref(), ip.as_deref())
                .await?;
            if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                store.update_task(task.id, "Succeeded", 100, None).await?;
            }
            store
                .insert_event("vm", Some(vm.id), event_for_phase(phase), "info", &vm.name)
                .await?;
        }
        Err(e) => {
            if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                store
                    .update_task(task.id, "Running", 50, Some(&e.to_string()))
                    .await?;
            }
            // Propagate rather than swallow. This used to log and return Ok,
            // which made every agent failure look transient to the caller: the
            // tick retried immediately, for ever, and nothing counted the
            // attempts or ever gave up (#35).
            return Err(anyhow::Error::new(e).context("ensure_vm failed on agent"));
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

/// Bound an agent RPC so a stalled migration step fails instead of hanging the
/// reconcile loop forever.
async fn with_timeout<F, T>(secs: u64, what: &str, fut: F) -> anyhow::Result<T>
where
    F: std::future::Future<Output = Result<T, crate::agent::AgentError>>,
{
    match tokio::time::timeout(Duration::from_secs(secs), fut).await {
        Ok(r) => Ok(r?),
        Err(_) => Err(anyhow::anyhow!("{what} timed out after {secs}s")),
    }
}

/// Sum the CPU + memory already committed to VMs on each host, so the scheduler
/// can spread new VMs by remaining logical capacity (section 17).
pub(crate) async fn committed_by_host(store: &Store) -> anyhow::Result<HashMap<Uuid, HostCommit>> {
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

/// Build the cloud-init netplan v2 `network-config` for a VM from its persisted
/// IP allocations (design M13a). Returns an empty string when no NIC is on a
/// managed (static IPAM) network, so DHCP-only VMs get no network-config at all.
async fn build_network_config(store: &Store, vm: &Vm) -> anyhow::Result<String> {
    let allocs = store.allocations_for_vm(vm.id).await?;
    let mut nics = Vec::with_capacity(vm.spec.network_interfaces.len());
    let mut any_managed = false;

    for (index, nic) in vm.spec.network_interfaces.iter().enumerate() {
        let Some(network) = store.get_network(nic.network_id.as_uuid()).await? else {
            continue;
        };
        let mac = nic
            .mac
            .clone()
            .unwrap_or_else(|| allocate_mac(vm.id, index));
        let v4 = crate::ipam::Subnet::parse_opt(
            network.cidr_v4.as_deref(),
            network.gateway_v4.as_deref(),
            None,
            None,
        )
        .ok()
        .flatten();
        let v6 = crate::ipam::Subnet::parse_opt(
            network.cidr_v6.as_deref(),
            network.gateway_v6.as_deref(),
            None,
            None,
        )
        .ok()
        .flatten();

        // Attach this NIC's allocated addresses with the right prefix length.
        let mut addresses = Vec::new();
        for a in allocs.iter().filter(|a| a.nic_index as usize == index) {
            let prefix = match a.family {
                6 => v6.as_ref().map(|s| s.prefix_len()),
                _ => v4.as_ref().map(|s| s.prefix_len()),
            };
            if let Some(p) = prefix {
                addresses.push(format!("{}/{}", a.ip, p));
            }
        }

        if network.is_managed() {
            any_managed = true;
        }
        nics.push(crate::ipam::NicRender {
            set_name: format!("eth{index}"),
            mac,
            addresses,
            gateway4: network.gateway_v4.clone(),
            gateway6: network.gateway_v6.clone(),
            dns: network.dns.clone(),
            // Overlay NICs shrink the MTU to absorb VXLAN encap (design M13b).
            mtu: network
                .is_overlay()
                .then(|| store.network_policy().overlay_guest_mtu()),
        });
    }

    if !any_managed {
        return Ok(String::new());
    }
    Ok(crate::ipam::render_network_config(&nics))
}

/// Resolve a host's agent endpoint to an underlay **IP** for VXLAN `remote_ip`
/// (design M13b). OVS rejects hostnames, so a name is resolved via DNS
/// (preferring IPv4); an endpoint that is already an IP is returned as-is.
async fn resolve_underlay(endpoint: &str) -> Option<String> {
    let host = underlay_ip(endpoint)?;
    if host.parse::<std::net::IpAddr>().is_ok() {
        return Some(host);
    }
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
        .await
        .ok()?
        .collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .map(|a| a.ip().to_string())
}

/// Extract a host's underlay IP/host from its agent endpoint: strip any scheme
/// and the port. e.g. `https://172.16.56.81:9500` → `172.16.56.81`.
fn underlay_ip(endpoint: &str) -> Option<String> {
    let host = endpoint
        .rsplit_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(endpoint);
    // Strip a trailing :port. IPv6 literals would be bracketed ([::1]:9500);
    // handle the common host:port case and leave bare hosts untouched.
    let host = host.split('/').next().unwrap_or(host);
    let trimmed = match host.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) && !h.is_empty() => h,
        _ => host,
    };
    let trimmed = trimmed.trim_matches(['[', ']']);
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Resolve a VM's NICs into agent dataplane bindings (MAC + VLAN/VNI), allocating
/// MACs deterministically. Returns `None` if a referenced network is missing.
async fn resolve_bindings(
    store: &Store,
    vm: &Vm,
    this_host: Uuid,
) -> anyhow::Result<Option<Vec<NetworkBinding>>> {
    let mut bindings = Vec::with_capacity(vm.spec.network_interfaces.len());

    // Underlay IPs of the *other* hosts, for any VXLAN tunnel mesh (design M13b).
    // Exclude this VM's assigned host (passed in, since `vm.host_id` may still be
    // stale on the tick it was first scheduled) so no host tunnels to itself.
    let mut overlay_peers: Vec<String> = Vec::new();
    let mut overlay_identities: Vec<vquasar_proto::agent::OverlayPeer> = Vec::new();
    for h in store.list_hosts().await? {
        if h.id == this_host {
            continue;
        }
        if let Some(ip) = resolve_underlay(&h.endpoint).await {
            overlay_peers.push(ip.clone());
            // The CN pins who the agent accepts on the other end of a tunnel
            // (M18b). Unknown for a host enrolled before it was recorded — the
            // agent then falls back to CA-only trust and says so.
            overlay_identities.push(vquasar_proto::agent::OverlayPeer {
                underlay_ip: ip,
                cert_cn: h.cert_cn.clone().unwrap_or_default(),
            });
        }
    }
    let encrypt_underlay = store.network_policy().overlay_encryption.is_encrypted();

    for (index, nic) in vm.spec.network_interfaces.iter().enumerate() {
        let Some(network) = store.get_network(nic.network_id.as_uuid()).await? else {
            return Ok(None);
        };
        let mac = nic
            .mac
            .clone()
            .unwrap_or_else(|| allocate_mac(vm.id, index));
        let (vni, peers, identities) = match network.vni {
            Some(v) => (v as u32, overlay_peers.clone(), overlay_identities.clone()),
            None => (0, Vec::new(), Vec::new()),
        };

        // Effective policy is the network's default group unioned with the NIC's
        // own (ADR-017). An empty NIC set therefore means "the network's
        // default applies" — never "unfiltered". A network with no default is
        // one created before migration 0017; it keeps the old opt-in behaviour
        // so an upgrade changes nothing.
        let mut groups = nic.security_groups.clone();
        if store.network_policy().policy_enforced() {
            if let Some(default) = network.default_security_group_id {
                if !groups.contains(&default) {
                    groups.push(default);
                }
            }
        }
        let (filtered, ingress_rules) = if groups.is_empty() {
            (false, Vec::new())
        } else {
            let rules = store
                .rules_for_groups(&groups)
                .await?
                .into_iter()
                .filter(|r| r.direction == "ingress")
                .map(|r| vquasar_proto::agent::SecurityRule {
                    ipv6: r.ethertype.eq_ignore_ascii_case("IPv6"),
                    protocol: r.protocol,
                    port_min: r.port_min.unwrap_or(0).max(0) as u32,
                    port_max: r.port_max.unwrap_or(0).max(0) as u32,
                    remote_cidr: r.remote_cidr.unwrap_or_default(),
                })
                .collect();
            (true, rules)
        };

        bindings.push(NetworkBinding {
            mac,
            vlan: network.vlan.unwrap_or(0) as u32,
            vni,
            overlay_peers: peers,
            overlay_peer_identities: identities,
            encrypt_underlay: encrypt_underlay && vni != 0,
            filtered,
            ingress_rules,
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

#[cfg(test)]
mod tests {
    use super::underlay_ip;

    #[test]
    fn underlay_ip_strips_scheme_and_port() {
        assert_eq!(
            underlay_ip("https://172.16.56.81:9500").as_deref(),
            Some("172.16.56.81")
        );
        assert_eq!(
            underlay_ip("http://10.0.0.5:9500").as_deref(),
            Some("10.0.0.5")
        );
        assert_eq!(
            underlay_ip("172.16.56.81:9500").as_deref(),
            Some("172.16.56.81")
        );
        assert_eq!(underlay_ip("172.16.56.81").as_deref(), Some("172.16.56.81"));
        assert_eq!(underlay_ip("chnode1.lab").as_deref(), Some("chnode1.lab"));
        assert_eq!(underlay_ip("[fd00::1]:9500").as_deref(), Some("fd00::1"));
        assert_eq!(underlay_ip("").as_deref(), None);
    }
}
