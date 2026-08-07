//! Network endpoints (design document, sections 14 and 18).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use serde::Deserialize;
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireNetworkUpdate};
use vquasar_model::{NetworkKind, SegmentKey};

use crate::ipam::Subnet;
use crate::store::{Network, NetworkIpam, Store};

#[derive(Debug, Deserialize)]
pub struct CreateNetwork {
    pub name: String,
    /// What this network is (design §18): `provider` | `vlan` | `tenant`.
    /// Defaults to `provider` so pre-kind clients keep working; `overlay: true`
    /// is still accepted and means `tenant`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Uplink a physical network attaches to. Defaults to `default`.
    #[serde(default)]
    pub physical_network: Option<String>,
    /// 802.1Q VLAN tag (1–4094). Only for `kind = "vlan"`, and only from the
    /// configured allowlist — a tag is a fact about the physical switch.
    #[serde(default)]
    pub vlan: Option<i32>,
    /// Deprecated spelling of `kind = "tenant"`.
    #[serde(default)]
    pub overlay: bool,
    /// Rejected: VNIs are allocated by the control plane (ADR-016). Accepted in
    /// the body only so a stale client gets a clear error instead of silently
    /// landing on someone else's overlay.
    #[serde(default)]
    pub vni: Option<i32>,
    /// IPAM config (design M13a). Absent/empty families ⇒ external DHCP.
    #[serde(flatten, default)]
    pub ipam: NetworkIpam,
}

/// Resolve the requested kind: explicit `kind`, else the legacy `overlay` flag,
/// else `provider`.
fn resolve_kind(body: &CreateNetwork) -> ApiResult<NetworkKind> {
    match body.kind.as_deref() {
        Some(k) => k
            .parse::<NetworkKind>()
            .map_err(|e| ApiError::invalid(e.to_string())),
        None if body.overlay => Ok(NetworkKind::Tenant),
        None => Ok(NetworkKind::Provider),
    }
}

/// Check the request's segment fields against the kind, and against what the
/// platform permits. A VLAN tag must match the physical switch, so it is
/// allow-listed rather than free-form; a VNI is never caller-supplied.
fn validate_request(
    kind: NetworkKind,
    body: &CreateNetwork,
    policy: &crate::config::NetworkPolicy,
) -> ApiResult<()> {
    if body.vni.is_some() {
        return Err(ApiError::invalid(
            "vni is allocated by the control plane and cannot be requested",
        ));
    }
    match kind {
        NetworkKind::Vlan => {
            let tag = body
                .vlan
                .ok_or_else(|| ApiError::invalid("a vlan network requires a vlan tag"))?;
            if !policy.permits_vlan(tag) {
                return Err(ApiError::invalid(format!(
                    "vlan {tag} is not in the permitted range ({})",
                    policy.describe_vlans()
                )));
            }
        }
        NetworkKind::Provider | NetworkKind::Tenant if body.vlan.is_some() => {
            return Err(ApiError::invalid(format!(
                "a {} network must not carry a vlan tag",
                kind.as_str()
            )));
        }
        _ => {}
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
    user: AuthUser,
    Json(body): Json<CreateNetwork>,
) -> ApiResult<(StatusCode, Json<Network>)> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    let kind = resolve_kind(&body)?;
    // Attaching to physical infrastructure is a platform decision: the tag and
    // the uplink are facts about the switch (ADR-016).
    if kind.is_platform_only() {
        user.require("network:create:provider")?;
    } else {
        user.require("network:create")?;
    }
    let policy = store.network_policy();
    validate_request(kind, &body, policy)?;
    validate_ipam(&body.ipam)?;

    let uplink = body.physical_network.as_deref().unwrap_or("default");
    if kind.is_physical() && !policy.permits_uplink(uplink) {
        return Err(ApiError::invalid(format!(
            "unknown physical_network {uplink:?} — configure it in [network] physical_networks"
        )));
    }

    let (segment, vni) = match kind {
        NetworkKind::Tenant => {
            let mut tx = store.begin().await?;
            let key = crate::segments::allocate_vxlan(&mut tx, &policy.segments())
                .await
                .map_err(|e| match e {
                    crate::segments::SegmentError::Exhausted { .. } => {
                        ApiError::invalid(e.to_string())
                    }
                    crate::segments::SegmentError::Db(e) => e.into(),
                })?;
            tx.commit().await?;
            let vni = match &key {
                SegmentKey::Vxlan { vni } => *vni as i32,
                _ => unreachable!("allocate_vxlan returns a vxlan key"),
            };
            (Some(key), Some(vni))
        }
        NetworkKind::Provider | NetworkKind::Vlan => (
            Some(SegmentKey::Physical {
                physical_network: uplink.to_string(),
                vlan: body.vlan.map(|v| v as u16),
            }),
            None,
        ),
    };
    let segment_key = segment.as_ref().map(|s| s.canonical());

    let net = store
        .insert_network(
            &body.name,
            kind.as_str(),
            kind.is_physical().then_some(uplink),
            segment_key.as_deref(),
            body.vlan,
            vni,
            &body.ipam,
        )
        .await
        .map_err(duplicate_segment)?;

    // Every network carries a policy from the moment it exists (ADR-017).
    let sg = store
        .insert_default_group(net.id, &net.name, kind == NetworkKind::Tenant)
        .await?;
    store.set_network_default_group(net.id, sg).await?;
    if let Some(key) = &segment_key {
        let mut tx = store.begin().await?;
        let _ = crate::segments::bind(&mut tx, key, net.id).await;
        tx.commit().await?;
    }
    let net = store.get_network(net.id).await?.unwrap_or(net);
    Ok((StatusCode::CREATED, Json(net)))
}

