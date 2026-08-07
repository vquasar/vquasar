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
    Ok(Json(store.list_projects().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Project>> {
    user.require("project:read")?;
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
