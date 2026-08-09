//! First-class volume endpoints (design M14a): block devices managed
//! independently of any VM. The control plane provisions the backing image on
//! shared storage (which it can reach — it is the NFS server), records the
//! metadata, and attaches/detaches volumes by editing the target VM's spec.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use vquasar_model::{DiskImageType, DiskSpec};

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireVolumeCreate, RequireVolumeUpdate};
use crate::store::{Store, Volume, VolumeSnapshot};

/// Volume + its derived backing-file path and attachment.
#[derive(Serialize)]
pub struct VolumeView {
    #[serde(flatten)]
    pub volume: Volume,
    pub path: String,
    /// How many snapshots this volume has. Carried on the row because the
    /// alternative — one request per volume — is what kept it off the list.
    pub snapshot_count: i64,
}

fn ext(format: &str) -> &str {
    if format == "raw" {
        "raw"
    } else {
        "qcow2"
    }
}

/// The backing-file path for a volume on shared storage.
pub(crate) fn volume_path(dir: &str, id: Uuid, format: &str) -> PathBuf {
    PathBuf::from(dir).join(format!("vol-{id}.{}", ext(format)))
}

/// Where a volume's bytes live: its pool's root (ADR-023), falling back to the
/// configured shared directory for one that predates pools and has not been
/// adopted into `default` yet.
fn dir_in(store: &Store, pools: &PoolPaths, v: &Volume) -> String {
    v.pool_id
        .and_then(|id| pools.get(&id).cloned())
        .unwrap_or_else(|| store.shared_volumes_dir().to_string())
}

type PoolPaths = std::collections::HashMap<Uuid, String>;

type SnapshotCounts = std::collections::HashMap<Uuid, i64>;

fn view(store: &Store, pools: &PoolPaths, snaps: &SnapshotCounts, v: Volume) -> VolumeView {
    let path = volume_path(&dir_in(store, pools, &v), v.id, &v.format)
        .to_string_lossy()
        .into_owned();
    let snapshot_count = snaps.get(&v.id).copied().unwrap_or(0);
    VolumeView {
        volume: v,
        path,
        snapshot_count,
    }
}

/// The same for a single volume, when the caller has no map to hand. Pools are
/// a handful of rows, so one read beats threading the map through every path.
async fn one(store: &Store, v: Volume) -> ApiResult<VolumeView> {
    let pools = store.pool_paths().await?;
    let snaps = store.snapshot_counts().await?;
    Ok(view(store, &pools, &snaps, v))
}

/// A volume's own directory, for the file operations that act on it.
async fn dir_of(store: &Store, v: &Volume) -> ApiResult<String> {
    Ok(dir_in(store, &store.pool_paths().await?, v))
}

pub async fn list(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<VolumeView>>> {
    user.require("volume:read")?;
    let scoped = crate::scoped::ScopedStore::new(store.clone(), scope.0);
    let pools = store.pool_paths().await?;
    // One aggregate for the whole page, not one request per row.
    let snaps = store.snapshot_counts().await?;
    Ok(Json(
        scoped
            .list_volumes()
            .await?
            .into_iter()
            .map(|v| view(&store, &pools, &snaps, v))
            .collect(),
    ))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<VolumeView>> {
    user.require("volume:read")?;
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    Ok(Json(one(&store, v).await?))
}

#[derive(Deserialize)]
pub struct CreateVolume {
    pub name: String,
    /// Blank size, or the minimum size when cloning from an image (grown to fit).
    #[serde(default)]
    pub size_bytes: i64,
    #[serde(default = "default_format")]
    pub format: String,
    /// Clone from this image to make a bootable volume (design M14d).
    #[serde(default)]
    pub source_image_id: Option<Uuid>,
    /// Cache mode, allocation and I/O ceilings for this volume (design §20).
    /// Omitted keeps the platform's previous behaviour exactly.
    #[serde(default)]
    pub policy: Option<vquasar_model::StoragePolicy>,
    /// Which host holds it, for a pool that is local to one machine (ADR-025).
    /// Required there and refused elsewhere: on shared storage no single host
    /// owns the bytes, and naming one would record a fact that is not true.
    #[serde(default)]
    pub host: Option<Uuid>,
    /// Which storage pool to place the volume in, by id or by name (ADR-023).
    /// Omitted means `default` — the pool an existing cluster was already
    /// using, so nothing about an unchanged request changes.
    #[serde(default)]
    pub pool: Option<String>,
}

fn default_format() -> String {
    "qcow2".into()
}

/// Resolve the pool a volume should be placed in: an id, a name, or `default`.
///
/// A pool nobody reports is still a legal place to put bytes — the file is
/// written by the control plane, which can reach its own storage. What it is
/// not is a place a VM can be *scheduled* against, and that refusal belongs to
/// the scheduler, where the host is known (ADR-023).
async fn resolve_pool(
    store: &Store,
    requested: Option<&str>,
) -> ApiResult<crate::store::StoragePool> {
    let name = requested.unwrap_or("default");
    if let Ok(id) = name.parse::<Uuid>() {
        if let Some(p) = store.get_storage_pool(id).await? {
            return Ok(p);
        }
    }
    store
        .get_storage_pool_by_name(name)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("unknown storage pool {name:?}")))
}

