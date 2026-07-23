//! Host endpoints (design document, section 14).

use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::store::{Host, Store};

/// Register a host by name + agent gRPC endpoint. The host controller then
/// polls it and flips it to `Ready` (design section 13: manual dev enrollment).
#[derive(Debug, Deserialize)]
pub struct RegisterHost {
    pub name: String,
    pub endpoint: String,
}

pub async fn register(
    State(store): State<Store>,
    Json(body): Json<RegisterHost>,
) -> ApiResult<(axum::http::StatusCode, Json<Host>)> {
    if body.name.is_empty() || body.endpoint.is_empty() {
        return Err(ApiError::invalid("name and endpoint are required"));
    }
    let host = store.register_host(&body.name, &body.endpoint).await?;
    Ok((axum::http::StatusCode::CREATED, Json(host)))
}

pub async fn list(State(store): State<Store>) -> ApiResult<Json<Vec<Host>>> {
    Ok(Json(store.list_hosts().await?))
}

pub async fn get(State(store): State<Store>, Path(id): Path<Uuid>) -> ApiResult<Json<Host>> {
    store
        .get_host(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::host_not_found(id))
}
