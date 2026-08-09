//! Host dataplane: TAP devices attached to Open vSwitch (design document,
//! sections 18 and 30).
//!
//! The agent owns these privileged operations; the control plane never touches
//! host networking directly (ADR-001/ADR-010). TAP names are derived
//! deterministically from the VM id so they can be recreated or torn down after
//! an agent restart without any persisted per-NIC state (section 23).

use async_trait::async_trait;
use tokio::process::Command;
use vquasar_model::VmId;

/// A failure preparing or releasing host networking.
#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("`{cmd}` failed: {stderr}")]
    Command { cmd: String, stderr: String },
    #[error("io error running network command: {0}")]
    Io(#[from] std::io::Error),
}

type Result<T> = std::result::Result<T, NetworkError>;

/// A control-resolved NIC binding: the guest MAC and its dataplane placement.
#[derive(Debug, Clone)]
pub struct NicBinding {
    pub mac: String,
    /// 802.1Q tag on the integration bridge; 0 means untagged/flat.
    pub vlan: u16,
    /// VXLAN VNI (design M13b); 0 ⇒ not an overlay. When set, the TAP is placed
    /// on a per-VNI overlay bridge with a tunnel mesh to `overlay_peers`.
    pub vni: u32,
    /// Underlay IPs of the other hosts, for the VXLAN tunnel mesh.
    pub overlay_peers: Vec<String>,
    /// Security groups (design M13c): when true, install a stateful conntrack
    /// firewall on the TAP with `ingress_rules` as the allow-list.
    pub filtered: bool,
    pub ingress_rules: Vec<crate::firewall::SecRule>,
    /// Egress allow-list, and whether egress is default-deny at all (§18).
    pub egress_rules: Vec<crate::firewall::SecRule>,
    pub egress_default_deny: bool,
    /// Protect the underlay to these peers with IPsec (design §18, M18b).
    pub encrypt_underlay: bool,
    /// Peers and their certificate identities, for IPsec pinning.
    pub overlay_peer_identities: Vec<OverlayPeerId>,
}

/// A peer host and the certificate identity to pin for it (M18b).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverlayPeerId {
    pub underlay_ip: String,
    pub cert_cn: String,
}

/// The per-VNI overlay bridge name (design M13b). `vxbr` + ≤8 digits ≤ IFNAMSIZ.
fn overlay_bridge(vni: u32) -> String {
    format!("vxbr{vni}")
}

/// The VNI an overlay bridge name refers to, if it is one.
pub fn vni_of_bridge(name: &str) -> Option<u32> {
    name.strip_prefix("vxbr")?.parse().ok()
}

/// VNIs this host currently carries an overlay bridge for (design §18).
///
/// Reported in inventory so the control plane can confirm a released segment is
/// gone from every host before reusing its VNI.
pub async fn overlay_vnis() -> Vec<u32> {
    match run_stdout("ovs-vsctl", &["list-br"]).await {
        Ok(out) => out.lines().filter_map(vni_of_bridge).collect(),
        Err(_) => Vec::new(),
    }
}

/// A prepared host interface ready to hand to Cloud Hypervisor.
#[derive(Debug, Clone)]
pub struct PreparedNic {
    pub tap: String,
    pub mac: String,
}

/// The host networking backend (section 18).
#[async_trait]
pub trait NetworkBackend: Send + Sync {
    /// Create the TAP for a VM's NIC and attach it to the dataplane.
    async fn prepare(&self, vm: VmId, index: usize, binding: &NicBinding) -> Result<PreparedNic>;
    /// Remove the TAP and its dataplane port (idempotent).
    async fn release(&self, vm: VmId, index: usize) -> Result<()>;
    /// Reconcile an already-present NIC's dataplane without recreating its TAP:
    /// move it to its network's bridge if the network changed (design M13d) and
    /// re-apply the security-group firewall so rule changes take effect (M13c).
    /// Idempotent; a no-op for backends without a dataplane.
    async fn rehome(&self, _vm: VmId, _index: usize, _binding: &NicBinding) -> Result<()> {
        Ok(())
    }
}

/// The host-local TAP name for a VM NIC.
///
/// `tap` + 8 hex chars + a single-digit index = 12 chars, within Linux's
/// `IFNAMSIZ` limit of 15 (section 23).
pub fn tap_name(vm: VmId, index: usize) -> String {
    format!("tap{}{}", vm.short(), index)
}

