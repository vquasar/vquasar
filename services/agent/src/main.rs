//! `ch-agent` — the host-agent binary.
//!
//! Milestone 0 scaffold: it loads configuration, initialises telemetry, and
//! collects and logs host inventory (section 40: "report CPU / report memory").
//! The gRPC server, VM process manager and control-plane registration arrive in
//! Milestone 2 (design document, section 42).

mod config;
mod inventory;

use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use crate::config::AgentConfig;

/// ch-orchestrator host agent.
#[derive(Debug, Parser)]
#[command(name = "ch-agent", version, about)]
struct Cli {
    /// Path to a TOML configuration file.
    #[arg(short, long, env = "CH_AGENT_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = AgentConfig::load(cli.config.as_deref())?;

    ch_common::telemetry::init(&config.logging.level);

    info!(
        name = %config.agent.name,
        control_plane = %config.agent.control_plane,
        "ch-agent starting (Milestone 0 scaffold)"
    );

    let host = inventory::collect();
    info!(
        hostname = ?host.hostname,
        arch = ?host.architecture,
        kernel = ?host.kernel_version,
        logical_cpus = ?host.logical_cpus,
        total_memory_bytes = ?host.total_memory_bytes,
        available_memory_bytes = ?host.available_memory_bytes,
        "collected host inventory"
    );
    info!(
        hypervisor_binary = %config.hypervisor.binary.display(),
        runtime_dir = %config.hypervisor.runtime_dir.display(),
        network_backend = %config.network.backend,
        bridge = %config.network.bridge,
        storage_backend = %config.storage.backend,
        storage_path = %config.storage.path.display(),
        "agent configuration loaded; gRPC server not yet implemented"
    );

    Ok(())
}
