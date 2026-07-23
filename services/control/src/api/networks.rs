//! Network endpoints (design document, sections 14 and 18).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::store::{Network, Store};

#[derive(Debug, Deserialize)]
pub struct CreateNetwork {
    pub name: String,
    /// Optional 802.1Q VLAN tag (1–4094); omit for a flat provider network.
    #[serde(default)]
    pub vlan: Option<i32>,
}

pub async fn create(
    State(store): State<Store>,
    Json(body): Json<CreateNetwork>,
) -> ApiResult<(StatusCode, Json<Network>)> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if let Some(vlan) = body.vlan {
        if !(1..=4094).contains(&vlan) {
            return Err(ApiError::invalid("vlan must be between 1 and 4094"));
        }
    }
    let net = store.insert_network(&body.name, body.vlan).await?;
    Ok((StatusCode::CREATED, Json(net)))
}

pub async fn list(State(store): State<Store>) -> ApiResult<Json<Vec<Network>>> {
    Ok(Json(store.list_networks().await?))
}

pub async fn get(State(store): State<Store>, Path(id): Path<Uuid>) -> ApiResult<Json<Network>> {
    store
        .get_network(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("network not found: {id}")))
}

pub async fn delete(State(store): State<Store>, Path(id): Path<Uuid>) -> ApiResult<StatusCode> {
    if store.delete_network(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("network not found: {id}")))
    }
}