/// Open vSwitch backend: TAPs on the configured integration bridge (`br-int`).
pub struct OvsNetworkBackend {
    bridge: String,
    /// (certificate, key, CA) paths for underlay IPsec (M18b).
    ipsec_credentials: Option<(String, String, String)>,
    /// Bind each TAP's egress to its allocated MAC (design §30).
    port_security: bool,
}

impl OvsNetworkBackend {
    pub fn new(bridge: impl Into<String>) -> Self {
        Self {
            bridge: bridge.into(),
            port_security: true,
            ipsec_credentials: None,
        }
    }

    /// TLS material the agent may reuse for underlay IPsec (M18b).
    pub fn with_ipsec_credentials(mut self, creds: Option<(String, String, String)>) -> Self {
        self.ipsec_credentials = creds;
        self
    }

    /// Disable MAC anti-spoofing (escape hatch for guests that legitimately
    /// source frames from other MACs).
    pub fn with_port_security(mut self, enabled: bool) -> Self {
        self.port_security = enabled;
        self
    }

    /// Apply this NIC's dataplane policy on `bridge`.
    ///
    /// Two layers: the security-group firewall when the NIC has groups (M13c),
    /// and port security — binding egress to the allocated MAC — which applies
    /// either way (design §30). A NIC with no groups is still unfiltered at
    /// L3/L4, but it can no longer impersonate another VM on the shared bridge.
    /// `port_security = false` restores the old behaviour for a guest that must
    /// source frames from other MACs (nested virtualisation, bridging).
    async fn apply_firewall(&self, bridge: &str, tap: &str, binding: &NicBinding) -> Result<()> {
        match (binding.filtered, self.port_security) {
            (true, _) => {
                crate::firewall::apply(
                    bridge,
                    tap,
                    &binding.mac,
                    &crate::firewall::Policy {
                        ingress: binding.ingress_rules.clone(),
                        egress: binding.egress_rules.clone(),
                        egress_default_deny: binding.egress_default_deny,
                    },
                )
                .await
            }
            (false, true) => crate::firewall::apply_port_security(bridge, tap, &binding.mac).await,
            // Neither: make sure no stale flows linger from a prior config.
            (false, false) => crate::firewall::clear(bridge, tap).await,
        }
    }

    /// Configure (or tear down) IPsec anchors for the overlay peers (M18b).
    ///
    /// Driven from the binding rather than agent config so the fleet converges
    /// on one decision made in the control plane, instead of drifting per host.
    async fn apply_underlay_ipsec(&self, binding: &NicBinding) -> Result<()> {
        if !binding.encrypt_underlay {
            // Encryption was turned off. The ingress filter must come down with
            // it: without IPsec no VXLAN packet carries a secpath, so leaving
            // the filter installed would drop every overlay frame on the host.
            if let Err(e) = crate::hostfw::clear().await {
                tracing::warn!(error = %e, "could not remove the VXLAN ingress filter");
            }
            if let Err(e) = crate::ipsec::clear().await {
                tracing::debug!(error = %e, "could not remove IPsec anchors");
            }
            return Ok(());
        }
        let Some((cert, key, ca)) = self.ipsec_credentials.as_ref() else {
            tracing::warn!(
                "control asked for an encrypted underlay but this agent has no TLS \
                 material configured — tunnels stay CLEARTEXT"
            );
            return Ok(());
        };
        // IPsec protects configured peers; it does not stop injection from an
        // unconfigured source (design §18, M18c).
        if let Err(e) = crate::hostfw::apply().await {
            tracing::warn!(error = %e, "could not install the VXLAN ingress filter — \
                 cleartext VXLAN from an unconfigured source is still accepted");
        }
        let peers: Vec<crate::ipsec::Peer> = binding
            .overlay_peer_identities
            .iter()
            .map(|p| crate::ipsec::Peer {
                underlay_ip: p.underlay_ip.clone(),
                cert_cn: p.cert_cn.clone(),
            })
            .collect();
        crate::ipsec::apply(cert, key, ca, &peers).await
    }

