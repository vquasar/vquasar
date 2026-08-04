//! Host dataplane: TAP devices attached to Open vSwitch (design document,
//! sections 18 and 30).
//!
//! The agent owns these privileged operations; the control plane never touches
//! host networking directly (ADR-001/ADR-010). TAP names are derived
//! deterministically from the VM id so they can be recreated or torn down after
//! an agent restart without any persisted per-NIC state (section 23).

use async_trait::async_trait;
use ch_model::VmId;
use tokio::process::Command;

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
}

/// The per-VNI overlay bridge name (design M13b). `vxbr` + ≤8 digits ≤ IFNAMSIZ.
fn overlay_bridge(vni: u32) -> String {
    format!("vxbr{vni}")
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
    /// Re-apply just the security-group firewall for an already-present NIC, so
    /// rule changes reach a running VM without recreating its TAP (design M13c).
    /// Idempotent; a no-op for backends without a firewall.
    async fn refresh_firewall(&self, _vm: VmId, _index: usize, _binding: &NicBinding) -> Result<()> {
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
}

impl OvsNetworkBackend {
    pub fn new(bridge: impl Into<String>) -> Self {
        Self {
            bridge: bridge.into(),
        }
    }

    /// Apply (or clear) this NIC's security-group firewall on `bridge` (M13c).
    async fn apply_firewall(&self, bridge: &str, tap: &str, binding: &NicBinding) -> Result<()> {
        if binding.filtered {
            crate::firewall::apply(bridge, tap, &binding.mac, &binding.ingress_rules).await
        } else {
            // Not filtered: make sure no stale flows linger from a prior config.
            crate::firewall::clear(bridge, tap).await
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

        // VXLAN overlay (design M13b): the TAP goes on a per-VNI bridge with a
        // full mesh of tunnel ports to the peer hosts, isolated on the wire by
        // the VNI — no physical-switch VLAN needed.
        if binding.vni != 0 {
            let br = overlay_bridge(binding.vni);
            run("ovs-vsctl", &["--may-exist", "add-br", &br]).await?;
            let _ = run("ip", &["link", "set", &br, "up"]).await;
            // One tunnel port per peer. Names are stable per (sorted) peer index
            // so repeated reconciles are idempotent (--may-exist).
            let mut peers = binding.overlay_peers.clone();
            peers.sort();
            peers.dedup();
            for (i, peer) in peers.iter().enumerate() {
                let port = format!("vx{i}");
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
                        &format!("options:key={}", binding.vni),
                    ],
                )
                .await?;
            }
            // Guest TAP untagged: the overlay bridge is the network's L2 domain.
            run("ovs-vsctl", &["--may-exist", "add-port", &br, &tap]).await?;
            self.apply_firewall(&br, &tap, binding).await?;
            return Ok(PreparedNic {
                tap,
                mac: binding.mac.clone(),
            });
        }

        // Flat/VLAN: TAP on the shared integration bridge, optionally 802.1Q.
        let mut args = vec!["--may-exist", "add-port", &self.bridge, &tap];
        let tag; // keep the formatted string alive for the borrow
        if binding.vlan != 0 {
            tag = format!("tag={}", binding.vlan);
            args.push(&tag);
        }
        run("ovs-vsctl", &args).await?;
        self.apply_firewall(&self.bridge, &tap, binding).await?;

        Ok(PreparedNic {
            tap,
            mac: binding.mac.clone(),
        })
    }

    async fn refresh_firewall(&self, vm: VmId, index: usize, binding: &NicBinding) -> Result<()> {
        let tap = tap_name(vm, index);
        // The TAP may not be attached yet on the first reconcile; that's fine —
        // prepare() applies the firewall, and the next tick refreshes it.
        if let Ok(br) = run_stdout("ovs-vsctl", &["port-to-br", &tap]).await {
            self.apply_firewall(br.trim(), &tap, binding).await?;
        }
        Ok(())
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
            // Garbage-collect an overlay bridge once its last guest TAP is gone
            // (this also removes its tunnel ports).
            if br.starts_with("vxbr") {
                if let Ok(ports) = run_stdout("ovs-vsctl", &["list-ports", &br]).await {
                    if !ports.lines().any(|p| p.trim().starts_with("tap")) {
                        let _ = run("ovs-vsctl", &["--if-exists", "del-br", &br]).await;
                    }
                }
            }
        } else {
            // Not on any bridge; still fall back to the integration bridge.
            let _ = run("ovs-vsctl", &["--if-exists", "del-port", &self.bridge, &tap]).await;
        }
        let _ = run("ip", &["link", "del", &tap]).await;
        Ok(())
    }
}

async fn run(cmd: &str, args: &[&str]) -> Result<()> {
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
                    mac: "02:00:00:00:00:01".into(),
                    vlan: 0,
                    vni: 0,
                    overlay_peers: Vec::new(),
                    filtered: false,
                    ingress_rules: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(nic.mac, "02:00:00:00:00:01");
        assert_eq!(nic.tap, tap_name(vm, 0));
    }
}
