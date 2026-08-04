//! Template endpoints (design M9): reusable VM presets instantiated into a
//! full spec by `POST /vms/from-template` (see [`crate::api::vms`]).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use ch_model::CloudInitSpec;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireTemplateCreate, RequireTemplateUpdate};
use crate::store::{Store, Template};

#[derive(Debug, Deserialize)]
pub struct CreateTemplate {
    pub name: String,
    pub image_id: Uuid,
    pub boot_vcpus: i32,
    pub max_vcpus: i32,
    pub memory_mib: i64,
    /// Provisioned volume size in bytes (omit to use the image default).
    #[serde(default)]
    pub disk_size_bytes: Option<i64>,
    /// Volume format for VMs from this template (`qcow2` | `raw`).
    #[serde(default = "default_disk_format")]
    pub disk_format: String,
    /// Optional default network for the VM's primary NIC.
    #[serde(default)]
    pub network_id: Option<Uuid>,
    /// Cloud-init defaults applied unless overridden at instantiation.
    #[serde(default)]
    pub cloud_init: Option<CloudInitSpec>,
}

fn default_disk_format() -> String {
    "qcow2".to_string()
}

pub async fn create(
    State(store): State<Store>,
    _: RequireTemplateCreate,
    Json(body): Json<CreateTemplate>,
) -> ApiResult<(StatusCode, Json<Template>)> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if body.boot_vcpus < 1 || body.max_vcpus < body.boot_vcpus {
        return Err(ApiError::invalid("require 1 <= boot_vcpus <= max_vcpus"));
    }
    if body.memory_mib < 1 {
        return Err(ApiError::invalid("memory_mib must be positive"));
    }
    if body.disk_format != "raw" && body.disk_format != "qcow2" {
        return Err(ApiError::invalid("disk_format must be 'raw' or 'qcow2'"));
    }
    // The referenced image must exist (FK also enforces this, but a clear 400
    // beats a 500 on a bad request).
    if store.get_image(body.image_id).await?.is_none() {
        return Err(ApiError::invalid(format!(
            "image not found: {}",
            body.image_id
        )));
    }
    let tpl = store
        .insert_template(
            &body.name,
            body.image_id,
            body.boot_vcpus,
            body.max_vcpus,
            body.memory_mib,
            body.disk_size_bytes,
            &body.disk_format,
            body.network_id,
            body.cloud_init.as_ref(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(tpl)))
}

pub async fn update(
    State(store): State<Store>,
    _: RequireTemplateUpdate,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateTemplate>,
) -> ApiResult<Json<Template>> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if body.boot_vcpus < 1 || body.max_vcpus < body.boot_vcpus {
        return Err(ApiError::invalid("require 1 <= boot_vcpus <= max_vcpus"));
    }
    if body.disk_format != "raw" && body.disk_format != "qcow2" {
        return Err(ApiError::invalid("disk_format must be 'raw' or 'qcow2'"));
    }
    if store.get_image(body.image_id).await?.is_none() {
        return Err(ApiError::invalid(format!(
            "image not found: {}",
            body.image_id
        )));
    }
    store
        .update_template(
            id,
            &body.name,
            body.image_id,
            body.boot_vcpus,
            body.max_vcpus,
            body.memory_mib,
            body.disk_size_bytes,
            &body.disk_format,
            body.network_id,
            body.cloud_init.as_ref(),
        )
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("template not found: {id}")))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Template>>> {
    user.require("template:read")?;
    Ok(Json(store.list_templates().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Template>> {
    user.require("template:read")?;
    store
        .get_template(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("template not found: {id}")))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("template:delete")?;
    if store.delete_template(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("template not found: {id}")))
    }
}