    /// Ensure a per-VNI overlay bridge and its full-mesh of VXLAN tunnel ports
    /// exist (design M13b), returning the bridge name.
    async fn ensure_overlay(&self, vni: u32, peers: &[String]) -> Result<String> {
        let br = overlay_bridge(vni);
        run("ovs-vsctl", &["--may-exist", "add-br", &br]).await?;
        let _ = run("ip", &["link", "set", &br, "up"]).await;
        let mut peers = peers.to_vec();
        peers.sort();
        peers.dedup();
        for (i, peer) in peers.iter().enumerate() {
            // Port names are global across the switch, so scope by VNI to avoid
            // collisions between overlay bridges on the same host.
            let port = format!("vx{vni}_{i}");
            run(
                "ovs-vsctl",
                &[
                    "--may-exist",
                    "add-port",
                    &br,
                    &port,
                    "--",
                    "set",
                    "interface",
                    &port,
                    "type=vxlan",
                    &format!("options:remote_ip={peer}"),
                    &format!("options:key={vni}"),
                ],
            )
            .await?;
        }
        Ok(br)
    }

    /// The bridge a NIC should live on: a per-VNI overlay bridge, or the shared
    /// integration bridge for flat/VLAN.
    async fn desired_bridge(&self, binding: &NicBinding) -> Result<String> {
        if binding.vni != 0 {
            // Protect the underlay before the overlay carries anything. The
            // anchors are per peer, not per VNI, so this converges to the same
            // configuration however many overlays a host runs (M18b).
            self.apply_underlay_ipsec(binding).await?;
            self.ensure_overlay(binding.vni, &binding.overlay_peers)
                .await
        } else {
            Ok(self.bridge.clone())
        }
    }

    /// Set (or clear) the 802.1Q tag on a TAP port for flat/VLAN placement.
    async fn set_vlan(&self, tap: &str, vlan: u16) -> Result<()> {
        if vlan != 0 {
            run("ovs-vsctl", &["set", "port", tap, &format!("tag={vlan}")]).await
        } else {
            let _ = run("ovs-vsctl", &["clear", "port", tap, "tag"]).await;
            Ok(())
        }
    }

    /// Delete an overlay bridge once its last guest TAP has left (design M13b).
    async fn gc_overlay_if_empty(&self, br: &str) {
        if br.starts_with("vxbr") {
            if let Ok(ports) = run_stdout("ovs-vsctl", &["list-ports", br]).await {
                if !ports.lines().any(|p| p.trim().starts_with("tap")) {
                    let _ = run("ovs-vsctl", &["--if-exists", "del-br", br]).await;
                }
            }
        }
    }
}

#[async_trait]
impl NetworkBackend for OvsNetworkBackend {
    async fn prepare(&self, vm: VmId, index: usize, binding: &NicBinding) -> Result<PreparedNic> {
        let tap = tap_name(vm, index);
        // Make creation idempotent: a launch that crashed after creating the TAP
        // (but before CH claimed it) leaves an orphan device, and `ip tuntap add`
        // would then fail forever with "Device or resource busy". Since the name
        // is deterministic per VM/NIC, clearing any stale same-named TAP first is
        // safe and lets reconcile recover on the next tick (sections 11, 23).
        let _ = run("ip", &["link", "del", &tap]).await;
        run("ip", &["tuntap", "add", "dev", &tap, "mode", "tap"]).await?;
        run("ip", &["link", "set", &tap, "up"]).await?;

        // Place the TAP on its network's bridge: a per-VNI overlay bridge (M13b)
        // or the shared integration bridge (flat/VLAN), then apply the firewall.
        let br = self.desired_bridge(binding).await?;
        run("ovs-vsctl", &["--may-exist", "add-port", &br, &tap]).await?;
        if binding.vni == 0 {
            self.set_vlan(&tap, binding.vlan).await?;
        }
        self.apply_firewall(&br, &tap, binding).await?;

        Ok(PreparedNic {
            tap,
            mac: binding.mac.clone(),
        })
    }

