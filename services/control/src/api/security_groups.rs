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
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<SecurityGroupView>>> {
    user.require("network:read")?;
    let mut out = Vec::new();
    let scoped = crate::scoped::ScopedStore::new(store.clone(), scope.0);
    for g in scoped.list_security_groups().await? {
        let rules = store.list_sg_rules(g.id).await?;
        out.push(SecurityGroupView { group: g, rules });
    }
    Ok(Json(out))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<SecurityGroupView>> {
    user.require("network:read")?;
    let group = crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .get_security_group(id)
        .await?
        .ok_or_else(|| ApiError::not_found("security group"))?;
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
    scope: crate::authz::RequestScope,
    _: RequireNetworkCreate,
    Json(body): Json<CreateGroup>,
) -> ApiResult<(StatusCode, Json<SecurityGroup>)> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    let g = store
        .create_security_group(
            body.name.trim(),
            body.description.as_deref(),
            crate::scoped::ScopedStore::new(store.clone(), scope.0).owning_project(),
        )
        .await?;
    Ok((StatusCode::CREATED, Json(g)))
}

pub async fn update(
    State(store): State<Store>,
    _: RequireNetworkUpdate,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateGroup>,
) -> ApiResult<Json<SecurityGroup>> {
    if body.name.trim().is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .security_groups_in_scope(&[id])
        .await?
    {
        return Err(ApiError::not_found("security group"));
    }
    store
        .update_security_group(id, body.name.trim(), body.description.as_deref())
        .await?
        .map(Json)
        .ok_or(ApiError::not_found("security group"))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("network:delete")?;
    // Scope first: the "managed group" refusal below is information about the
    // group, and a caller who cannot see it must not learn that much.
    let scoped = crate::scoped::ScopedStore::new(store.clone(), scope.0);
    if !scoped.security_groups_in_scope(&[id]).await? {
        return Err(ApiError::not_found("security group"));
    }
    // A managed group is a network's policy object, not a user-created one:
    // deleting it would leave that network with no default and every NIC on it
    // silently unpoliced (ADR-017). Delete or re-default the network instead.
    if store.security_group_is_managed(id).await? {
        return Err(ApiError::invalid(
            "this is a network's default policy group and cannot be deleted on its own; \
             delete the network, or point it at a different default",
        ));
    }
    if scoped.delete_security_group(id).await? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("security group"))
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
    /// The remote is every member of this group, resolved to their addresses
    /// on each reconcile tick (design §18). Mutually exclusive with
    /// `remote_cidr`, and the reason a rule survives a VM being replaced.
    #[serde(default)]
    pub remote_group_id: Option<Uuid>,
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
    // Two remotes are two different answers to "who is the other end", and the
    // rule would quietly mean whichever the resolver read first (design §18).
    if r.remote_group_id.is_some()
        && r.remote_cidr
            .as_deref()
            .is_some_and(|c| !c.trim().is_empty())
    {
        return Err(ApiError::invalid(
            "a rule names either a remote_cidr or a remote_group_id, not both",
        ));
    }
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
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateRule>,
) -> ApiResult<(StatusCode, Json<SecurityGroupRule>)> {
    // A rule carries no project of its own; it inherits the group's, so the
    // group is what has to be in scope (design §47).
    if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .security_groups_in_scope(&[id])
        .await?
    {
        return Err(ApiError::not_found("security group"));
    }
    validate_rule(&body)?;
    // Under default-allow egress an egress rule permits what is already
    // permitted — it changes nothing, forever, silently. Accepting it would put
    // a rule in the console that reads like a control and is not one, which is
    // worse than refusing (design §18).
    if body.direction == "egress" && !store.network_policy().egress_enforced() {
        return Err(ApiError::invalid(
            "egress rules are not enforced on this cluster: egress is default-allow, \
             so this rule would do nothing. Set [network] egress_mode = \"enforced\" first.",
        ));
    }
    let cidr = body
        .remote_cidr
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty());
    // A remote group has to be one this caller can see, or a rule becomes a way
    // to learn that another project's group exists (design §47).
    if let Some(remote) = body.remote_group_id {
        if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
            .security_groups_in_scope(&[remote])
            .await?
        {
            return Err(ApiError::not_found("security group"));
        }
    }
    let rule = store
        .add_sg_rule(
            id,
            &body.direction,
            &body.ethertype,
            &body.protocol,
            body.port_min,
            body.port_max,
            body.remote_group_id,
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
    scope: crate::authz::RequestScope,
    Path((id, rule_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    user.require("network:update")?;
    if !crate::scoped::ScopedStore::new(store.clone(), scope.0)
        .security_groups_in_scope(&[id])
        .await?
    {
        return Err(ApiError::not_found("security group"));
    }
    if store.delete_sg_rule(id, rule_id).await? {
        store.touch_vms_using_security_group(id).await?;
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid("rule not found"))
    }
}
