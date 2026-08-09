//! Storage pool endpoints (design §20, ADR-023).
//!
//! A pool is a platform resource, like a host: any project may place a volume
//! in any pool, so nothing here is project-scoped. Creating one names a
//! directory the agents will open with privilege, which is why it is gated on
//! `storagepool:manage` and confined to the configured storage roots (§30).
//!
//! Every response carries observed state next to the desired state it belongs
//! to: which hosts report the pool, and what they say about its capacity. A
//! pool that reads as correct and is reported by nobody is the exact situation
//! this resource exists to make visible.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use vquasar_model::{PoolParams, StoragePoolState};

use crate::api::error::{ApiError, ApiResult};
use crate::authz::AuthUser;
use crate::store::{PoolReachability, StoragePool, Store};

/// A pool as the API returns it: the row, plus what the fleet reports.
#[derive(Debug, serde::Serialize)]
pub struct PoolView {
    #[serde(flatten)]
    pub pool: StoragePool,
    /// `pending` until some host reports the pool (ADR-023).
    pub state: &'static str,
    #[serde(flatten)]
    pub reachability: PoolReachability,
}

impl PoolView {
    fn new(pool: StoragePool, reachability: PoolReachability) -> Self {
        Self {
            state: StoragePoolState::from_reporting_hosts(reachability.reachable_hosts).as_str(),
            pool,
            reachability,
        }
    }
}

/// Body for `POST /storage-pools`. The kind-specific fields are flattened, so a
/// `shared_dir` pool is `{"name": "...", "kind": "shared_dir", "path": "..."}`.
#[derive(Debug, Deserialize)]
pub struct CreatePool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(flatten)]
    pub params: PoolParams,
}

/// Body for `PATCH /storage-pools/:id`. Kind and parameters are absent on
/// purpose: a pool's identity is where its bytes are, and repointing it would
/// strand every volume already there while leaving the row looking correct.
#[derive(Debug, Deserialize)]
pub struct UpdatePool {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

fn invalid<E: std::fmt::Display>(e: E) -> ApiError {
    ApiError::invalid(e.to_string())
}

/// A name or a path that is already some other pool's.
fn duplicate(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::invalid(
            "a storage pool with that name, or over that directory, already exists",
        ),
        _ => e.into(),
    }
}

pub async fn create(
    State(store): State<Store>,
    user: AuthUser,
    Json(body): Json<CreatePool>,
) -> ApiResult<(StatusCode, Json<PoolView>)> {
    user.require("storagepool:manage")?;
    vquasar_model::validate_pool_name(&body.name).map_err(invalid)?;
    body.params.validate().map_err(invalid)?;
    // The agent opens files under this root with privilege, so a pool is one
    // more caller-supplied host path and gets the same confinement as a VM's
    // disks (design §30).
    if let Some(path) = body.params.host_path() {
        crate::api::pathsafe::ensure_within(
            std::path::Path::new(path),
            store.allowed_paths(),
            "path",
        )?;
    }
    let pool = store
        .insert_storage_pool(&body.name, body.description.as_deref(), &body.params)
        .await
        .map_err(duplicate)?;
    // Freshly created, so nothing reports it yet: `pending`, truthfully.
    Ok((
        StatusCode::CREATED,
        Json(PoolView::new(pool, PoolReachability::default())),
    ))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<PoolView>>> {
    user.require("storagepool:read")?;
    let pools = store.list_storage_pools().await?;
    let mut reach = store.pool_reachability_all().await?;
    Ok(Json(
        pools
            .into_iter()
            .map(|p| {
                let r = reach.remove(&p.id).unwrap_or_default();
                PoolView::new(p, r)
            })
            .collect(),
    ))
}

/// One pool, with every host's word on it.
///
/// The aggregate answers "can this be used"; the per-host list answers "why
/// not", which is the question an operator actually has once a placement has
/// been refused.
#[derive(Debug, serde::Serialize)]
pub struct PoolDetail {
    #[serde(flatten)]
    pub view: PoolView,
    pub hosts: Vec<crate::store::PoolHostReport>,
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PoolDetail>> {
    user.require("storagepool:read")?;
    let pool = store
        .get_storage_pool(id)
        .await?
        .ok_or(ApiError::not_found("storage pool"))?;
    let reach = store.pool_reachability(id).await?;
    let hosts = store.pool_host_reports(id).await?;
    Ok(Json(PoolDetail {
        view: PoolView::new(pool, reach),
        hosts,
    }))
}

pub async fn update(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdatePool>,
) -> ApiResult<Json<PoolView>> {
    user.require("storagepool:manage")?;
    vquasar_model::validate_pool_name(&body.name).map_err(invalid)?;
    let pool = store
        .update_storage_pool(id, &body.name, body.description.as_deref())
        .await
        .map_err(duplicate)?
        .ok_or(ApiError::not_found("storage pool"))?;
    let reach = store.pool_reachability(id).await?;
    Ok(Json(PoolView::new(pool, reach)))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("storagepool:manage")?;
    if !store.delete_storage_pool(id).await? {
        return Err(ApiError::not_found("storage pool"));
    }
    Ok(StatusCode::NO_CONTENT)
}
