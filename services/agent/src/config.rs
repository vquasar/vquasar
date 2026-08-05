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
/// paths are set, the agent requires a client certificate signed by `ca`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TlsSection {
    #[serde(default)]
    pub ca: Option<PathBuf>,
    #[serde(default)]
    pub cert: Option<PathBuf>,
    #[serde(default)]
    pub key: Option<PathBuf>,
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
    PathBuf::from("/var/lib/ch-orchestrator/migrations")
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
            runtime_dir: PathBuf::from("/run/ch-orchestrator"),
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
}

impl Default for NetworkSection {
    fn default() -> Self {
        Self {
            backend: "ovs".to_string(),
            bridge: "br-int".to_string(),
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
    PathBuf::from("/var/lib/ch-orchestrator/shared")
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            backend: "local".to_string(),
            path: PathBuf::from("/var/lib/ch-orchestrator"),
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
}

fn default_log_format() -> String {
    "text".to_string()
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: default_log_format(),
        }
    }
}

impl AgentConfig {
    /// Load configuration from an optional TOML file, then apply environment
    /// overrides prefixed with `CH_AGENT_` (e.g. `CH_AGENT_AGENT__NAME`).
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        // Seed with the full default config so partial file/env overrides (a
        // single leaf key) merge cleanly instead of failing on missing fields.
        let mut figment = Figment::from(Serialized::defaults(AgentConfig::default()));
        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        }
        let config = figment
            .merge(Env::prefixed("CH_AGENT_").split("__"))
            .extract()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
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
    }

    #[test]
    fn env_overrides_apply() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("CH_AGENT_AGENT__NAME", "hv-42");
            let cfg = AgentConfig::load(None).unwrap();
            assert_eq!(cfg.agent.name, "hv-42");
            Ok(())
        });
    }
}
