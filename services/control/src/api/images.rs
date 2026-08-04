//! Image endpoints (design M9): a catalog of base disks + boot recipes that
//! VMs and templates are provisioned from.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use ch_model::BootSpec;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireImageCreate, RequireImageUpdate};
use crate::store::{Image, Store};

#[derive(Debug, Deserialize)]
pub struct CreateImage {
    pub name: String,
    /// Read-only golden base disk on shared storage.
    pub source_path: String,
    /// On-disk format of the base image (`raw` | `qcow2`).
    pub format: String,
    /// Boot recipe applied to VMs created from this image.
    pub boot: BootSpec,
    /// Default provisioned volume size in bytes (omit to keep the base size).
    #[serde(default)]
    pub default_size_bytes: Option<i64>,
    /// Whether VMs from this image expect a cloud-init NoCloud seed.
    #[serde(default = "default_true")]
    pub cloud_init: bool,
    /// Free-form OS label for the UI (e.g. "ubuntu-26.04").
    #[serde(default)]
    pub os: Option<String>,
}

fn default_true() -> bool {
    true
}

pub async fn create(
    State(store): State<Store>,
    _: RequireImageCreate,
    Json(body): Json<CreateImage>,
) -> ApiResult<(StatusCode, Json<Image>)> {
    if body.name.is_empty() || body.source_path.is_empty() {
        return Err(ApiError::invalid("name and source_path are required"));
    }
    if body.format != "raw" && body.format != "qcow2" {
        return Err(ApiError::invalid("format must be 'raw' or 'qcow2'"));
    }
    let image = store
        .insert_image(
            &body.name,
            &body.source_path,
            &body.format,
            &body.boot,
            body.default_size_bytes,
            body.cloud_init,
            body.os.as_deref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(image)))
}

pub async fn update(
    State(store): State<Store>,
    _: RequireImageUpdate,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateImage>,
) -> ApiResult<Json<Image>> {
    if body.name.is_empty() || body.source_path.is_empty() {
        return Err(ApiError::invalid("name and source_path are required"));
    }
    if body.format != "raw" && body.format != "qcow2" {
        return Err(ApiError::invalid("format must be 'raw' or 'qcow2'"));
    }
    store
        .update_image(
            id,
            &body.name,
            &body.source_path,
            &body.format,
            &body.boot,
            body.default_size_bytes,
            body.cloud_init,
            body.os.as_deref(),
        )
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("image not found: {id}")))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Image>>> {
    user.require("image:read")?;
    Ok(Json(store.list_images().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Image>> {
    user.require("image:read")?;
    store
        .get_image(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("image not found: {id}")))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("image:delete")?;
    let image = store.get_image(id).await?;
    if store.delete_image(id).await? {
        // Remove the backing file only for images the platform created (M14b);
        // a registered-by-path image's file belongs to the operator.
        if let Some(img) = image {
            if img.managed {
                let _ = tokio::fs::remove_file(&img.source_path).await;
            }
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("image not found: {id}")))
    }
}

#[derive(Debug, Deserialize)]
pub struct ImportImage {
    pub name: String,
    /// URL to download the base disk from (http/https).
    pub url: String,
    pub format: String,
    pub boot: BootSpec,
    #[serde(default)]
    pub default_size_bytes: Option<i64>,
    #[serde(default = "default_true")]
    pub cloud_init: bool,
    #[serde(default)]
    pub os: Option<String>,
}

/// The shared-storage images directory (sibling of the volumes dir).
fn images_dir(store: &Store) -> std::path::PathBuf {
    let vols = std::path::PathBuf::from(store.shared_volumes_dir());
    vols.parent().unwrap_or(&vols).join("images")
}

/// Import an image by downloading it from a URL (design M14b). Returns the
/// record immediately in `importing` state; the download runs in the background
/// and flips the image to `ready` (or `failed`).
pub async fn import(
    State(store): State<Store>,
    _: RequireImageCreate,
    Json(body): Json<ImportImage>,
) -> ApiResult<(StatusCode, Json<Image>)> {
    if body.name.trim().is_empty() || body.url.trim().is_empty() {
        return Err(ApiError::invalid("name and url are required"));
    }
    if !matches!(body.format.as_str(), "raw" | "qcow2") {
        return Err(ApiError::invalid("format must be 'raw' or 'qcow2'"));
    }
    if !(body.url.starts_with("http://") || body.url.starts_with("https://")) {
        return Err(ApiError::invalid("url must be http(s)"));
    }

    let id = Uuid::new_v4();
    let ext = if body.format == "raw" { "raw" } else { "qcow2" };
    let path = images_dir(&store).join(format!("img-{id}.{ext}"));
    tokio::fs::create_dir_all(images_dir(&store))
        .await
        .map_err(|e| ApiError::internal(format!("images dir: {e}")))?;
    let image = store
        .insert_image_importing(
            id,
            body.name.trim(),
            &path.to_string_lossy(),
            &body.format,
            &body.boot,
            body.default_size_bytes,
            body.cloud_init,
            body.os.as_deref(),
        )
        .await?;

    // Background download; flips status when done.
    let store2 = store.clone();
    let url = body.url.clone();
    tokio::spawn(async move { download_image(store2, id, url, path).await });

    Ok((StatusCode::ACCEPTED, Json(image)))
}

#[derive(Debug, Deserialize)]
pub struct UploadParams {
    pub name: String,
    pub format: String,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default = "default_true")]
    pub cloud_init: bool,
    #[serde(default = "default_firmware")]
    pub firmware: String,
    #[serde(default)]
    pub default_size_bytes: Option<i64>,
}

