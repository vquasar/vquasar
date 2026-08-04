//! Network endpoints (design document, sections 14 and 18).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireNetworkCreate, RequireNetworkUpdate};
use crate::ipam::Subnet;
use crate::store::{Network, NetworkIpam, Store};

#[derive(Debug, Deserialize)]
pub struct CreateNetwork {
    pub name: String,
    /// Optional 802.1Q VLAN tag (1–4094); omit for a flat provider network.
    #[serde(default)]
    pub vlan: Option<i32>,
    /// IPAM config (design M13a). Absent/empty families ⇒ external DHCP.
    #[serde(flatten, default)]
    pub ipam: NetworkIpam,
}

fn validate_vlan(vlan: Option<i32>) -> ApiResult<()> {
    if let Some(v) = vlan {
        if !(1..=4094).contains(&v) {
            return Err(ApiError::invalid("vlan must be between 1 and 4094"));
        }
    }
    Ok(())
}

/// Validate the IPAM config: subnets/gateways parse, gateways sit inside their
/// subnet and match its family, and DNS entries are IPs.
fn validate_ipam(ipam: &NetworkIpam) -> ApiResult<()> {
    let map = |e: crate::ipam::IpamError| ApiError::invalid(e.to_string());
    for (cidr, gw, ps, pe) in [
        (
            ipam.cidr_v4.as_deref(),
            ipam.gateway_v4.as_deref(),
            ipam.pool_v4_start.as_deref(),
            ipam.pool_v4_end.as_deref(),
        ),
        (
            ipam.cidr_v6.as_deref(),
            ipam.gateway_v6.as_deref(),
            ipam.pool_v6_start.as_deref(),
            ipam.pool_v6_end.as_deref(),
        ),
    ] {
        let Some(subnet) = Subnet::parse_opt(cidr, gw, ps, pe).map_err(map)? else {
            continue;
        };
        if let Some(gw) = subnet.gateway {
            if subnet.is_v6() != gw.is_ipv6() || !subnet.net.contains(&gw) {
                return Err(ApiError::invalid(format!(
                    "gateway {gw} is not inside {}",
                    subnet.net
                )));
            }
        }
    }
    for d in &ipam.dns {
        if d.trim().parse::<std::net::IpAddr>().is_err() {
            return Err(ApiError::invalid(format!("dns entry is not an IP: {d}")));
        }
    }
    Ok(())
}

pub async fn create(
    State(store): State<Store>,
    _: RequireNetworkCreate,
    Json(body): Json<CreateNetwork>,
) -> ApiResult<(StatusCode, Json<Network>)> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    validate_vlan(body.vlan)?;
    validate_ipam(&body.ipam)?;
    let net = store
        .insert_network(&body.name, body.vlan, &body.ipam)
        .await?;
    Ok((StatusCode::CREATED, Json(net)))
}

pub async fn update(
    State(store): State<Store>,
    _: RequireNetworkUpdate,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateNetwork>,
) -> ApiResult<Json<Network>> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    validate_vlan(body.vlan)?;
    validate_ipam(&body.ipam)?;
    store
        .update_network(id, &body.name, body.vlan, &body.ipam)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("network not found: {id}")))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Network>>> {
    user.require("network:read")?;
    Ok(Json(store.list_networks().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Network>> {
    user.require("network:read")?;
    store
        .get_network(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("network not found: {id}")))
}

/// IP allocations in a network (design M13a).
pub async fn allocations(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vec<crate::store::IpAllocation>>> {
    user.require("network:read")?;
    Ok(Json(store.allocations_for_network(id).await?))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("network:delete")?;
    if store.delete_network(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("network not found: {id}")))
    }
}
