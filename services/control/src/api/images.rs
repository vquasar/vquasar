//! Image endpoints (design M9): a catalog of base disks + boot recipes that
//! VMs and templates are provisioned from.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use ch_model::BootSpec;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::AuthUser;
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
    user: AuthUser,
    Json(body): Json<CreateImage>,
) -> ApiResult<(StatusCode, Json<Image>)> {
    user.require("image:create")?;
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
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateImage>,
) -> ApiResult<Json<Image>> {
    user.require("image:update")?;
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
    if store.delete_image(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("image not found: {id}")))
    }
}
