//! Host-agent configuration (design document, section 36).

use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

/// Top-level agent configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentConfig {
    #[serde(default)]
    pub agent: AgentSection,
    #[serde(default)]
    pub grpc: GrpcSection,
    #[serde(default)]
    pub hypervisor: HypervisorSection,
    #[serde(default)]
    pub network: NetworkSection,
    #[serde(default)]
    pub storage: StorageSection,
    #[serde(default)]
    pub migration: MigrationSection,
    #[serde(default)]
    pub tls: TlsSection,
    #[serde(default)]
    pub phone_home: PhoneHomeSection,
    #[serde(default)]
    pub logging: LoggingSection,
}

/// cloud-init phone_home IP-discovery fallback (design M13e). When `url` (the
/// control plane's base URL) is set, generated cloud-init tells the guest to
/// POST to `<url>/api/v1/phone-home/$INSTANCE_ID` on first boot; the control
/// plane records the request's source IP. If the agent has a TLS CA configured,
/// it is injected into the guest (cloud-init `ca_certs`) so an HTTPS control
/// endpoint with an internal CA is trusted.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PhoneHomeSection {
    #[serde(default)]
    pub url: Option<String>,
}

/// Mutual-TLS material for the agent's gRPC server (design M12a). When all three
/// paths are set, the agent requires a client certificate signed by `ca` whose
/// Common Name is `control_cn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlsSection {
    #[serde(default)]
    pub ca: Option<PathBuf>,
    #[serde(default)]
    pub cert: Option<PathBuf>,
    #[serde(default)]
    pub key: Option<PathBuf>,
    /// Common Name the control plane's client certificate must carry.
    ///
    /// Chaining to the CA is not identity: every agent's certificate also
    /// chains to it, so without this check any host that can read its own key
    /// can drive every other agent's gRPC API — a host compromise would become
    /// a fleet compromise, which design §30 forbids. `scripts/gen-certs.sh`
    /// issues the control certificate as `CN=control`.
    #[serde(default = "default_control_cn")]
    pub control_cn: String,
}

fn default_control_cn() -> String {
    "control".to_string()
}

impl Default for TlsSection {
    fn default() -> Self {
        Self {
            ca: None,
            cert: None,
            key: None,
            control_cn: default_control_cn(),
        }
    }
}

