//! Virtual networks and what their isolation actually means (design §18).
//!
//! A network used to be an IPAM record: two networks with no VLAN and no VNI
//! were the *same* untagged L2 domain on the shared integration bridge, so
//! "network" carried no isolation meaning at all. [`NetworkKind`] fixes that by
//! making each network say what it is — and, just as importantly, by refusing to
//! claim isolation where the dataplane cannot enforce it. A provider network
//! bridged to the physical LAN is a legitimate thing to want; pretending it is
//! isolated would be a lie encoded in the schema.

use serde::{Deserialize, Serialize};

/// What kind of segment a network is, and therefore what it guarantees.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkKind {
    /// Attached to physical infrastructure, untagged. Platform-created.
    ///
    /// Guarantees nothing by itself: its security is the physical network's.
    #[default]
    Provider,
    /// Attached to physical infrastructure with an 802.1Q tag. Platform-created,
    /// and the tag must match what the switch actually trunks.
    ///
    /// Isolated only to the extent the physical network honours the tag.
    Vlan,
    /// A VXLAN overlay with a platform-allocated VNI.
    ///
    /// The only kind that is self-contained: a distinct L2 broadcast domain,
    /// disjoint from every other tenant network and from the physical segments.
    Tenant,
}

impl NetworkKind {
    /// Whether this kind attaches to physical infrastructure, and so is only as
    /// isolated as that infrastructure is.
    pub fn is_physical(self) -> bool {
        matches!(self, NetworkKind::Provider | NetworkKind::Vlan)
    }

    /// Whether creating one is restricted to platform administrators.
    ///
    /// Physical attachment is a platform concern: a VLAN tag is a fact about
    /// the switch, and picking one is picking which provider segment you land
    /// on. Tenant networks are self-contained and safe to delegate.
    pub fn is_platform_only(self) -> bool {
        self.is_physical()
    }

    /// A one-line statement of what this kind isolates, for the API and UI.
    ///
    /// Deliberately part of the model: the guarantee is a property of the kind,
    /// and callers should not have to infer it from whether a VLAN is set.
    pub fn isolation_guarantee(self) -> &'static str {
        match self {
            NetworkKind::Provider => {
                "None. Bridged to physical infrastructure; its security is the physical network's."
            }
            NetworkKind::Vlan => "Only as far as the physical network honours the 802.1Q tag.",
            NetworkKind::Tenant => "A distinct L2 domain, disjoint from every other network.",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NetworkKind::Provider => "provider",
            NetworkKind::Vlan => "vlan",
            NetworkKind::Tenant => "tenant",
        }
    }
}

impl std::str::FromStr for NetworkKind {
    type Err = InvalidNetworkKind;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "provider" => Ok(NetworkKind::Provider),
            "vlan" => Ok(NetworkKind::Vlan),
            "tenant" => Ok(NetworkKind::Tenant),
            other => Err(InvalidNetworkKind(other.to_string())),
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown network kind {0:?} — expected provider, vlan or tenant")]
pub struct InvalidNetworkKind(pub String);

/// The L2 segment a network occupies.
///
/// Two networks with the same [`SegmentKey`] are the same broadcast domain.
/// Making that a value — and a unique index in the database — is what stops
/// "network" from silently meaning "IPAM record" again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentKey {
    /// An uplink, optionally tagged. `None` tag is the untagged domain.
    Physical {
        physical_network: String,
        vlan: Option<u16>,
    },
    /// A VXLAN VNI, unique fleet-wide.
    Vxlan { vni: u32 },
}

impl SegmentKey {
    /// The canonical string stored in `networks.segment_key` and uniquely
    /// indexed. Networks predating the kind model store `NULL` instead and are
    /// excluded from the index — see [`crate::network`] docs and ADR-016.
    pub fn canonical(&self) -> String {
        match self {
            SegmentKey::Physical {
                physical_network,
                vlan,
            } => match vlan {
                Some(v) => format!("{physical_network}:{v}"),
                None => format!("{physical_network}:untagged"),
            },
            SegmentKey::Vxlan { vni } => format!("vxlan:{vni}"),
        }
    }
}