pub async fn create(
    State(store): State<Store>,
    _: RequireVolumeCreate,
    scope: crate::authz::RequestScope,
    Json(body): Json<CreateVolume>,
) -> ApiResult<(StatusCode, Json<VolumeView>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if !matches!(body.format.as_str(), "raw" | "qcow2") {
        return Err(ApiError::invalid("format must be raw or qcow2"));
    }
    // This path runs qemu-img on shared storage *before* anything is persisted,
    // so an absurd size is real work and real disk, not just a bad row.
    if body.size_bytes < 0 {
        return Err(ApiError::invalid("size_bytes must not be negative"));
    }
    if body.size_bytes as u64 > vquasar_model::validation::MAX_DISK_BYTES {
        return Err(ApiError::invalid(format!(
            "size_bytes {} exceeds the limit of {} bytes",
            body.size_bytes,
            vquasar_model::validation::MAX_DISK_BYTES
        )));
    }

    if let Some(p) = &body.policy {
        p.validate().map_err(|e| ApiError::invalid(e.to_string()))?;
    }
    let id = Uuid::new_v4();
    let pool = resolve_pool(&store, body.pool.as_deref()).await?;
    // Where the file gets built depends on who can reach the storage: the
    // control plane for a shared pool, the host that owns the disk for a local
    // one (ADR-025). Which host is the operator's call — a volume exists before
    // any VM references it, so nothing else has chosen yet, and the choice
    // pins every VM that later attaches it.
    let local = !pool.params.0.sharing().is_shared();
    let host = match (local, body.host) {
        (true, Some(h)) => Some(host_for_pool(&store, h, pool.id, &pool.name).await?),
        (true, None) => {
            return Err(ApiError::invalid(format!(
                "storage pool {:?} is local to each host, so a volume in it has to name the \
                 host that will hold it: pass \"host\".",
                pool.name
            )))
        }
        (false, Some(_)) => {
            return Err(ApiError::invalid(format!(
                "storage pool {:?} is shared, so no single host holds a volume in it — \
                 remove \"host\".",
                pool.name
            )))
        }
        (false, None) => None,
    };
    let pool_dir = pool
        .params
        .0
        .host_path()
        .ok_or_else(|| ApiError::invalid("that pool has no host path to place a volume in"))?
        .to_string();

    // Reserve before doing the work, then build the file, then finalise
    // (ADR-019). The old order — provision, then insert — cannot be admitted:
    // the expensive part would happen before anything was counted, so two
    // concurrent creates would both convert gigabytes and only then discover
    // one of them did not fit.
    //
    // For a clone the true size is the image's virtual size, which `qemu-img`
    // only reveals after converting. Reserve the largest figure known up front;
    // `finalize_volume` admits the difference if the result is bigger.
    let (format, reserve) = match body.source_image_id {
        Some(image_id) => {
            if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
                .image_visible(image_id)
                .await?
            {
                return Err(ApiError::not_found("image"));
            }
            let img = store
                .get_image(image_id)
                .await?
                .ok_or(ApiError::not_found("image"))?;
            if img.status != "ready" {
                return Err(ApiError::invalid("image is not ready"));
            }
            (
                img.format.clone(),
                body.size_bytes.max(img.default_size_bytes.unwrap_or(0)),
            )
        }
        None => {
            if body.size_bytes <= 0 {
                return Err(ApiError::invalid("size_bytes must be positive"));
            }
            (body.format.clone(), body.size_bytes)
        }
    };

    store
        .create_volume(
            id,
            body.name.trim(),
            reserve,
            &format,
            body.source_image_id,
            crate::scoped::ScopedStore::new(store.clone(), scope.0).owning_project(),
            pool.id,
            body.policy.as_ref(),
            host.as_ref().map(|h| h.id),
        )
        .await?;

    let path = volume_path(&pool_dir, id, &format);
    let built = match &host {
        Some(h) => provision_on_host(&store, &body, h, &path, &format).await,
        None => provision(&store, &body, &path, &format).await,
    };
    let size = match built {
        Ok(size) => size,
        Err(e) => {
            cleanup_partial(&store, host.as_ref(), &path).await;
            let _ = store.drop_volume_reservation(id).await;
            return Err(e);
        }
    };
    match store.finalize_volume(id, size).await {
        Ok(Some(v)) => Ok((StatusCode::CREATED, Json(one(&store, v).await?))),
        // The reservation vanished, or the true size did not fit after all.
        Ok(None) => {
            cleanup_partial(&store, host.as_ref(), &path).await;
            Err(ApiError::internal("volume reservation disappeared"))
        }
        Err(e) => {
            cleanup_partial(&store, host.as_ref(), &path).await;
            let _ = store.drop_volume_reservation(id).await;
            Err(e.into())
        }
    }
}

