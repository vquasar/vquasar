//! Task endpoints (design document, section 15).

use axum::extract::{Path, State};
use axum::Json;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::AuthUser;
use crate::store::{Store, Task};

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Task>>> {
    user.require("vm:read")?;
    Ok(Json(store.list_tasks().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Task>> {
    user.require("vm:read")?;
    store
        .get_task(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::task_not_found(id))
}