/// Why a proposed network is not coherent.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum NetworkValidationError {
    #[error("a {kind} network must not set a VLAN tag")]
    UnexpectedVlan { kind: &'static str },
    #[error("a vlan network requires a vlan tag")]
    MissingVlan,
    #[error("vlan must be between 1 and 4094, got {0}")]
    VlanOutOfRange(i64),
    #[error("a {kind} network must not carry a VNI")]
    UnexpectedVni { kind: &'static str },
    #[error("vni must be between 1 and 16777215, got {0}")]
    VniOutOfRange(i64),
    #[error("a physical network requires a physical_network (uplink) name")]
    MissingPhysicalNetwork,
}

/// Check that a kind and its segment fields agree.
///
/// The VNI is *not* caller-supplied — the control plane allocates it — so this
/// validates a resolved network, not a request body.
pub fn validate_segment(
    kind: NetworkKind,
    physical_network: Option<&str>,
    vlan: Option<i64>,
    vni: Option<i64>,
) -> Result<(), NetworkValidationError> {
    match kind {
        NetworkKind::Provider => {
            if vlan.is_some() {
                return Err(NetworkValidationError::UnexpectedVlan { kind: "provider" });
            }
        }
        NetworkKind::Vlan => {
            let v = vlan.ok_or(NetworkValidationError::MissingVlan)?;
            if !(1..=4094).contains(&v) {
                return Err(NetworkValidationError::VlanOutOfRange(v));
            }
        }
        NetworkKind::Tenant => {
            if vlan.is_some() {
                return Err(NetworkValidationError::UnexpectedVlan { kind: "tenant" });
            }
        }
    }
    if kind.is_physical() {
        if vni.is_some() {
            return Err(NetworkValidationError::UnexpectedVni {
                kind: kind.as_str(),
            });
        }
        if physical_network.is_none_or(str::is_empty) {
            return Err(NetworkValidationError::MissingPhysicalNetwork);
        }
    }
    if let Some(v) = vni {
        if !(1..=16_777_215).contains(&v) {
            return Err(NetworkValidationError::VniOutOfRange(v));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_tenant_claims_isolation() {
        assert!(NetworkKind::Tenant
            .isolation_guarantee()
            .contains("disjoint"));
        // The point of the type: a provider network says so out loud.
        assert!(NetworkKind::Provider
            .isolation_guarantee()
            .starts_with("None"));
        assert!(!NetworkKind::Tenant.is_physical());
        assert!(NetworkKind::Provider.is_physical() && NetworkKind::Vlan.is_physical());
    }

    #[test]
    fn physical_kinds_are_platform_only() {
        assert!(NetworkKind::Provider.is_platform_only());
        assert!(NetworkKind::Vlan.is_platform_only());
        // Tenant networks are self-contained, so they can be delegated.
        assert!(!NetworkKind::Tenant.is_platform_only());
    }

    #[test]
    fn segment_key_distinguishes_untagged_from_tagged_and_overlay() {
        let untagged = SegmentKey::Physical {
            physical_network: "default".into(),
            vlan: None,
        };
        let tagged = SegmentKey::Physical {
            physical_network: "default".into(),
            vlan: Some(100),
        };
        let other_uplink = SegmentKey::Physical {
            physical_network: "dmz".into(),
            vlan: None,
        };
        assert_eq!(untagged.canonical(), "default:untagged");
        assert_eq!(tagged.canonical(), "default:100");
        assert_ne!(untagged.canonical(), other_uplink.canonical());
        assert_eq!(SegmentKey::Vxlan { vni: 4096 }.canonical(), "vxlan:4096");
    }

    /// Two untagged provider networks on the same uplink are the same L2
    /// domain — the collision the unique index exists to prevent.
    #[test]
    fn same_uplink_untagged_collides() {
        let a = SegmentKey::Physical {
            physical_network: "default".into(),
            vlan: None,
        };
        let b = SegmentKey::Physical {
            physical_network: "default".into(),
            vlan: None,
        };
        assert_eq!(a.canonical(), b.canonical());
    }

    #[test]
    fn kind_round_trips_through_strings() {
        for k in [
            NetworkKind::Provider,
            NetworkKind::Vlan,
            NetworkKind::Tenant,
        ] {
            assert_eq!(k.as_str().parse::<NetworkKind>().unwrap(), k);
        }
        assert!("overlay".parse::<NetworkKind>().is_err());
    }

    #[test]
    fn segment_validation_rejects_incoherent_combinations() {
        use NetworkValidationError as E;
        // A vlan network without a tag is meaningless.
        assert_eq!(
            validate_segment(NetworkKind::Vlan, Some("default"), None, None),
            Err(E::MissingVlan)
        );
        // A tenant network is an overlay; a tag would put it on the wire.
        assert_eq!(
            validate_segment(NetworkKind::Tenant, None, Some(100), Some(4096)),
            Err(E::UnexpectedVlan { kind: "tenant" })
        );
        // A physical network cannot carry a VNI.
        assert_eq!(
            validate_segment(NetworkKind::Provider, Some("default"), None, Some(4096)),
            Err(E::UnexpectedVni { kind: "provider" })
        );
        // Physical kinds must name their uplink.
        assert_eq!(
            validate_segment(NetworkKind::Provider, None, None, None),
            Err(E::MissingPhysicalNetwork)
        );
        assert_eq!(
            validate_segment(NetworkKind::Vlan, Some("default"), Some(4095), None),
            Err(E::VlanOutOfRange(4095))
        );
        assert_eq!(
            validate_segment(NetworkKind::Tenant, None, None, Some(0)),
            Err(E::VniOutOfRange(0))
        );
    }

    #[test]
    fn coherent_combinations_pass() {
        assert!(validate_segment(NetworkKind::Provider, Some("default"), None, None).is_ok());
        assert!(validate_segment(NetworkKind::Vlan, Some("default"), Some(100), None).is_ok());
        assert!(validate_segment(NetworkKind::Tenant, None, None, Some(4096)).is_ok());
    }
}