impl TlsSection {
    /// Whether mutual TLS is fully configured.
    pub fn enabled(&self) -> bool {
        self.ca.is_some() && self.cert.is_some() && self.key.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationSection {
    /// Live-migration transport: `tcp` (cross-host) or `unix` (single-host lab).
    #[serde(default = "default_migration_transport")]
    pub transport: String,
    /// Address peers use to reach this host for TCP migration (e.g. its
    /// hostname or IP). Defaults to the machine hostname.
    #[serde(default)]
    pub advertise_host: String,
    /// TCP port range for incoming migrations (must be open in the firewall).
    #[serde(default = "default_migration_port_min")]
    pub port_min: u16,
    #[serde(default = "default_migration_port_max")]
    pub port_max: u16,
    /// Directory for `unix`-transport migration sockets.
    #[serde(default = "default_migration_socket_dir")]
    pub socket_dir: PathBuf,
}

fn default_migration_transport() -> String {
    "tcp".to_string()
}
fn default_migration_port_min() -> u16 {
    9600
}
fn default_migration_port_max() -> u16 {
    9700
}
fn default_migration_socket_dir() -> PathBuf {
    PathBuf::from("/var/lib/vquasar/migrations")
}

impl Default for MigrationSection {
    fn default() -> Self {
        Self {
            transport: default_migration_transport(),
            advertise_host: String::new(),
            port_min: default_migration_port_min(),
            port_max: default_migration_port_max(),
            socket_dir: default_migration_socket_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcSection {
    /// Address the agent's HostAgent gRPC service listens on (control-plane
    /// facing; must not be exposed publicly — section 12).
    pub listen: String,
}

impl Default for GrpcSection {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:9500".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSection {
    /// Human-friendly host name for this agent.
    pub name: String,
    /// Control-plane address the agent registers with (unused until Milestone 3).
    pub control_plane: String,
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            name: "host-01".to_string(),
            control_plane: "http://127.0.0.1:9443".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HypervisorSection {
    /// Path to the `cloud-hypervisor` binary.
    pub binary: PathBuf,
    /// Root directory for per-VM runtime state (section 9).
    pub runtime_dir: PathBuf,
    /// Serial console target: `socket` (interactive console, section 25) or
    /// `file` (log only). On a single-host lab, `file` avoids the socket-path
    /// conflict when live-migrating between two co-located agents (section 28).
    #[serde(default = "default_serial_mode")]
    pub serial_mode: String,
    /// Cloud Hypervisor seccomp mode: `true` | `false` | `log` | `errno`. Live
    /// migration currently trips the default filter on some platforms, so `log`
    /// may be needed for migration to work (design section 30 prefers `true`).
    #[serde(default = "default_seccomp")]
    pub seccomp: String,
}

fn default_serial_mode() -> String {
    "socket".to_string()
}

fn default_seccomp() -> String {
    "true".to_string()
}

impl Default for HypervisorSection {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("/usr/bin/cloud-hypervisor"),
            runtime_dir: PathBuf::from("/run/vquasar"),
            serial_mode: default_serial_mode(),
            seccomp: default_seccomp(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkSection {
    /// Dataplane backend (`ovs` for the MVP).
    pub backend: String,
    /// Integration bridge name (section 18).
    pub bridge: String,
    /// Bind each TAP's egress to the MAC the control plane allocated, dropping
    /// spoofed frames and ARP whose sender hardware address is not the guest's
    /// own (design §30). Without it, any guest can impersonate any other VM on
    /// the shared bridge — no control-plane scoping can undo that.
    ///
    /// **Defaults to off**, because switching it on changes the dataplane of a
    /// running cluster: a guest that legitimately sources other MACs — VRRP or
    /// keepalived (virtual MAC `00:00:5e:00:01:xx`), nested virtualisation,
    /// in-guest bridging — loses that traffic. There is no allowed-address-pairs
    /// escape yet, so it is all-or-nothing per host. Check for VIPs, then enable.
    ///
    /// The agent warns on every start while this is off; silence is not the
    /// intended resting state.
    #[serde(default = "default_port_security")]
    pub port_security: bool,
}

fn default_port_security() -> bool {
    false
}

impl Default for NetworkSection {
    fn default() -> Self {
        Self {
            backend: "ovs".to_string(),
            bridge: "br-int".to_string(),
            port_security: default_port_security(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageSection {
    /// Storage backend (`local` for the MVP).
    pub backend: String,
    /// Root path for local volumes (section 20).
    pub path: PathBuf,
    /// Shared-storage root. Provisioned per-VM volumes and cloud-init seeds live
    /// here so live migration can reuse them on the destination (design M9).
    #[serde(default = "default_shared_dir")]
    pub shared_dir: PathBuf,
}

fn default_shared_dir() -> PathBuf {
    PathBuf::from("/var/lib/vquasar/shared")
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            backend: "local".to_string(),
            path: PathBuf::from("/var/lib/vquasar"),
            shared_dir: default_shared_dir(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingSection {
    pub level: String,
    /// Log output format: "text" (default) or "json" for structured export
    /// (design M17).
    #[serde(default = "default_log_format")]
    pub format: String,
    /// OpenTelemetry collector endpoint for OTLP/gRPC span export, e.g.
    /// `http://collector:4317`. Unset ⇒ no span export (design M17).
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

fn default_log_format() -> String {
    "text".to_string()
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: default_log_format(),
            otlp_endpoint: None,
        }
    }
}

impl AgentConfig {
    /// Load configuration from an optional TOML file, then apply environment
    /// overrides prefixed with `VQUASAR_AGENT_` (e.g. `VQUASAR_AGENT_AGENT__NAME`).
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        // Seed with the full default config so partial file/env overrides (a
        // single leaf key) merge cleanly instead of failing on missing fields.
        let mut figment = Figment::from(Serialized::defaults(AgentConfig::default()));
        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        }
        let config = figment
            .merge(Env::prefixed("VQUASAR_AGENT_").split("__"))
            .extract()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::result_large_err)]
    use super::*;

    #[test]
    fn defaults_match_design() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.network.bridge, "br-int");
        assert_eq!(cfg.storage.backend, "local");
        assert_eq!(
            cfg.hypervisor.binary,
            PathBuf::from("/usr/bin/cloud-hypervisor")
        );
        // Port security is opt-in: an upgrade must not change the dataplane of
        // a running cluster (a guest may rely on a virtual MAC).
        assert!(!cfg.network.port_security);
    }

    #[test]
    fn port_security_can_be_enabled_by_config() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("VQUASAR_AGENT_NETWORK__PORT_SECURITY", "true");
            let cfg = AgentConfig::load(None).unwrap();
            assert!(cfg.network.port_security);
            Ok(())
        });
    }

    #[test]
    fn env_overrides_apply() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("VQUASAR_AGENT_AGENT__NAME", "hv-42");
            let cfg = AgentConfig::load(None).unwrap();
            assert_eq!(cfg.agent.name, "hv-42");
            Ok(())
        });
    }
}
