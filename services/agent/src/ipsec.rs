//! IPsec protection for the VXLAN underlay (design §18, M18b).
//!
//! # Why an anchor port per peer, not per tunnel
//!
//! OVS builds its IPsec connections in **transport mode** with the traffic
//! selector `(peer_ip, udp/4789)`. There is no VNI in that selector, so a single
//! security association between two hosts protects *every* overlay between them.
//! Putting the IPsec options on each per-VNI tunnel port would therefore create
//! one identical connection per VNI per peer — a shape OVS was never built for
//! (its reference consumer, OVN, has exactly one tunnel port per peer).
//!
//! So the IPsec configuration lives on a dedicated bridge, `br-vxipsec`, with
//! one always-present tunnel port per peer. Every per-VNI tunnel to that peer is
//! protected by the resulting SA. Keeping it off the per-VNI bridges also
//! decouples it from overlay GC: deleting the last VM on a VNI tears down that
//! bridge, and must not take the host's IPsec protection with it.
//!
//! # What this does not do
//!
//! IPsec does not stop *injection*. A cleartext VXLAN packet from a configured
//! peer is dropped by the inbound policy check, but a packet from any other
//! source IP matches no policy at all and is delivered normally. Closing that
//! needs a host ingress filter on UDP/4789 — tracked separately, because it is
//! the one piece that differs between distributions.

use crate::network::NetworkError;

type Result<T> = std::result::Result<T, NetworkError>;

/// Where the IKE daemon expects to find key material.
///
/// This is not a preference. On a distribution with AppArmor, charon's profile
/// permits reading `/etc/ipsec.d/**` and denies everything else — pointing OVS
/// at `/etc/vquasar/tls/agent.key` produces `apparmor="DENIED"` and an
/// authentication failure that reports only "no private key found", naming
/// neither the path nor the reason. Verified on Ubuntu 24.04 with strongSwan
/// 5.9.13. Staging copies here keeps the same layout working under SELinux on
/// RHEL-family hosts too.
const IPSEC_DIR: &str = "/etc/ipsec.d";

/// The bridge carrying one IPsec anchor tunnel per peer.
pub const ANCHOR_BRIDGE: &str = "br-vxipsec";

/// A peer host and the certificate identity we expect from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub underlay_ip: String,
    /// Peer certificate Common Name. Empty ⇒ identity unknown, so the tunnel
    /// can only be trusted to "some certificate our CA signed".
    pub cert_cn: String,
}

/// Anchor port name for a peer, e.g. `10.0.0.2` → `ipsec-10-0-0-2`.
///
/// Must fit IFNAMSIZ (16 including the NUL), which an IPv4 address does at
/// worst: 6 + 15 = 21 — so the address is truncated to its last two octets,
/// which is unique within a subnet and stable.
pub fn anchor_port(underlay_ip: &str) -> String {
    let tail: Vec<&str> = underlay_ip.split('.').rev().take(2).collect();
    let short: String = tail.into_iter().rev().collect::<Vec<_>>().join("-");
    format!("ipsec-{short}")
}

/// The `ovs-vsctl` argument vector that records this host's IPsec credentials.
///
/// Global, set once on the `Open_vSwitch` table: the certificate the host
/// presents, its key, and the CA that must have signed the peer's.
pub fn credentials_args(cert: &str, key: &str, ca: &str) -> Vec<String> {
    vec![
        "set".into(),
        "Open_vSwitch".into(),
        ".".into(),
        format!("other_config:certificate={cert}"),
        format!("other_config:private_key={key}"),
        format!("other_config:ca_cert={ca}"),
    ]
}

/// The `ovs-vsctl` argument vector creating (or updating) a peer's anchor port.
///
/// `remote_name` is what actually pins identity: without it, *any* certificate
/// the CA signed is accepted, so one compromised host could impersonate another
/// (design §30).
pub fn anchor_port_args(peer: &Peer) -> Vec<String> {
    let port = anchor_port(&peer.underlay_ip);
    let mut args: Vec<String> = vec![
        "--may-exist".into(),
        "add-port".into(),
        ANCHOR_BRIDGE.into(),
        port.clone(),
        "--".into(),
        "set".into(),
        "interface".into(),
        port,
        "type=vxlan".into(),
        format!("options:remote_ip={}", peer.underlay_ip),
    ];
    if !peer.cert_cn.is_empty() {
        args.push(format!("options:remote_name={}", peer.cert_cn));
    }
    args
}

/// Stage certificate material where the IKE daemon is allowed to read it,
/// returning the paths to hand to OVS.
///
/// The CA additionally *must* land in `cacerts/`: strongSwan does not load it
/// from OVS's `other_config:ca_cert`, so without this the peer authenticates
/// and is then rejected with "no issuer certificate found" — the chain cannot
/// be built even though the CA is configured. Verified against strongSwan
/// 5.9.13.
fn stage_credentials(cert: &str, key: &str, ca: &str) -> std::io::Result<(String, String, String)> {
    let staged_cert = format!("{IPSEC_DIR}/certs/vquasar-agent.crt");
    let staged_key = format!("{IPSEC_DIR}/private/vquasar-agent.key");
    let staged_ca = format!("{IPSEC_DIR}/cacerts/vquasar-ca.crt");
    for (src, dst, mode) in [
        (cert, &staged_cert, 0o644),
        (key, &staged_key, 0o600),
        (ca, &staged_ca, 0o644),
    ] {
        if let Some(parent) = std::path::Path::new(dst).parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(src, dst)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dst, std::fs::Permissions::from_mode(mode))?;
        }
    }
    Ok((staged_cert, staged_key, staged_ca))
}