/// Drop a half-built volume file, on whichever machine was building it.
async fn cleanup_partial(store: &Store, host: Option<&crate::store::Host>, path: &std::path::Path) {
    let _ = store;
    match host {
        None => {
            let _ = tokio::fs::remove_file(path).await;
        }
        Some(h) => {
            let agent = crate::agent::Agent::new(h.endpoint.clone());
            let _ = agent
                .delete_volume(path.to_string_lossy().into_owned())
                .await;
        }
    }
}

/// Remove a volume's file, from whichever machine can reach it (ADR-025).
///
/// Best-effort in both cases, as deletion has always been here: the row going
/// is what the caller asked for, and a file left behind is what the orphan
/// sweep exists to find. Sending the request to the wrong machine, though,
/// would silently leave every local volume's bytes on disk forever — so the
/// host is asked rather than assumed.
async fn remove_file_wherever_it_is(store: &Store, v: &Volume, path: &std::path::Path) {
    let Some(host_id) = v.host_id else {
        let _ = tokio::fs::remove_file(path).await;
        return;
    };
    match store.get_host(host_id).await {
        Ok(Some(h)) => {
            let agent = crate::agent::Agent::new(h.endpoint.clone());
            if let Err(e) = agent
                .delete_volume(path.to_string_lossy().into_owned())
                .await
            {
                tracing::warn!(volume = %v.id, host = %h.name, error = %e,
                               "could not remove a local volume's file");
            }
        }
        _ => tracing::warn!(volume = %v.id, host = %host_id,
                            "a local volume's host is gone; its file stays behind"),
    }
}

/// Resolve the host a local volume is to be built on, refusing one that does
/// not report the pool.
///
/// The same question the scheduler asks for a VM, asked here because a volume
/// has no VM to be scheduled with — and answered against the agents' reports
/// rather than the operator's belief (ADR-023).
async fn host_for_pool(
    store: &Store,
    host: Uuid,
    pool: Uuid,
    pool_name: &str,
) -> ApiResult<crate::store::Host> {
    let h = store
        .get_host(host)
        .await?
        .ok_or_else(|| ApiError::host_not_found(host))?;
    let reports = store
        .pools_by_host()
        .await?
        .remove(&host)
        .is_some_and(|p| p.contains(&pool));
    if !reports {
        return Err(ApiError::invalid(format!(
            "host {:?} does not report storage pool {pool_name:?}, so it cannot hold a \
             volume there",
            h.name
        )));
    }
    Ok(h)
}

