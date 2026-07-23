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
    pub logging: LoggingSection,
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
}

impl Default for HypervisorSection {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("/usr/bin/cloud-hypervisor"),
            runtime_dir: PathBuf::from("/run/ch-orchestrator"),
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
}

impl Default for StorageSection {
    fn default() -> Self {
        Self {
            backend: "local".to_string(),
            path: PathBuf::from("/var/lib/ch-orchestrator"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingSection {
    pub level: String,
}

impl Default for LoggingSection {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
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