    async fn rehome(&self, vm: VmId, index: usize, binding: &NicBinding) -> Result<()> {
        let tap = tap_name(vm, index);
        // The TAP may not be attached yet on the first reconcile; prepare() owns
        // initial placement, so only act once it exists on some bridge.
        let Ok(current) = run_stdout("ovs-vsctl", &["port-to-br", &tap]).await else {
            return Ok(());
        };
        let current = current.trim().to_string();
        let desired = self.desired_bridge(binding).await?;

        if current != desired {
            // NIC network changed (design M13d): move the TAP to the new bridge.
            // CH keeps the same TAP fd, so the VM is not restarted; only its L2
            // placement changes.
            let _ = crate::firewall::clear(&current, &tap).await;
            let _ = run("ovs-vsctl", &["--if-exists", "del-port", &current, &tap]).await;
            run("ovs-vsctl", &["--may-exist", "add-port", &desired, &tap]).await?;
            self.gc_overlay_if_empty(&current).await;
        }
        if binding.vni == 0 {
            self.set_vlan(&tap, binding.vlan).await?;
        }
        self.apply_firewall(&desired, &tap, binding).await
    }

    async fn release(&self, vm: VmId, index: usize) -> Result<()> {
        let tap = tap_name(vm, index);
        // Find whichever bridge holds the TAP (integration or a per-VNI overlay)
        // so release works without knowing the original binding.
        if let Ok(br) = run_stdout("ovs-vsctl", &["port-to-br", &tap]).await {
            let br = br.trim().to_string();
            // Remove this NIC's security-group flows before the port (design M13c).
            let _ = crate::firewall::clear(&br, &tap).await;
            let _ = run("ovs-vsctl", &["--if-exists", "del-port", &br, &tap]).await;
            // Garbage-collect an overlay bridge once its last guest TAP is gone.
            self.gc_overlay_if_empty(&br).await;
        } else {
            // Not on any bridge; still fall back to the integration bridge.
            let _ = run(
                "ovs-vsctl",
                &["--if-exists", "del-port", &self.bridge, &tap],
            )
            .await;
        }
        let _ = run("ip", &["link", "del", &tap]).await;
        Ok(())
    }
}

pub(crate) async fn run(cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd).args(args).output().await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(NetworkError::Command {
            cmd: format!("{cmd} {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// Run a command and return its trimmed stdout, erroring on non-zero exit.
async fn run_stdout(cmd: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(cmd).args(args).output().await?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(NetworkError::Command {
            cmd: format!("{cmd} {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        })
    }
}

/// A no-op backend for tests: it returns the deterministic TAP name but touches
/// no host state.
#[cfg(test)]
pub struct NoopNetworkBackend;

#[cfg(test)]
#[async_trait]
impl NetworkBackend for NoopNetworkBackend {
    async fn prepare(&self, vm: VmId, index: usize, binding: &NicBinding) -> Result<PreparedNic> {
        Ok(PreparedNic {
            tap: tap_name(vm, index),
            mac: binding.mac.clone(),
        })
    }
    async fn release(&self, _vm: VmId, _index: usize) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_names_fit_ifnamsiz_and_are_deterministic() {
        let vm = VmId::new();
        let a = tap_name(vm, 0);
        let b = tap_name(vm, 0);
        assert_eq!(a, b);
        assert!(a.len() <= 15, "tap name {a} exceeds IFNAMSIZ");
        assert_ne!(tap_name(vm, 0), tap_name(vm, 1));
    }

    #[tokio::test]
    async fn noop_backend_returns_binding_mac() {
        let vm = VmId::new();
        let nic = NoopNetworkBackend
            .prepare(
                vm,
                0,
                &NicBinding {
                    encrypt_underlay: false,
                    overlay_peer_identities: Vec::new(),
                    mac: "02:00:00:00:00:01".into(),
                    vlan: 0,
                    vni: 0,
                    overlay_peers: Vec::new(),
                    filtered: false,
                    ingress_rules: Vec::new(),
                    egress_rules: Vec::new(),
                    egress_default_deny: false,
                },
            )
            .await
            .unwrap();
        assert_eq!(nic.mac, "02:00:00:00:00:01");
        assert_eq!(nic.tap, tap_name(vm, 0));
    }

    #[test]
    fn overlay_bridge_names_round_trip_to_their_vni() {
        assert_eq!(vni_of_bridge("vxbr4096"), Some(4096));
        assert_eq!(vni_of_bridge(&overlay_bridge(16_777_215)), Some(16_777_215));
        // Anything that is not an overlay bridge must not be reported as one —
        // a false report would keep a VNI quarantined forever.
        assert_eq!(vni_of_bridge("br-int"), None);
        assert_eq!(vni_of_bridge("vxbr"), None);
        assert_eq!(vni_of_bridge("vxbrx"), None);
        assert_eq!(vni_of_bridge("br-vxipsec"), None);
    }
}
