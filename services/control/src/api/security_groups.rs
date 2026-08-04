//! Security-group endpoints (design M13c). Groups and their rules are network
//! policy, so they reuse the `network:*` permissions.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireNetworkCreate, RequireNetworkUpdate};
use crate::store::{SecurityGroup, SecurityGroupRule, Store};

#[derive(Serialize)]
pub struct SecurityGroupView {
    #[serde(flatten)]
    pub group: SecurityGroup,
    pub rules: Vec<SecurityGroupRule>,
}

pub async fn list(
    State(store): State<Store>,
    user: AuthUser,
) -> ApiResult<Json<Vec<SecurityGroupView>>> {
    user.require("network:read")?;
    let mut out = Vec::new();
    for g in store.list_security_groups().await? {
        let rules = store.list_sg_rules(g.id).await?;
        out.push(SecurityGroupView { group: g, rules });
    }
    Ok(Json(out))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SecurityGroupView>> {
    user.require("network:read")?;
    let group = store
        .get_security_group(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("security group not found: {id}")))?;
    let rules = store.list_sg_rules(id).await?;
    Ok(Json(SecurityGroupView { group, rules }))
}

#[derive(Deserialize)]
pub struct CreateGroup {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

pub async fn create(
    State(store): State<Store>,
    _: RequireNetworkCreate,
    Json(body): Json<CreateGroup>,
) -> ApiResult<(StatusCode, Json<SecurityGroup>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    let g = store
        .create_security_group(body.name.trim(), body.description.as_deref())
        .await?;
    Ok((StatusCode::CREATED, Json(g)))
}

pub async fn update(
    State(store): State<Store>,
    _: RequireNetworkUpdate,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateGroup>,
) -> ApiResult<Json<SecurityGroup>> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    store
        .update_security_group(id, body.name.trim(), body.description.as_deref())
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("security group not found: {id}")))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("network:delete")?;
    if store.delete_security_group(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("security group not found: {id}")))
    }
}

#[derive(Deserialize)]
pub struct CreateRule {
    #[serde(default = "ingress")]
    pub direction: String,
    #[serde(default = "ipv4")]
    pub ethertype: String,
    #[serde(default = "any")]
    pub protocol: String,
    #[serde(default)]
    pub port_min: Option<i32>,
    #[serde(default)]
    pub port_max: Option<i32>,
    #[serde(default)]
    pub remote_cidr: Option<String>,
}

fn ingress() -> String {
    "ingress".into()
}
fn ipv4() -> String {
    "IPv4".into()
}
fn any() -> String {
    "any".into()
}

fn validate_rule(r: &CreateRule) -> ApiResult<()> {
    if !matches!(r.direction.as_str(), "ingress" | "egress") {
        return Err(ApiError::invalid("direction must be ingress or egress"));
    }
    if !matches!(r.ethertype.as_str(), "IPv4" | "IPv6") {
        return Err(ApiError::invalid("ethertype must be IPv4 or IPv6"));
    }
    if !matches!(r.protocol.as_str(), "tcp" | "udp" | "icmp" | "any") {
        return Err(ApiError::invalid("protocol must be tcp, udp, icmp or any"));
    }
    let has_ports = matches!(r.protocol.as_str(), "tcp" | "udp");
    for p in [r.port_min, r.port_max].into_iter().flatten() {
        if !has_ports {
            return Err(ApiError::invalid("ports only apply to tcp/udp"));
        }
        if !(0..=65535).contains(&p) {
            return Err(ApiError::invalid("port must be 0..65535"));
        }
    }
    if let (Some(lo), Some(hi)) = (r.port_min, r.port_max) {
        if lo > hi {
            return Err(ApiError::invalid("port_min must be ≤ port_max"));
        }
    }
    if let Some(cidr) = r.remote_cidr.as_deref().filter(|c| !c.trim().is_empty()) {
        if cidr.trim().parse::<ipnet::IpNet>().is_err() {
            return Err(ApiError::invalid(format!("invalid remote_cidr: {cidr}")));
        }
    }
    Ok(())
}

pub async fn add_rule(
    State(store): State<Store>,
    _: RequireNetworkUpdate,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateRule>,
) -> ApiResult<(StatusCode, Json<SecurityGroupRule>)> {
    store
        .get_security_group(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("security group not found: {id}")))?;
    validate_rule(&body)?;
    let cidr = body
        .remote_cidr
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    let rule = store
        .add_sg_rule(
            id,
            &body.direction,
            &body.ethertype,
            &body.protocol,
            body.port_min,
            body.port_max,
            cidr,
        )
        .await?;
    // Re-apply the firewall to any running VMs using this group (M13c).
    store.touch_vms_using_security_group(id).await?;
    Ok((StatusCode::CREATED, Json(rule)))
}

pub async fn delete_rule(
    State(store): State<Store>,
    user: AuthUser,
    Path((id, rule_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    user.require("network:update")?;
    if store.delete_sg_rule(id, rule_id).await? {
        store.touch_vms_using_security_group(id).await?;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid("rule not found"))
    }
}