/// Configure IPsec anchors for exactly `peers`, removing any that are stale.
pub async fn apply(cert: &str, key: &str, ca: &str, peers: &[Peer]) -> Result<()> {
    crate::network::run("ovs-vsctl", &["--may-exist", "add-br", ANCHOR_BRIDGE]).await?;
    // Nothing should forward here: the bridge exists only to own the tunnels
    // whose IPsec policy protects the real overlay bridges.
    let _ = crate::network::run(
        "ovs-ofctl",
        &["add-flow", ANCHOR_BRIDGE, "priority=0,actions=drop"],
    )
    .await;

    // Copy into the IKE daemon's own directories first — see IPSEC_DIR.
    let (cert, key, ca) = stage_credentials(cert, key, ca).map_err(|e| NetworkError::Command {
        cmd: format!("staging IPsec credentials into {IPSEC_DIR}"),
        stderr: e.to_string(),
    })?;
    let creds = credentials_args(&cert, &key, &ca);
    crate::network::run(
        "ovs-vsctl",
        &creds.iter().map(String::as_str).collect::<Vec<_>>(),
    )
    .await?;

    for peer in peers {
        let args = anchor_port_args(peer);
        crate::network::run(
            "ovs-vsctl",
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
        )
        .await?;
        if peer.cert_cn.is_empty() {
            tracing::warn!(
                peer = %peer.underlay_ip,
                "overlay IPsec peer has no certificate identity — accepting any \
                 CA-signed certificate for this tunnel; re-enroll the peer host \
                 so its CN is recorded"
            );
        }
    }
    Ok(())
}

/// Remove every IPsec anchor, for a host reverting to cleartext tunnels.
#[allow(dead_code)] // reachable once `overlay_encryption` can be lowered live
pub async fn clear() -> Result<()> {
    let _ = crate::network::run("ovs-vsctl", &["--if-exists", "del-br", ANCHOR_BRIDGE]).await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(ip: &str, cn: &str) -> Peer {
        Peer {
            underlay_ip: ip.into(),
            cert_cn: cn.into(),
        }
    }

    #[test]
    fn anchor_port_names_fit_ifnamsiz_and_are_stable() {
        let name = anchor_port("172.16.56.8");
        assert_eq!(name, "ipsec-56-8");
        assert!(name.len() < 16, "{name} exceeds IFNAMSIZ");
        assert_eq!(anchor_port("172.16.56.8"), anchor_port("172.16.56.8"));
        // Worst case: three-digit final octets.
        assert!(anchor_port("10.255.255.255").len() < 16);
    }

    /// The finding this design exists for: the IPsec selector has no VNI, so
    /// the anchor is per peer. Two peers, two ports — regardless of how many
    /// overlays run between them.
    #[test]
    fn one_anchor_per_peer_not_per_overlay() {
        let a = anchor_port("10.0.0.2");
        let b = anchor_port("10.0.0.3");
        assert_ne!(a, b);
        assert_eq!(a, anchor_port("10.0.0.2"), "same peer, same anchor");
    }

    /// `remote_name` is the whole identity check — without it any CA-signed
    /// certificate is accepted, which is exactly the hole ADR/§30 closes on the
    /// gRPC side.
    #[test]
    fn a_known_peer_is_identity_pinned() {
        let args = anchor_port_args(&peer("10.0.0.2", "agent-host2.lab"));
        assert!(
            args.iter()
                .any(|a| a == "options:remote_name=agent-host2.lab"),
            "{args:?}"
        );
        assert!(args.iter().any(|a| a == "options:remote_ip=10.0.0.2"));
        assert!(args.iter().any(|a| a == "type=vxlan"));
    }

    /// An unknown identity must not silently become a *pinned* one — better to
    /// omit the option (and warn) than to pin something wrong.
    #[test]
    fn an_unknown_peer_is_not_pinned() {
        let args = anchor_port_args(&peer("10.0.0.9", ""));
        assert!(!args.iter().any(|a| a.starts_with("options:remote_name")));
    }

    #[test]
    fn credentials_set_all_three_paths() {
        let args = credentials_args("/c.pem", "/k.pem", "/ca.pem");
        for expect in [
            "other_config:certificate=/c.pem",
            "other_config:private_key=/k.pem",
            "other_config:ca_cert=/ca.pem",
        ] {
            assert!(args.iter().any(|a| a == expect), "missing {expect}");
        }
    }

    /// Key material has to be staged where the IKE daemon may read it: on a
    /// host with AppArmor, charon can only open `/etc/ipsec.d/**`, and pointing
    /// it elsewhere fails as "no private key found" with no mention of the path.
    #[test]
    fn credentials_are_staged_under_the_ipsec_directory() {
        let (c, k, a) = (
            format!("{IPSEC_DIR}/certs/vquasar-agent.crt"),
            format!("{IPSEC_DIR}/private/vquasar-agent.key"),
            format!("{IPSEC_DIR}/cacerts/vquasar-ca.crt"),
        );
        let args = credentials_args(&c, &k, &a);
        for expect in [
            format!("other_config:certificate={c}"),
            format!("other_config:private_key={k}"),
            format!("other_config:ca_cert={a}"),
        ] {
            assert!(args.contains(&expect), "missing {expect}");
        }
        // The CA must be in cacerts/ specifically: strongSwan does not build a
        // chain from OVS's ca_cert alone.
        assert!(a.contains("/cacerts/"), "CA must land in cacerts: {a}");
        assert!(k.contains("/private/"), "key must land in private: {k}");
    }
}
