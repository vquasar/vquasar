//! Event endpoints (design document, section 16).

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;

use crate::api::error::ApiResult;
use crate::authz::AuthUser;
use crate::store::{Event, Store};

#[derive(Debug, Deserialize)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    100
}

pub async fn list(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Query(params): Query<ListParams>,
) -> ApiResult<Json<Vec<Event>>> {
    user.require("vm:read")?;
    let limit = params.limit.clamp(1, 1000);
    Ok(Json(
        crate::scoped::ScopedStore::new(store, scope.0)
            .list_events(limit)
            .await?,
    ))
}