/// Build the volume's file on the host that owns the disk (ADR-025).
async fn provision_on_host(
    store: &Store,
    body: &CreateVolume,
    host: &crate::store::Host,
    path: &std::path::Path,
    format: &str,
) -> ApiResult<i64> {
    let source_path = match body.source_image_id {
        Some(image_id) => {
            let img = store
                .get_image(image_id)
                .await?
                .ok_or(ApiError::not_found("image"))?;
            img.source_path
        }
        None => String::new(),
    };
    let agent = crate::agent::Agent::new(host.endpoint.clone());
    let size = agent
        .provision_volume(vquasar_proto::agent::ProvisionVolumeRequest {
            path: path.to_string_lossy().into_owned(),
            format: ext(format).to_string(),
            size_bytes: body.size_bytes.max(0) as u64,
            source_path,
            preallocation: body
                .policy
                .as_ref()
                .and_then(|p| p.preallocation())
                .unwrap_or_default()
                .to_string(),
        })
        .await
        .map_err(|e| ApiError::internal(format!("provisioning on {}: {e}", host.name)))?;
    Ok(size as i64)
}

/// Build the volume's file, returning its guest-visible size.
async fn provision(
    store: &Store,
    body: &CreateVolume,
    path: &std::path::Path,
    format: &str,
) -> ApiResult<i64> {
    let prealloc = body.policy.as_ref().and_then(|p| p.preallocation());
    let Some(image_id) = body.source_image_id else {
        provision_blank(path, format, body.size_bytes as u64, prealloc).await?;
        return Ok(body.size_bytes);
    };
    let img = store
        .get_image(image_id)
        .await?
        .ok_or(ApiError::not_found("image"))?;
    // Full, independent copy so the volume outlives the image.
    let mut convert = vec!["convert", "-O", ext(format)];
    let prealloc_opt = prealloc.map(|v| format!("preallocation={v}"));
    if let Some(o) = &prealloc_opt {
        convert.extend_from_slice(&["-o", o]);
    }
    let src = img.source_path.clone();
    let dst = path.to_string_lossy().into_owned();
    convert.push(&src);
    convert.push(&dst);
    qemu_img(&convert).await?;
    if body.size_bytes > 0 {
        // Grow to the requested size if it exceeds the image's virtual size.
        let _ = qemu_img(&[
            "resize",
            &path.to_string_lossy(),
            &body.size_bytes.to_string(),
        ])
        .await;
    }
    Ok(disk_virtual_size(path)
        .await
        .unwrap_or(body.size_bytes.max(0)))
}

