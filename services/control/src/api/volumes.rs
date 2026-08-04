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

use ch_model::{DiskImageType, DiskSpec};

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireVolumeCreate, RequireVolumeUpdate};
use crate::store::{Store, Volume, VolumeSnapshot};

/// Volume + its derived backing-file path and attachment.
#[derive(Serialize)]
pub struct VolumeView {
    #[serde(flatten)]
    pub volume: Volume,
    pub path: String,
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

fn view(store: &Store, v: Volume) -> VolumeView {
    let path = volume_path(store.shared_volumes_dir(), v.id, &v.format)
        .to_string_lossy()
        .into_owned();
    VolumeView { volume: v, path }
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<VolumeView>>> {
    user.require("volume:read")?;
    Ok(Json(
        store
            .list_volumes()
            .await?
            .into_iter()
            .map(|v| view(&store, v))
            .collect(),
    ))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<VolumeView>> {
    user.require("volume:read")?;
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
    Ok(Json(view(&store, v)))
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
}

fn default_format() -> String {
    "qcow2".into()
}

pub async fn create(
    State(store): State<Store>,
    _: RequireVolumeCreate,
    Json(body): Json<CreateVolume>,
) -> ApiResult<(StatusCode, Json<VolumeView>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if !matches!(body.format.as_str(), "raw" | "qcow2") {
        return Err(ApiError::invalid("format must be raw or qcow2"));
    }

    let id = Uuid::new_v4();

    // Clone-from-image (bootable) vs blank data volume.
    let (format, size, path) = if let Some(image_id) = body.source_image_id {
        let img = store
            .get_image(image_id)
            .await?
            .ok_or_else(|| ApiError::invalid(format!("image not found: {image_id}")))?;
        if img.status != "ready" {
            return Err(ApiError::invalid("image is not ready"));
        }
        let format = img.format.clone();
        let path = volume_path(store.shared_volumes_dir(), id, &format);
        // Full, independent copy so the volume outlives the image.
        qemu_img(&["convert", "-O", ext(&format), &img.source_path, &path.to_string_lossy()]).await?;
        if body.size_bytes > 0 {
            // Grow to the requested size if it exceeds the image's virtual size.
            let _ = qemu_img(&["resize", &path.to_string_lossy(), &body.size_bytes.to_string()]).await;
        }
        let size = disk_virtual_size(&path).await.unwrap_or(body.size_bytes.max(0));
        (format, size, path)
    } else {
        if body.size_bytes <= 0 {
            return Err(ApiError::invalid("size_bytes must be positive"));
        }
        let path = volume_path(store.shared_volumes_dir(), id, &body.format);
        provision_blank(&path, &body.format, body.size_bytes as u64).await?;
        (body.format.clone(), body.size_bytes, path)
    };

    match store
        .create_volume(id, body.name.trim(), size, &format, body.source_image_id)
        .await
    {
        Ok(v) => Ok((StatusCode::CREATED, Json(view(&store, v)))),
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            Err(e.into())
        }
    }
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
) -> ApiResult<StatusCode> {
    user.require("volume:delete")?;
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
    if v.attached_vm_id.is_some() {
        return Err(ApiError::invalid("detach the volume before deleting it"));
    }
    let path = volume_path(store.shared_volumes_dir(), v.id, &v.format);
    let _ = tokio::fs::remove_file(&path).await; // best-effort
    store.delete_volume(id).await?;
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
    Json(body): Json<AttachVolume>,
) -> ApiResult<Json<VolumeView>> {
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
    if v.attached_vm_id.is_some() {
        return Err(ApiError::invalid("volume is already attached"));
    }
    let vm = store
        .get_vm(body.vm_id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("vm not found: {}", body.vm_id)))?;

    let path = volume_path(store.shared_volumes_dir(), v.id, &v.format);
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
    });
    spec.validate().map_err(|e| ApiError::invalid(e.to_string()))?;
    store.set_vm_spec(body.vm_id, &spec).await?;
    store
        .set_volume_attachment(id, Some(body.vm_id), Some(serial))
        .await?;
    let v = store.get_volume(id).await?.unwrap();
    Ok(Json(view(&store, v)))
}

pub async fn detach(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<VolumeView>> {
    user.require("volume:update")?;
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
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
        let target = volume_path(store.shared_volumes_dir(), v.id, &v.format);
        let mut spec = vm.spec.0.clone();
        spec.disks.retain(|d| d.path != target);
        store.set_vm_spec(vm_id, &spec).await?;
    }
    store.set_volume_attachment(id, None, None).await?;
    let v = store.get_volume(id).await?.unwrap();
    Ok(Json(view(&store, v)))
}

// ---- snapshots (design M14c) -----------------------------------------------

/// Guard shared by snapshot create/revert: the volume must be qcow2 and not held
/// by a running VMM (which holds an exclusive lock on the file).
async fn snapshottable(store: &Store, v: &Volume) -> ApiResult<()> {
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
) -> ApiResult<Json<Vec<VolumeSnapshot>>> {
    user.require("volume:read")?;
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
    Json(body): Json<CreateSnapshot>,
) -> ApiResult<(StatusCode, Json<VolumeSnapshot>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
    snapshottable(&store, &v).await?;
    let snap_id = Uuid::new_v4();
    let path = volume_path(store.shared_volumes_dir(), v.id, &v.format);
    // Tag the qcow2 internal snapshot with the record id (stable + unique).
    qemu_img(&["snapshot", "-c", &snap_id.to_string(), &path.to_string_lossy()]).await?;
    let snap = store
        .create_volume_snapshot(snap_id, id, body.name.trim())
        .await?;
    Ok((StatusCode::CREATED, Json(snap)))
}

pub async fn delete_snapshot(
    State(store): State<Store>,
    user: AuthUser,
    Path((id, snap_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    user.require("volume:update")?;
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
    if store.get_volume_snapshot(snap_id).await?.is_none() {
        return Err(ApiError::invalid("snapshot not found"));
    }
    let path = volume_path(store.shared_volumes_dir(), v.id, &v.format);
    // Best-effort qcow2 delete (ignore if already gone), then drop the record.
    let _ = qemu_img(&["snapshot", "-d", &snap_id.to_string(), &path.to_string_lossy()]).await;
    store.delete_volume_snapshot(snap_id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn revert_snapshot(
    State(store): State<Store>,
    _: RequireVolumeUpdate,
    Path((id, snap_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let v = store
        .get_volume(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("volume not found: {id}")))?;
    if store.get_volume_snapshot(snap_id).await?.is_none() {
        return Err(ApiError::invalid("snapshot not found"));
    }
    snapshottable(&store, &v).await?;
    let path = volume_path(store.shared_volumes_dir(), v.id, &v.format);
    qemu_img(&["snapshot", "-a", &snap_id.to_string(), &path.to_string_lossy()]).await?;
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
async fn provision_blank(path: &std::path::Path, format: &str, size_bytes: u64) -> ApiResult<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| ApiError::internal(format!("volume dir: {e}")))?;
    }
    let fmt = ext(format).to_string();
    let p = path.to_string_lossy().into_owned();
    let size = size_bytes.to_string();
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("qemu-img")
            .args(["create", "-f", &fmt, &p, &size])
            .output()
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
