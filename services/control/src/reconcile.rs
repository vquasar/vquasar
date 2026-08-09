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
    // Swept on its own cadence, not every tick: reading a shared directory is
    // not free and an orphaned file is not urgent. `None` means "not yet", so
    // the first pass after this instance takes the lease does one.
    let mut last_sweep: Option<std::time::Instant> = None;
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
        // Files whose owning row is gone (#41). Reporting by default; see
        // [storage] orphan_reclaim.
        let sweep_every = store.orphan_sweep_interval();
        if last_sweep.is_none_or(|t| t.elapsed() >= sweep_every) {
            last_sweep = Some(std::time::Instant::now());
            match crate::orphans::sweep(&store, store.orphan_policy(), store.orphan_min_age()).await
            {
                Ok(s) if s.found > 0 => tracing::info!(
                    found = s.found,
                    reclaimed = s.reclaimed,
                    bytes = s.bytes,
                    "orphaned storage sweep"
                ),
                Ok(_) => {}
                Err(e) => warn!(error = %e, "orphaned storage sweep failed"),
            }
        }
        // Refresh inventory gauges from the current DB state (design M17).
        if let Err(e) = crate::metrics::update_from_store(&store).await {
            warn!(error = %e, "metrics refresh failed");
        }
        // The pass finished. Recorded at the end rather than the start, so a
        // loop that wedges mid-pass stops updating this instead of claiming a
        // freshness it does not have.
        if let Err(e) = crate::lease::mark_pass(store.pool(), lease.identity()).await {
            warn!(error = %e, "could not record the reconcile heartbeat");
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

/// The pools every agent is asked about this tick (ADR-023).
async fn pool_probes(store: &Store) -> anyhow::Result<Vec<vquasar_proto::agent::StoragePoolProbe>> {
    Ok(store
        .list_storage_pools()
        .await?
        .into_iter()
        .map(|p| vquasar_proto::agent::StoragePoolProbe {
            pool_id: p.id.to_string(),
            name: p.name,
            kind: p.kind,
            path: p.params.0.host_path().unwrap_or_default().to_string(),
            // The whole parameter set, so a kind that needs more than a path
            // (an NFS export, a future pool name) needs no wire change.
            params: serde_json::to_string(&p.params.0).unwrap_or_default(),
        })
        .collect())
}

/// Translate an agent's reports for the store, dropping any whose pool id is
/// not a UUID — a report the control plane cannot attribute is not a report.
fn pool_reports(
    reported: &[vquasar_proto::agent::StoragePoolReport],
) -> Vec<crate::store::PoolReport> {
    reported
        .iter()
        .filter_map(|r| {
            let pool_id = r.pool_id.parse::<uuid::Uuid>().ok()?;
            Some(crate::store::PoolReport {
                pool_id,
                usable: r.usable,
                message: (!r.message.is_empty()).then(|| r.message.clone()),
                // Sizes are only meaningful for a pool the host can use;
                // recording zeroes for one it cannot would put a real-looking
                // number where there is no measurement.
                capacity_bytes: r.usable.then_some(r.capacity_bytes as i64),
                available_bytes: r.usable.then_some(r.available_bytes as i64),
            })
        })
        .collect()
}

/// Poll every host's agent and refresh its availability + inventory.
#[tracing::instrument(skip_all)]
pub async fn reconcile_hosts(store: &Store) -> anyhow::Result<()> {
    // Read once per tick, not once per host: every agent is asked about the
    // same pools, because a pool is defined in one place (ADR-023).
    let probes = pool_probes(store).await?;
    for host in store.list_hosts().await? {
        let agent = Agent::new(host.endpoint.clone());
        match agent.get_host_info(probes.clone()).await {
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
                store
                    .record_pool_reports(host.id, &pool_reports(&info.storage_pools))
                    .await?;
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
                // A host that cannot be polled is not reporting anything. Its
                // last word about a pool must not keep that pool looking
                // usable on the strength of a machine that is gone.
                store.clear_pool_reports_for_host(host.id).await?;
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
            // Tell the target to drop any receiver it prepared. `prepare_receive`
            // is idempotent by VM id (#45), so a receiver left behind by a failed
            // migration would be handed straight back to the *next* migration of
            // this VM to this host — a URL nothing is listening on. Best-effort
            // and a no-op when no receiver exists.
            release_receiver(store, &m).await;
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

/// Drop a receiver the target prepared for a migration that failed before any
/// state was transferred.
///
/// Only from `Pending`, and the limit is load-bearing. Past `Pending` the guest
/// may already be live on the target — a `Finalizing` failure can happen *after*
/// the target adopted it — and `DiscardVm` there would delete a running VM. In
/// `Pending` nothing has been sent, so the only thing the target can be holding
/// is a receiver.
///
/// Within `Pending` it is unconditional rather than "only if we think one
/// exists", because the case that motivates it is a controller that died before
/// recording what it had already done: the state machine's own account of how
/// far it got is exactly what cannot be trusted here (#45).
async fn release_receiver(store: &Store, m: &crate::store::Migration) {
    if m.state != "Pending" {
        return;
    }
    let target = match store.get_host(m.target_host_id).await {
        Ok(Some(h)) => h,
        Ok(None) => return,
        Err(e) => {
            warn!(migration = %m.id, error = %e, "could not look up the migration target");
            return;
        }
    };
    if let Err(e) = with_timeout(
        30,
        "discard_vm",
        Agent::new(target.endpoint).discard_vm(m.vm_id.to_string()),
    )
    .await
    {
        // Worth a line, not worth failing over: the migration has already
        // failed, and this is cleanup after it.
        debug!(migration = %m.id, vm = %m.vm_id, error = %e,
               "target had no prepared receiver to discard");
    }
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
            let pools = store.pools_by_host().await?;
            match schedule(&vm.spec, &hosts, &committed, &pools) {
                Ok(h) => {
                    store.assign_vm_host(vm.id, h).await?;
                    store
                        .insert_event("vm", Some(vm.id), "vm.scheduled", "info", &vm.name)
                        .await?;
                    h
                }
                Err(why) => {
                    // Nothing fits right now; keep the task open and retry, but
                    // say *why* it does not fit. A host that cannot reach the
                    // VM's storage is not a host that is merely busy, and an
                    // operator waiting for capacity that will never come is the
                    // failure this reports (ADR-023).
                    if let Some(task) = store.latest_open_task_for_vm(vm.id).await? {
                        store
                            .update_task(task.id, "Running", 10, Some(why.reason()))
                            .await?;
                    }
                    debug!(vm = %vm.id, reason = why.reason(), "not schedulable; deferring");
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
            // The network's default, and the project's. A tenant network
            // belongs to one project, so its default is already that tenant's;
            // a provider or VLAN network is platform-shared, and a rule added
            // to *its* default would apply to every tenant on it. The project
            // default is where a tenant's baseline goes instead (design §18).
            let project_default = store.project_default_group(vm.id).await?;
            for default in [network.default_security_group_id, project_default]
                .into_iter()
                .flatten()
            {
                if !groups.contains(&default) {
                    groups.push(default);
                }
            }
        }
        let egress_default_deny = store.network_policy().egress_enforced();
        let (filtered, ingress_rules, egress_rules) = if groups.is_empty() {
            (false, Vec::new(), Vec::new())
        } else {
            let all = store.rules_for_groups(&groups).await?;
            // A rule naming a group is resolved to its members' addresses here,
            // on every tick — which is what makes "the web tier may reach the
            // database tier" survive a VM being replaced. One rule becomes one
            // per address; a group with no addressable member becomes none, and
            // so matches nothing, which is the safe direction.
            let remotes: Vec<uuid::Uuid> = all.iter().filter_map(|r| r.remote_group_id).collect();
            let members = if remotes.is_empty() {
                crate::store::GroupMembers::new()
            } else {
                store.addresses_in_groups(&remotes).await?
            };
            let wire = |r: &crate::store::SecurityGroupRule| expand_rule(r, &members);
            let ingress = all
                .iter()
                .filter(|r| r.direction == "ingress")
                .flat_map(&wire)
                .collect();
            // Only when egress is enforced. Sending an allow-list the agent
            // will not act on would be inventing a guarantee out of a config
            // value that is off.
            let egress = if egress_default_deny {
                all.iter()
                    .filter(|r| r.direction == "egress")
                    .flat_map(&wire)
                    .collect()
            } else {
                Vec::new()
            };
            (true, ingress, egress)
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
            egress_rules,
            // Only a filtered NIC has a policy at all; an unfiltered one keeps
            // today's behaviour whatever the mode says.
            egress_default_deny: filtered && egress_default_deny,
        });
    }
    Ok(Some(bindings))
}

/// One stored rule as the rules that go on the wire.
///
/// A rule naming a CIDR is itself. A rule naming a *group* becomes one rule per
/// member address — which is what makes "the web tier may reach the database
/// tier" survive a VM being replaced, since this is recomputed every tick.
///
/// Two properties are load-bearing. A member address of the wrong family is
/// dropped: putting an IPv6 address into an IPv4 rule produces a match the
/// dataplane cannot make, and a rule that silently matches nothing is worse
/// than one that is not there. And a group with no addressable members expands
/// to *nothing* rather than to "any" — the safe direction, and the one an
/// operator would not think to check.
fn expand_rule(
    r: &crate::store::SecurityGroupRule,
    members: &crate::store::GroupMembers,
) -> Vec<vquasar_proto::agent::SecurityRule> {
    let base = vquasar_proto::agent::SecurityRule {
        ipv6: r.ethertype.eq_ignore_ascii_case("IPv6"),
        protocol: r.protocol.clone(),
        port_min: r.port_min.unwrap_or(0).max(0) as u32,
        port_max: r.port_max.unwrap_or(0).max(0) as u32,
        remote_cidr: r.remote_cidr.clone().unwrap_or_default(),
    };
    let Some(group) = r.remote_group_id else {
        return vec![base];
    };
    members
        .get(&group)
        .map(|ips| {
            ips.iter()
                .filter(|ip| ip.contains(':') == base.ipv6)
                .map(|ip| vquasar_proto::agent::SecurityRule {
                    remote_cidr: format!("{ip}/{}", if base.ipv6 { 128 } else { 32 }),
                    ..base.clone()
                })
                .collect()
        })
        .unwrap_or_default()
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

#[cfg(test)]
mod policy_tests {
    use super::*;
    use chrono::Utc;

    fn rule(
        ethertype: &str,
        cidr: Option<&str>,
        group: Option<uuid::Uuid>,
    ) -> crate::store::SecurityGroupRule {
        crate::store::SecurityGroupRule {
            id: uuid::Uuid::new_v4(),
            security_group_id: uuid::Uuid::new_v4(),
            direction: "ingress".into(),
            ethertype: ethertype.into(),
            protocol: "tcp".into(),
            port_min: Some(5432),
            port_max: Some(5432),
            remote_cidr: cidr.map(str::to_string),
            remote_group_id: group,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn a_cidr_rule_passes_through_unchanged() {
        let r = rule("IPv4", Some("10.0.0.0/8"), None);
        let out = expand_rule(&r, &crate::store::GroupMembers::new());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].remote_cidr, "10.0.0.0/8");
    }

    #[test]
    fn a_group_rule_becomes_one_rule_per_member_address() {
        let g = uuid::Uuid::new_v4();
        let mut members = crate::store::GroupMembers::new();
        members.insert(g, vec!["10.40.0.5".into(), "10.40.0.9".into()]);
        let out = expand_rule(&rule("IPv4", None, Some(g)), &members);
        let cidrs: Vec<_> = out.iter().map(|r| r.remote_cidr.clone()).collect();
        assert_eq!(cidrs, vec!["10.40.0.5/32", "10.40.0.9/32"]);
        // Everything else about the rule is carried over.
        assert_eq!(out[0].port_min, 5432);
        assert!(!out[0].ipv6);
    }

    /// An address of the wrong family in an IPv4 rule is a match the dataplane
    /// cannot make. Dropped, so the rule is what it claims to be.
    #[test]
    fn a_member_of_the_wrong_family_is_not_smuggled_in() {
        let g = uuid::Uuid::new_v4();
        let mut members = crate::store::GroupMembers::new();
        members.insert(g, vec!["10.40.0.5".into(), "fd00::5".into()]);
        let v4 = expand_rule(&rule("IPv4", None, Some(g)), &members);
        assert_eq!(
            v4.iter().map(|r| r.remote_cidr.clone()).collect::<Vec<_>>(),
            vec!["10.40.0.5/32"]
        );
        let v6 = expand_rule(&rule("IPv6", None, Some(g)), &members);
        assert_eq!(
            v6.iter().map(|r| r.remote_cidr.clone()).collect::<Vec<_>>(),
            vec!["fd00::5/128"]
        );
    }

    /// A group nobody is in matches nothing — never "any". The empty
    /// `remote_cidr` an unexpanded rule would carry means exactly that.
    #[test]
    fn a_group_with_no_addressable_members_expands_to_nothing() {
        let g = uuid::Uuid::new_v4();
        assert!(expand_rule(
            &rule("IPv4", None, Some(g)),
            &crate::store::GroupMembers::new()
        )
        .is_empty());
        let mut empty = crate::store::GroupMembers::new();
        empty.insert(g, vec![]);
        assert!(expand_rule(&rule("IPv4", None, Some(g)), &empty).is_empty());
    }
}