/// A segment collision means another network already owns that L2 domain.
fn duplicate_segment(e: sqlx::Error) -> ApiError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::invalid(
            "that segment is already in use by another network (one network = one L2 domain)",
        ),
        _ => e.into(),
    }
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
    validate_ipam(&body.ipam)?;
    let existing = store
        .get_network(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("network not found: {id}")))?;
    // A network's segment is its identity: changing it would silently move
    // every attached VM to a different broadcast domain. Retarget the NICs
    // instead (PUT /vms/:id/nics/:index).
    if body.vni.is_some() || (body.vlan.is_some() && body.vlan != existing.vlan) {
        return Err(ApiError::invalid(
            "a network's segment cannot be changed after creation",
        ));
    }
    store
        .update_network(id, &body.name, existing.vlan, existing.vni, &body.ipam)
        .await
        .map_err(duplicate_segment)?
        .map(Json)
        .ok_or_else(|| ApiError::invalid(format!("network not found: {id}")))
}

pub async fn list(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
) -> ApiResult<Json<Vec<Network>>> {
    user.require("network:read")?;
    Ok(Json(
        crate::scoped::ScopedStore::new(store, scope.0)
            .list_networks()
            .await?,
    ))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    scope: crate::authz::RequestScope,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Network>> {
    user.require("network:read")?;
    crate::scoped::ScopedStore::new(store, scope.0)
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

/// Body for `POST /networks/:id/adopt-segment`.
#[derive(Debug, Deserialize)]
pub struct AdoptSegment {
    /// Uplink the network attaches to. Defaults to the one it already has, or
    /// `default`.
    #[serde(default)]
    pub physical_network: Option<String>,
    /// 802.1Q tag to adopt. Omit to keep the network untagged.
    #[serde(default)]
    pub vlan: Option<i32>,
    /// Required when the tag differs from what the network uses today, because
    /// that re-tags every attached NIC and changes what they can reach.
    #[serde(default)]
    pub retag_ok: bool,
}

/// `POST /networks/:id/adopt-segment` — give a grandfathered network a real
/// segment identity (design §18, ADR-016).
///
/// Networks that predate the kind model carry `segment_key = NULL`: they are
/// excluded from the uniqueness constraint, so nothing guarantees they are not
/// sharing a broadcast domain with another network. Adopting a segment records
/// which one they occupy.
///
/// Adopting the segment a network *already* occupies changes no packets — it
/// only writes down what is true. Adopting a different tag does re-tag every
/// attached NIC, so that needs `retag_ok`.
///
/// This is deliberately not `PATCH /networks/:id`: a network *with* a segment
/// may never change it, because that would move every attached VM to a
/// different broadcast domain. Adoption is only ever NULL → set.
pub async fn adopt_segment(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<AdoptSegment>,
) -> ApiResult<Json<Network>> {
    let net = store
        .get_network(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("network not found: {id}")))?;

    let kind: NetworkKind =
        net.kind
            .parse()
            .map_err(|e: vquasar_model::network::InvalidNetworkKind| {
                ApiError::internal(e.to_string())
            })?;
    if kind.is_platform_only() {
        user.require("network:create:provider")?;
    } else {
        user.require("network:update")?;
    }

    if net.segment_key.is_some() {
        return Err(ApiError::invalid(
            "this network already has a segment, and a segment cannot be changed: \
             every attached VM would move to a different broadcast domain. Retarget \
             the NICs instead (PUT /vms/:id/nics/:index)",
        ));
    }

    let policy = store.network_policy();
    let uplink = body
        .physical_network
        .clone()
        .or_else(|| net.physical_network.clone())
        .unwrap_or_else(|| "default".to_string());
    if !policy.permits_uplink(&uplink) {
        return Err(ApiError::invalid(format!(
            "unknown physical_network {uplink:?} — configure it in [network] physical_networks"
        )));
    }
    if let Some(tag) = body.vlan {
        if !policy.permits_vlan(tag) {
            return Err(ApiError::invalid(format!(
                "vlan {tag} is not in the permitted range ({})",
                policy.describe_vlans()
            )));
        }
    }
    if body.vlan != net.vlan && !body.retag_ok {
        return Err(ApiError::invalid(
            "adopting a different vlan re-tags every NIC on this network and changes \
             what they can reach; pass retag_ok=true to confirm",
        ));
    }

    let key = SegmentKey::Physical {
        physical_network: uplink.clone(),
        vlan: body.vlan.map(|v| v as u16),
    }
    .canonical();

    store
        .adopt_network_segment(id, &uplink, body.vlan, &key)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(db) if db.is_unique_violation() => ApiError::invalid(format!(
                "segment {key} is already taken by another network — they are the same \
                 broadcast domain, so only one of them can describe it. Consolidate them \
                 (retarget the NICs and delete the spare), or adopt a distinct vlan"
            )),
            _ => e.into(),
        })?
        .map(Json)
        .ok_or_else(|| ApiError::invalid("network already has a segment"))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    user.require("network:delete")?;
    // Quarantine the segment rather than freeing it: a host may still carry the
    // overlay bridge and its tunnel mesh (ADR-016).
    let segment = store.get_network(id).await?.and_then(|n| n.segment_key);
    if store.delete_network(id).await? {
        if let Some(key) = segment {
            if let Err(e) = crate::segments::release(store.pool(), &key).await {
                tracing::warn!(error = %e, segment = %key, "failed to quarantine segment");
            }
        }
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::invalid(format!("network not found: {id}")))
    }
}
