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
        if let Err(e) = reconcile_migrations(&store).await {
            warn!(error = %e, "migration reconcile pass failed");
        }
        if let Err(e) = reconcile_vms(&store).await {
            warn!(error = %e, "vm reconcile pass failed");
        }
        if let Err(e) = refresh_vm_ips(&store).await {
            warn!(error = %e, "vm ip refresh pass failed");
        }
        sleep(interval).await;
    }
}

/// Refresh each VM's agentlessly-discovered IP from its host, independent of the
/// reconcile generation so settled (Running) VMs still get an up-to-date address
/// (design M11).
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
        // Migrating VMs are driven by the migration controller, not here.
        if vm.phase == "Migrating" {
            continue;
        }
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

/// Advance each in-flight live migration by one step (design section 28). The
/// migration is a persisted state machine, so it resumes after a control-plane
/// restart. One step runs per tick; failures move it to `Failed` and leave the
/// VM on its source host.
pub async fn reconcile_migrations(store: &Store) -> anyhow::Result<()> {
    for m in store.list_active_migrations().await? {
        if let Err(e) = advance_migration(store, &m).await {
            warn!(migration = %m.id, vm = %m.vm_id, error = %e, "migration failed");
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
    match agent
        .ensure_vm(
            vm.id.to_string(),
            vm.name.clone(),
            spec_json,
            bindings,
            network_config,
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
            mtu: network.is_overlay().then_some(1450),
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
    for h in store.list_hosts().await? {
        if h.id == this_host {
            continue;
        }
        if let Some(ip) = resolve_underlay(&h.endpoint).await {
            overlay_peers.push(ip);
        }
    }

    for (index, nic) in vm.spec.network_interfaces.iter().enumerate() {
        let Some(network) = store.get_network(nic.network_id.as_uuid()).await? else {
            return Ok(None);
        };
        let mac = nic
            .mac
            .clone()
            .unwrap_or_else(|| allocate_mac(vm.id, index));
        let (vni, peers) = match network.vni {
            Some(v) => (v as u32, overlay_peers.clone()),
            None => (0, Vec::new()),
        };

        // Security groups (design M13c): a NIC with groups is filtered — collect
        // the union of their ingress rules as the allow-list.
        let (filtered, ingress_rules) = if nic.security_groups.is_empty() {
            (false, Vec::new())
        } else {
            let rules = store
                .rules_for_groups(&nic.security_groups)
                .await?
                .into_iter()
                .filter(|r| r.direction == "ingress")
                .map(|r| ch_proto::agent::SecurityRule {
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
        assert_eq!(underlay_ip("https://172.16.56.81:9500").as_deref(), Some("172.16.56.81"));
        assert_eq!(underlay_ip("http://10.0.0.5:9500").as_deref(), Some("10.0.0.5"));
        assert_eq!(underlay_ip("172.16.56.81:9500").as_deref(), Some("172.16.56.81"));
        assert_eq!(underlay_ip("172.16.56.81").as_deref(), Some("172.16.56.81"));
        assert_eq!(underlay_ip("chnode1.lab").as_deref(), Some("chnode1.lab"));
        assert_eq!(underlay_ip("[fd00::1]:9500").as_deref(), Some("fd00::1"));
        assert_eq!(underlay_ip("").as_deref(), None);
    }
}
