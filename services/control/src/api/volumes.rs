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
use crate::store::{Store, Volume};

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
fn volume_path(dir: &str, id: Uuid, format: &str) -> PathBuf {
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
    pub size_bytes: i64,
    #[serde(default = "default_format")]
    pub format: String,
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
    if body.size_bytes <= 0 {
        return Err(ApiError::invalid("size_bytes must be positive"));
    }
    if !matches!(body.format.as_str(), "raw" | "qcow2") {
        return Err(ApiError::invalid("format must be raw or qcow2"));
    }

    let id = Uuid::new_v4();
    let path = volume_path(store.shared_volumes_dir(), id, &body.format);
    provision_blank(&path, &body.format, body.size_bytes as u64).await?;

    match store
        .create_volume(id, body.name.trim(), body.size_bytes, &body.format)
        .await
    {
        Ok(v) => Ok((StatusCode::CREATED, Json(view(&store, v)))),
        Err(e) => {
            // Roll back the file if the metadata insert failed.
            let _ = tokio::fs::remove_file(&path).await;
            Err(e.into())
        }
    }
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
