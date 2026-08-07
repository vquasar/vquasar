//! Image endpoints (design M9): a catalog of base disks + boot recipes that
//! VMs and templates are provisioned from.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;
use vquasar_model::BootSpec;

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
    scope: crate::authz::RequestScope,
    Json(body): Json<CreateImage>,
) -> ApiResult<(StatusCode, Json<Image>)> {
    if body.name.is_empty() || body.source_path.is_empty() {
        return Err(ApiError::invalid("name and source_path are required"));
    }
    if body.format != "raw" && body.format != "qcow2" {
        return Err(ApiError::invalid("format must be 'raw' or 'qcow2'"));
    }
    // A registered image is opened by the agent with privilege (design §30).
    crate::api::pathsafe::ensure_within(
        std::path::Path::new(&body.source_path),
        store.allowed_paths(),
        "source_path",
    )?;
    let image = store
        .insert_image(
            &body.name,
            &body.source_path,
            &body.format,
            &body.boot,
            body.default_size_bytes,
            body.cloud_init,
            body.os.as_deref(),
            crate::scoped::ScopedStore::new(store.clone(), scope.0).shareable_owner(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(image)))
}

pub async fn update(
    State(store): State<Store>,
    _: RequireImageUpdate,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateImage>,
) -> ApiResult<Json<Image>> {
    if body.name.is_empty() || body.source_path.is_empty() {
        return Err(ApiError::invalid("name and source_path are required"));
    }
    if body.format != "raw" && body.format != "qcow2" {
        return Err(ApiError::invalid("format must be 'raw' or 'qcow2'"));
    }
    crate::api::pathsafe::ensure_within(
        std::path::Path::new(&body.source_path),
        store.allowed_paths(),
        "source_path",
    )?;
    if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .image_writable(id)
        .await?
    {
        return Err(ApiError::not_found("image"));
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

pub async fn list(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<Image>>> {
    user.require("image:read")?;
    Ok(Json(
        crate::scoped::ScopedStore::new(store, scope.0)
            .list_images()
            .await?,
    ))
}

/// An ISO available for read-only attachment as install/driver media (design
/// M15, Windows guests). ISOs live under `<images_dir>/isos` on shared storage.
#[derive(Debug, serde::Serialize)]
pub struct IsoEntry {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// List ISOs available to attach as read-only CDs (e.g. a Windows install ISO
/// and the virtio-win driver ISO). Read-only: it only enumerates files.
pub async fn list_isos(
    State(store): State<Store>,
    user: AuthUser,
) -> ApiResult<Json<Vec<IsoEntry>>> {
    user.require("image:read")?;
    let dir = images_dir(&store).join("isos");
    let mut out = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("iso") {
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                out.push(IsoEntry {
                    name: path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default()
                        .to_string(),
                    path: path.to_string_lossy().into_owned(),
                    size_bytes: size,
                });
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(Json(out))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Image>> {
    user.require("image:read")?;
    if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .image_visible(id)
        .await?
    {
        return Err(ApiError::not_found("image"));
    }
    store
        .get_image(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("image"))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("image:delete")?;
    let image = store.get_image(id).await?;
    if crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .delete_image(id)
        .await?
    {
        // Remove the backing file only for images the platform created (M14b);
        // a registered-by-path image's file belongs to the operator.
        if let Some(img) = image {
            if img.managed {
                let _ = tokio::fs::remove_file(&img.source_path).await;
            }
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("image"))
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
/// Cap on a caller-supplied default size for an imported image. The download
/// itself is bounded by what the remote actually serves; this stops the
/// *provisioning* side from being asked for something absurd later.
fn check_default_size(size: Option<i64>) -> ApiResult<()> {
    if let Some(size) = size {
        if size < 0 {
            return Err(ApiError::invalid("default_size_bytes must not be negative"));
        }
        if size as u64 > vquasar_model::validation::MAX_DISK_BYTES {
            return Err(ApiError::invalid(format!(
                "default_size_bytes {size} exceeds the limit of {} bytes",
                vquasar_model::validation::MAX_DISK_BYTES
            )));
        }
    }
    Ok(())
}

pub async fn import(
    State(store): State<Store>,
    _: RequireImageCreate,
    scope: crate::authz::RequestScope,
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
    check_default_size(body.default_size_bytes)?;

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
            crate::scoped::ScopedStore::new(store.clone(), scope.0).shareable_owner(),
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
    "/var/lib/vquasar/firmware/CLOUDHV.fd".into()
}

/// Upload an image by streaming the disk file in the request body (design M14e).
/// Metadata comes via query params; the body is the raw image, streamed to
/// shared storage so arbitrarily large images don't buffer in memory.
pub async fn upload(
    State(store): State<Store>,
    _: RequireImageCreate,
    scope: crate::authz::RequestScope,
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
            crate::scoped::ScopedStore::new(store.clone(), scope.0).shareable_owner(),
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
