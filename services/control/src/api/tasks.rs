//! Task endpoints (design document, section 15).

use axum::extract::{Path, State};
use axum::Json;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::AuthUser;
use crate::store::{Store, Task};

pub async fn list(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<Task>>> {
    user.require("vm:read")?;
    Ok(Json(
        crate::scoped::ScopedStore::new(store, scope.0)
            .list_tasks()
            .await?,
    ))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Task>> {
    user.require("vm:read")?;
    crate::scoped::ScopedStore::new(store, scope.0)
        .get_task(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::task_not_found(id))
}
