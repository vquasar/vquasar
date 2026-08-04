//! Control-plane configuration (design document, section 36).

use std::path::Path;

use figment::providers::{Env, Format, Serialized, Toml};
use figment::Figment;
use serde::{Deserialize, Serialize};

/// Top-level control-plane configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ControlConfig {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub grpc: GrpcConfig,
    #[serde(default)]
    pub reconcile: ReconcileConfig,
    #[serde(default)]
    pub storage: StorageConfig,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// TLS material (design M12a). When all set, the control plane serves its API
/// over HTTPS and talks to agents over mutual TLS using this identity. The same
/// `control` certificate serves both roles (serverAuth + clientAuth).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub ca: Option<String>,
    #[serde(default)]
    pub cert: Option<String>,
    #[serde(default)]
    pub key: Option<String>,
}

impl TlsConfig {
    pub fn enabled(&self) -> bool {
        self.ca.is_some() && self.cert.is_some() && self.key.is_some()
    }
}

/// Shared-storage layout the control plane uses to place provisioned volumes
/// (design M9). Must be a path every agent can reach (e.g. an NFS mount).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Directory for per-VM provisioned volumes.
    pub shared_volumes_dir: String,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            shared_volumes_dir: "/var/lib/ch-orchestrator/shared/volumes".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconcileConfig {
    /// How often the controllers poll agents and reconcile VMs (section 26).
    pub interval_secs: u64,
}

impl Default for ReconcileConfig {
    fn default() -> Self {
        Self { interval_secs: 5 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Public REST API listen address.
    pub listen: String,
    /// Optional path to the built web UI (`ui/dist`). When set, the control
    /// plane serves it as a single-page app alongside the API.
    #[serde(default)]
    pub ui_dir: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".to_string(),
            ui_dir: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL (unused until Milestone 3).
    pub url: String,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://localhost/ch_orchestrator".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrpcConfig {
    /// Listen address for the agent-facing gRPC endpoint (Milestone 3).
    pub listen: String,
}

impl Default for GrpcConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:9443".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
        }
    }
}

impl ControlConfig {
    /// Load configuration from an optional TOML file, then apply environment
    /// overrides prefixed with `CH_CONTROL_` (e.g. `CH_CONTROL_SERVER__LISTEN`).
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        // Seed with the full default config so partial file/env overrides (a
        // single leaf key) merge cleanly instead of failing on missing fields.
        let mut figment = Figment::from(Serialized::defaults(ControlConfig::default()));
        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        }
        let config = figment
            .merge(Env::prefixed("CH_CONTROL_").split("__"))
            .extract()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = ControlConfig::default();
        assert_eq!(cfg.server.listen, "0.0.0.0:8080");
        assert_eq!(cfg.grpc.listen, "0.0.0.0:9443");
        assert_eq!(cfg.logging.level, "info");
    }

    #[test]
    fn env_overrides_apply() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("CH_CONTROL_SERVER__LISTEN", "127.0.0.1:9000");
            let cfg = ControlConfig::load(None).unwrap();
            assert_eq!(cfg.server.listen, "127.0.0.1:9000");
            Ok(())
        });
    }
}
