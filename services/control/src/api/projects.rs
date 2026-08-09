//! Project endpoints — the unit of tenancy (design §47, ADR-018).
//!
//! Projects exist as objects before anything is scoped to them. That ordering
//! is deliberate: the schema change is the invasive part and lands once, while
//! scoping, per-project RBAC and quotas follow as separate, reviewable steps.
//! Until `[tenancy] enabled` is on, this is a catalogue of one.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::AuthUser;
use crate::store::{Project, Store};

#[derive(Debug, Deserialize)]
pub struct CreateProject {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

fn check_name(name: &str) -> ApiResult<()> {
    vquasar_model::project::validate_name(name).map_err(|e| ApiError::invalid(e.to_string()))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Project>>> {
    user.require("project:read")?;
    let all = store.list_projects().await?;
    // Which projects exist is itself tenancy information: a tenant must not
    // learn the shape of the fleet from the picker. A caller with a
    // platform-wide binding is not a tenant and sees them all (ADR-020).
    let Some(caller) = user.user.as_ref() else {
        return Ok(Json(all)); // dev superuser
    };
    let Some(mine) = store.projects_for_caller(caller.id, &user.groups).await? else {
        return Ok(Json(all));
    };
    Ok(Json(
        all.into_iter().filter(|p| mine.contains(&p.id)).collect(),
    ))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Project>> {
    user.require("project:read")?;
    if let Some(u) = user.user.as_ref() {
        if let Some(mine) = store.projects_for_caller(u.id, &user.groups).await? {
            if !mine.contains(&id) {
                return Err(ApiError::not_found("project"));
            }
        }
    }
    store
        .get_project(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("project"))
}

pub async fn create(
    State(store): State<Store>,
    user: AuthUser,
    Json(body): Json<CreateProject>,
) -> ApiResult<(StatusCode, Json<Project>)> {
    // Creating a tenancy boundary is a platform act, not a workload one.
    user.require("project:create")?;
    check_name(&body.name)?;
    let project = store
        .insert_project(&body.name, body.description.as_deref())
        .await
        .map_err(duplicate_name)?;
    // A project carries a policy group from the moment it exists, the way a
    // network does (ADR-017): a tenant's baseline has to have somewhere to go
    // that is not a provider network every other tenant shares (design §18).
    let sg = store
        .insert_project_default_group(project.id, &project.name)
        .await?;
    store.set_project_default_group(project.id, sg).await?;
    let project = store.get_project(project.id).await?.unwrap_or(project);
    Ok((StatusCode::CREATED, Json(project)))
}

pub async fn update(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateProject>,
) -> ApiResult<Json<Project>> {
    user.require("project:update")?;
    check_name(&body.name)?;
    store
        .update_project(id, &body.name, body.description.as_deref())
        .await
        .map_err(duplicate_name)?
        .map(Json)
        .ok_or_else(|| ApiError::not_found("project"))
}

/// `DELETE /projects/:id` — refuses while the project still owns anything.
///
/// Not a cascade. Deleting a project's VMs is a long, agent-touching operation
/// that has to survive a restart, and a DELETE that quietly started one would
/// be the wrong shape (design §7). The refusal names what is in the way so the
/// operator can act on it.
pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("project:delete")?;
    let project = store
        .get_project(id)
        .await?
        .ok_or_else(|| ApiError::not_found("project"))?;
    if project.is_default {
        return Err(ApiError::invalid(
            "the default project cannot be deleted: every caller without project \
             context resolves to it",
        ));
    }
    let contents = store.project_contents(id).await?;
    if !contents.is_empty() {
        return Err(ApiError::invalid(format!(
            "project still owns {} — delete or move them first",
            contents.summary()
        )));
    }
    if store.delete_project(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("project"))
    }
}

/// Project names are identifiers, so a collision is the caller's problem to
/// see, not an opaque 500.
fn duplicate_name(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::invalid("a project with that name already exists")
        }
        _ => e.into(),
    }
}

// ---- quotas (ADR-019) ------------------------------------------------------

/// A project's limits and what it is currently using.
///
/// Returned together on purpose: a limit without the usage beside it does not
/// answer the question an operator actually has, which is whether there is room.
#[derive(serde::Serialize)]
pub struct QuotaView {
    pub limits: crate::quota::Limits,
    pub usage: crate::quota::Amounts,
    /// True when usage already exceeds a limit — after that limit was lowered,
    /// which is permitted. New commitments are refused; nothing is destroyed.
    pub over_quota: bool,
}

fn over(limits: &crate::quota::Limits, usage: &crate::quota::Amounts) -> bool {
    // A one-unit demand in each dimension is exactly "is there room for more".
    [
        (limits.max_vms, usage.vms),
        (limits.max_vcpus, usage.vcpus),
        (limits.max_memory_mib, usage.memory_mib),
        (limits.max_volumes, usage.volumes),
        (limits.max_storage_bytes, usage.storage_bytes),
    ]
    .into_iter()
    .any(|(limit, used)| limit.is_some_and(|l| used > l))
}

pub async fn get_quota(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<QuotaView>> {
    user.require("project:read")?;
    if let Some(u) = user.user.as_ref() {
        if let Some(mine) = store.projects_for_caller(u.id, &user.groups).await? {
            if !mine.contains(&id) {
                return Err(ApiError::not_found("project"));
            }
        }
    }
    if store.get_project(id).await?.is_none() {
        return Err(ApiError::not_found("project"));
    }
    let limits = store.get_quota(id).await?.unwrap_or_default();
    let usage = store.quota_usage(id).await?;
    Ok(Json(QuotaView {
        over_quota: over(&limits, &usage),
        limits,
        usage,
    }))
}

/// `PUT /projects/:id/quota` — replace the limits. An omitted field is
/// unlimited, so this is a whole-object write and there is no way to leave a
/// stale limit behind by forgetting to mention it.
pub async fn set_quota(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<crate::quota::Limits>,
) -> ApiResult<Json<QuotaView>> {
    // Setting a quota is shaping tenancy, which is a platform act — the same
    // authority that creates the boundary sets what fits inside it.
    user.require("project:update")?;
    if store.get_project(id).await?.is_none() {
        return Err(ApiError::not_found("project"));
    }
    for (name, v) in [
        ("max_vms", body.max_vms),
        ("max_vcpus", body.max_vcpus),
        ("max_memory_mib", body.max_memory_mib),
        ("max_volumes", body.max_volumes),
        ("max_storage_bytes", body.max_storage_bytes),
    ] {
        if v.is_some_and(|v| v < 0) {
            return Err(ApiError::invalid(format!("{name} must not be negative")));
        }
    }
    store.set_quota(id, &body).await?;
    let usage = store.quota_usage(id).await?;
    Ok(Json(QuotaView {
        over_quota: over(&body, &usage),
        limits: body,
        usage,
    }))
}

/// `DELETE /projects/:id/quota` — back to unlimited.
pub async fn clear_quota(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("project:update")?;
    if store.clear_quota(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("quota"))
    }
}
