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

/// A control-resolved NIC binding: the guest MAC and its VLAN on the bridge.
#[derive(Debug, Clone)]
pub struct NicBinding {
    pub mac: String,
    /// 802.1Q tag; 0 means untagged/flat.
    pub vlan: u16,
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

        let mut args = vec!["--may-exist", "add-port", &self.bridge, &tap];
        let tag; // keep the formatted string alive for the borrow
        if binding.vlan != 0 {
            tag = format!("tag={}", binding.vlan);
            args.push(&tag);
        }
        run("ovs-vsctl", &args).await?;

        Ok(PreparedNic {
            tap,
            mac: binding.mac.clone(),
        })
    }

    async fn release(&self, vm: VmId, index: usize) -> Result<()> {
        let tap = tap_name(vm, index);
        // Best-effort and idempotent: ignore "already gone".
        let _ = run(
            "ovs-vsctl",
            &["--if-exists", "del-port", &self.bridge, &tap],
        )
        .await;
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
                },
            )
            .await
            .unwrap();
        assert_eq!(nic.mac, "02:00:00:00:00:01");
        assert_eq!(nic.tap, tap_name(vm, 0));
    }
}