/// Guest-visible size of a disk via `qemu-img info` (best-effort).
async fn disk_virtual_size(path: &std::path::Path) -> Option<i64> {
    let p = path.to_string_lossy().into_owned();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("qemu-img")
            .args(["info", "--output=json", &p])
            .output()
    })
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    v.get("virtual-size").and_then(|s| s.as_i64())
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    scope: crate::authz::RequestScope,
) -> ApiResult<StatusCode> {
    user.require("volume:delete")?;
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    if v.attached_vm_id.is_some() {
        return Err(ApiError::invalid("detach the volume before deleting it"));
    }
    let path = volume_path(&dir_of(&store, &v).await?, v.id, &v.format);
    remove_file_wherever_it_is(&store, &v, &path).await;
    crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .delete_volume(id)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct AttachVolume {
    pub vm_id: Uuid,
}

pub async fn attach(
    State(store): State<Store>,
    _: RequireVolumeUpdate,
    Path(id): Path<Uuid>,
    scope: crate::authz::RequestScope,
    Json(body): Json<AttachVolume>,
) -> ApiResult<Json<VolumeView>> {
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    if v.status != "ready" {
        return Err(ApiError::invalid("volume is still being provisioned"));
    }
    if v.attached_vm_id.is_some() {
        return Err(ApiError::invalid("volume is already attached"));
    }
    let vm = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_vm(body.vm_id)
        .await?
        .ok_or(ApiError::not_found("vm"))?;

    let path = volume_path(&dir_of(&store, &v).await?, v.id, &v.format);
    let mut spec = vm.spec.0.clone();
    let serial = spec.disks.len() as i32;
    spec.disks.push(DiskSpec {
        path,
        readonly: false,
        image_type: if v.format == "raw" {
            DiskImageType::Raw
        } else {
            DiskImageType::Qcow2
        },
        source: None, // already provisioned; reuse as-is
        size_bytes: None,
        // The disk carries the volume's pool, which is what lets the scheduler
        // refuse a host that cannot reach it (ADR-023).
        pool: v.pool_id.map(vquasar_model::StoragePoolId::from_uuid),
        // …and the volume's policy, so a throttle set on the volume follows it
        // onto whichever VM it is attached to rather than being re-typed there.
        policy: v.policy.as_ref().map(|p| p.0.clone()),
        // …and, for a local volume, the one host that has those bytes. The pool
        // is not enough: every host reporting a local pool has a disk by that
        // name, and only one of them has this volume on it (ADR-025).
        pinned_host: v.host_id.map(vquasar_model::HostId::from_uuid),
    });
    spec.validate()
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    store.set_vm_spec(body.vm_id, &spec).await?;
    store
        .set_volume_attachment(id, Some(body.vm_id), Some(serial))
        .await?;
    let v = store.get_volume(id).await?.unwrap();
    Ok(Json(one(&store, v).await?))
}

pub async fn detach(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<VolumeView>> {
    user.require("volume:update")?;
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    let Some(vm_id) = v.attached_vm_id else {
        return Err(ApiError::invalid("volume is not attached"));
    };
    if let Some(vm) = store.get_vm(vm_id).await? {
        // Cloud Hypervisor has no disk hot-unplug yet, so detach needs the VM
        // powered off; the disk is removed from the spec and gone on next start.
        if vm.phase == "Running" || vm.phase == "Starting" {
            return Err(ApiError::invalid(
                "stop the VM before detaching this volume (no hot-unplug yet)",
            ));
        }
        let target = volume_path(&dir_of(&store, &v).await?, v.id, &v.format);
        let mut spec = vm.spec.0.clone();
        spec.disks.retain(|d| d.path != target);
        store.set_vm_spec(vm_id, &spec).await?;
    }
    store.set_volume_attachment(id, None, None).await?;
    let v = store.get_volume(id).await?.unwrap();
    Ok(Json(one(&store, v).await?))
}

// ---- snapshots (design M14c) -----------------------------------------------

/// Guard shared by snapshot create/revert: the volume must be qcow2 and not held
/// by a running VMM (which holds an exclusive lock on the file).
async fn snapshottable(store: &Store, v: &Volume) -> ApiResult<()> {
    if v.status != "ready" {
        return Err(ApiError::invalid("volume is still being provisioned"));
    }
    if v.format != "qcow2" {
        return Err(ApiError::invalid("snapshots require a qcow2 volume"));
    }
    if let Some(vm_id) = v.attached_vm_id {
        if let Some(vm) = store.get_vm(vm_id).await? {
            if vm.phase == "Running" || vm.phase == "Starting" {
                return Err(ApiError::invalid(
                    "stop the VM before snapshotting/reverting this volume",
                ));
            }
        }
    }
    Ok(())
}

pub async fn list_snapshots(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<VolumeSnapshot>>> {
    user.require("volume:read")?;
    if crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .is_none()
    {
        return Err(ApiError::not_found("volume"));
    }
    Ok(Json(store.list_volume_snapshots(id).await?))
}

#[derive(Deserialize)]
pub struct CreateSnapshot {
    pub name: String,
}

pub async fn create_snapshot(
    State(store): State<Store>,
    _: RequireVolumeUpdate,
    Path(id): Path<Uuid>,
    scope: crate::authz::RequestScope,
    Json(body): Json<CreateSnapshot>,
) -> ApiResult<(StatusCode, Json<VolumeSnapshot>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    snapshottable(&store, &v).await?;
    let snap_id = Uuid::new_v4();
    let path = volume_path(&dir_of(&store, &v).await?, v.id, &v.format);
    // Tag the qcow2 internal snapshot with the record id (stable + unique).
    qemu_img(&[
        "snapshot",
        "-c",
        &snap_id.to_string(),
        &path.to_string_lossy(),
    ])
    .await?;
    let snap = store
        .create_volume_snapshot(snap_id, id, body.name.trim())
        .await?;
    Ok((StatusCode::CREATED, Json(snap)))
}

pub async fn delete_snapshot(
    State(store): State<Store>,
    user: AuthUser,
    Path((id, snap_id)): Path<(Uuid, Uuid)>,
    scope: crate::authz::RequestScope,
) -> ApiResult<StatusCode> {
    user.require("volume:update")?;
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    if store.get_volume_snapshot(snap_id).await?.is_none() {
        return Err(ApiError::invalid("snapshot not found"));
    }
    let path = volume_path(&dir_of(&store, &v).await?, v.id, &v.format);
    // Best-effort qcow2 delete (ignore if already gone), then drop the record.
    let _ = qemu_img(&[
        "snapshot",
        "-d",
        &snap_id.to_string(),
        &path.to_string_lossy(),
    ])
    .await;
    store.delete_volume_snapshot(snap_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revert_snapshot(
    State(store): State<Store>,
    _: RequireVolumeUpdate,
    Path((id, snap_id)): Path<(Uuid, Uuid)>,
    scope: crate::authz::RequestScope,
) -> ApiResult<StatusCode> {
    let v = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_volume(id)
        .await?
        .ok_or(ApiError::not_found("volume"))?;
    if store.get_volume_snapshot(snap_id).await?.is_none() {
        return Err(ApiError::invalid("snapshot not found"));
    }
    snapshottable(&store, &v).await?;
    let path = volume_path(&dir_of(&store, &v).await?, v.id, &v.format);
    qemu_img(&[
        "snapshot",
        "-a",
        &snap_id.to_string(),
        &path.to_string_lossy(),
    ])
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Run `qemu-img <args>` on a blocking thread (control's tokio has no process).
async fn qemu_img(args: &[&str]) -> ApiResult<()> {
    let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("qemu-img").args(&owned).output()
    })
    .await
    .map_err(|e| ApiError::internal(format!("qemu-img join: {e}")))?
    .map_err(|e| ApiError::internal(format!("qemu-img: {e}")))?;
    if !out.status.success() {
        return Err(ApiError::internal(format!(
            "qemu-img failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}

/// Provision a blank disk image on shared storage via `qemu-img` (run on a
/// blocking thread; control's tokio has no process feature).
async fn provision_blank(
    path: &std::path::Path,
    format: &str,
    size_bytes: u64,
    prealloc: Option<&'static str>,
) -> ApiResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ApiError::internal(format!("volume dir: {e}")))?;
    }
    let fmt = ext(format).to_string();
    let p = path.to_string_lossy().into_owned();
    let size = size_bytes.to_string();
    // Thick allocation reserves the space now, so the guest cannot later hit
    // ENOSPC on a filesystem somebody else filled.
    let prealloc = prealloc.map(|v| format!("preallocation={v}"));
    let out = tokio::task::spawn_blocking(move || {
        let mut cmd = std::process::Command::new("qemu-img");
        cmd.args(["create", "-f", &fmt]);
        if let Some(o) = &prealloc {
            cmd.args(["-o", o]);
        }
        cmd.args([&p, &size]).output()
    })
    .await
    .map_err(|e| ApiError::internal(format!("qemu-img join: {e}")))?
    .map_err(|e| ApiError::internal(format!("qemu-img: {e}")))?;
    if !out.status.success() {
        return Err(ApiError::internal(format!(
            "qemu-img create failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(())
}