fn default_firmware() -> String {
    "/var/lib/ch-orchestrator/firmware/CLOUDHV.fd".into()
}

/// Upload an image by streaming the disk file in the request body (design M14e).
/// Metadata comes via query params; the body is the raw image, streamed to
/// shared storage so arbitrarily large images don't buffer in memory.
pub async fn upload(
    State(store): State<Store>,
    _: RequireImageCreate,
    axum::extract::Query(p): axum::extract::Query<UploadParams>,
    body: axum::body::Body,
) -> ApiResult<(StatusCode, Json<Image>)> {
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    if p.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if !matches!(p.format.as_str(), "raw" | "qcow2") {
        return Err(ApiError::invalid("format must be 'raw' or 'qcow2'"));
    }
    let id = Uuid::new_v4();
    let ext = if p.format == "raw" { "raw" } else { "qcow2" };
    let path = images_dir(&store).join(format!("img-{id}.{ext}"));
    tokio::fs::create_dir_all(images_dir(&store))
        .await
        .map_err(|e| ApiError::internal(format!("images dir: {e}")))?;
    let boot = BootSpec::Firmware {
        firmware: p.firmware.clone().into(),
    };
    // Record it importing; flip to ready/failed once the stream lands.
    let _image = store
        .insert_image_importing(
            id,
            p.name.trim(),
            &path.to_string_lossy(),
            &p.format,
            &boot,
            p.default_size_bytes,
            p.cloud_init,
            p.os.as_deref(),
        )
        .await?;

    // Stream the request body straight to the file.
    let write: Result<(), String> = async {
        let mut file = tokio::fs::File::create(&path)
            .await
            .map_err(|e| e.to_string())?;
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| e.to_string())?;
            file.write_all(&bytes).await.map_err(|e| e.to_string())?;
        }
        file.flush().await.map_err(|e| e.to_string())?;
        Ok(())
    }
    .await;

    match write {
        Ok(()) => {
            let size = virtual_size(&path).await;
            store.set_image_status(id, "ready", size, None).await?;
            let img = store.get_image(id).await?.unwrap();
            Ok((StatusCode::CREATED, Json(img)))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            let _ = store.set_image_status(id, "failed", None, Some(&e)).await;
            Err(ApiError::internal(format!("upload failed: {e}")))
        }
    }
}

/// Download `url` to `path`, then mark the image ready/failed (design M14b).
async fn download_image(store: Store, id: Uuid, url: String, path: std::path::PathBuf) {
    let p = path.to_string_lossy().into_owned();
    let dl = tokio::task::spawn_blocking(move || {
        std::process::Command::new("curl")
            .args(["-fSL", "--connect-timeout", "20", "-o", &p, &url])
            .output()
    })
    .await;

    let result: Result<(), String> = match dl {
        Ok(Ok(out)) if out.status.success() => Ok(()),
        Ok(Ok(out)) => Err(format!(
            "download failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )),
        Ok(Err(e)) => Err(format!("curl: {e}")),
        Err(e) => Err(format!("download task: {e}")),
    };

    match result {
        Ok(()) => {
            let size = virtual_size(&path).await;
            let _ = store.set_image_status(id, "ready", size, None).await;
            tracing::info!(%id, path = %path.display(), "image import complete");
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            let _ = store.set_image_status(id, "failed", None, Some(&e)).await;
            tracing::warn!(%id, error = %e, "image import failed");
        }
    }
}

/// Guest-visible size of a downloaded image via `qemu-img info` (best-effort).
async fn virtual_size(path: &std::path::Path) -> Option<i64> {
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
