//! Host endpoints (design document, section 14).

use axum::extract::{Path, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireHostManage};
use crate::cpucompat::CpuCompat;
use crate::store::{Host, Store};

/// Register a host by name + agent gRPC endpoint. The host controller then
/// polls it and flips it to `Ready` (design section 13: manual dev enrollment).
#[derive(Debug, Deserialize)]
pub struct RegisterHost {
    pub name: String,
    pub endpoint: String,
}

pub async fn register(
    State(store): State<Store>,
    _: RequireHostManage,
    Json(body): Json<RegisterHost>,
) -> ApiResult<(axum::http::StatusCode, Json<Host>)> {
    if body.name.is_empty() || body.endpoint.is_empty() {
        return Err(ApiError::invalid("name and endpoint are required"));
    }
    let host = store.register_host(&body.name, &body.endpoint).await?;
    Ok((axum::http::StatusCode::CREATED, Json(host)))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Host>>> {
    user.require("host:read")?;
    Ok(Json(store.list_hosts().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Host>> {
    user.require("host:read")?;
    store
        .get_host(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::host_not_found(id))
}

/// Host lifecycle: cordon/uncordon (design M15). `schedulable=false` puts the
/// host in maintenance mode — the scheduler places no new VMs there, but VMs
/// already running keep running (drain to evacuate them).
#[derive(Debug, Deserialize)]
pub struct UpdateHost {
    pub schedulable: bool,
}

pub async fn update(
    State(store): State<Store>,
    _: RequireHostManage,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateHost>,
) -> ApiResult<Json<Host>> {
    let host = store
        .get_host(id)
        .await?
        .ok_or_else(|| ApiError::host_not_found(id))?;
    store.set_host_schedulable(id, body.schedulable).await?;
    let event = if body.schedulable {
        "host.uncordoned"
    } else {
        "host.cordoned"
    };
    store
        .insert_event("host", Some(id), event, "info", &host.name)
        .await?;
    store
        .get_host(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::host_not_found(id))
}

/// One VM the drain is live-migrating away, and where to.
#[derive(Debug, Serialize)]
pub struct DrainMove {
    pub vm_id: Uuid,
    pub vm_name: String,
    pub target_host_id: Uuid,
    pub target_host_name: String,
}

/// A running VM the drain could not evacuate, with the reason.
#[derive(Debug, Serialize)]
pub struct DrainSkip {
    pub vm_id: Uuid,
    pub vm_name: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct DrainResult {
    pub cordoned: bool,
    pub migrating: Vec<DrainMove>,
    pub skipped: Vec<DrainSkip>,
}

/// Drain a host (design M15, host lifecycle): cordon it, then live-migrate each
/// running VM to a CPU-compatible host the scheduler picks by free capacity
/// (reuses the M8 migration machinery and the M15 CPU-compatibility gate).
/// Stopped VMs are left in place; a running VM with no compatible host that has
/// capacity is reported as skipped rather than forced.
pub async fn drain(
    State(store): State<Store>,
    _: RequireHostManage,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DrainResult>> {
    let source = store
        .get_host(id)
        .await?
        .ok_or_else(|| ApiError::host_not_found(id))?;

    // Cordon first so nothing new lands while we evacuate.
    store.set_host_schedulable(id, false).await?;
    store
        .insert_event("host", Some(id), "host.cordoned", "info", &source.name)
        .await?;

    let candidates: Vec<Host> = store
        .list_schedulable_hosts()
        .await?
        .into_iter()
        .filter(|h| h.id != id)
        .collect();
    let mut committed = crate::reconcile::committed_by_host(&store)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?;
    // A drain target must be able to reach the VM's storage. Shared storage is
    // what makes live migration possible at all (§28), and it has been assumed
    // rather than checked since it was written (ADR-023).
    let pools = store.pools_by_host().await?;
    // A VM pinned to this host by local storage cannot be evacuated at all, so
    // a drain has to say so rather than report it as "no capacity" (ADR-025).
    let local = store.local_pool_ids().await?;

    let mut migrating = Vec::new();
    let mut skipped = Vec::new();
    for vm in store.list_vms().await? {
        if vm.host_id != Some(id) || vm.phase != "Running" {
            continue;
        }
        if store.active_migration_for_vm(vm.id).await?.is_some() {
            skipped.push(DrainSkip {
                vm_id: vm.id,
                vm_name: vm.name.clone(),
                reason: "a migration is already in progress".into(),
            });
            continue;
        }
        // Only CPU-compatible destinations (M15); then let the scheduler pick by
        // free capacity among those.
        let compat: Vec<Host> = candidates
            .iter()
            .filter(|h| {
                !matches!(
                    crate::cpucompat::check(
                        source.cpu_vendor.as_deref(),
                        &source.cpu_features,
                        h.cpu_vendor.as_deref(),
                        &h.cpu_features,
                    ),
                    CpuCompat::VendorMismatch { .. } | CpuCompat::MissingFeatures(_)
                )
            })
            .cloned()
            .collect();
        if crate::scheduler::required_pools(&vm.spec.0)
            .iter()
            .any(|p| local.contains(p))
        {
            skipped.push(DrainSkip {
                vm_id: vm.id,
                vm_name: vm.name,
                reason: "pinned to this host by a disk on local storage".into(),
            });
            continue;
        }
        match crate::scheduler::schedule(&vm.spec, &compat, &committed, &pools) {
            Ok(dest_id) => {
                let dest_name = compat
                    .iter()
                    .find(|h| h.id == dest_id)
                    .map(|h| h.name.clone())
                    .unwrap_or_default();
                let task = store.insert_task("vm.migrate", Some(vm.id)).await?;
                store
                    .insert_migration(vm.id, Some(id), dest_id, task.id)
                    .await?;
                store.set_vm_phase(vm.id, "Migrating").await?;
                store
                    .insert_event("vm", Some(vm.id), "migration.requested", "info", &vm.name)
                    .await?;
                // Account for the added load so multiple VMs spread across dests.
                let e = committed.entry(dest_id).or_default();
                e.vcpus += vm.spec.cpu.boot_vcpus as i64;
                e.memory_bytes += vm.spec.memory.size_bytes() as i64;
                migrating.push(DrainMove {
                    vm_id: vm.id,
                    vm_name: vm.name,
                    target_host_id: dest_id,
                    target_host_name: dest_name,
                });
            }
            // A drain that cannot move a VM should say which wall it hit:
            // "nowhere has room" and "nowhere can see its disks" are different
            // problems with different fixes (ADR-023).
            Err(crate::scheduler::Unschedulable::UnreachableStorage) => skipped.push(DrainSkip {
                vm_id: vm.id,
                vm_name: vm.name,
                reason: "no CPU-compatible host reports this VM's storage pool".into(),
            }),
            Err(crate::scheduler::Unschedulable::NoCapacity) => skipped.push(DrainSkip {
                vm_id: vm.id,
                vm_name: vm.name,
                reason: "no CPU-compatible host with free capacity".into(),
            }),
        }
    }
    store
        .insert_event(
            "host",
            Some(id),
            "host.drain",
            "info",
            &format!(
                "{}: {} migrating, {} skipped",
                source.name,
                migrating.len(),
                skipped.len()
            ),
        )
        .await?;
    Ok(Json(DrainResult {
        cordoned: true,
        migrating,
        skipped,
    }))
}
