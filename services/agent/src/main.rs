//! `ch-agent` — the host-agent binary (design document, section 9).
//!
//! Milestone 2: the agent is the local authority for one host. It collects host
//! inventory, recovers any VMs already running (so a restart never kills them —
//! section 11), and serves the `HostAgent` gRPC API backed by a real Cloud
//! Hypervisor process manager.

mod backend;
mod config;
mod grpc;
mod inventory;
mod manager;
mod network;
mod runtime;

use std::path::PathBuf;
use std::sync::Arc;

use ch_proto::agent::host_agent_server::HostAgentServer;
use clap::Parser;
use tonic::transport::Server;
use tracing::info;

use crate::backend::CloudHypervisorBackend;
use crate::config::AgentConfig;
use crate::grpc::AgentService;
use crate::manager::VmManager;
use crate::runtime::RuntimeLayout;

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

    let host = inventory::collect();
    let ch_version = inventory::cloud_hypervisor_version(&config.hypervisor.binary);
    info!(
        name = %config.agent.name,
        hostname = ?host.hostname,
        arch = ?host.architecture,
        logical_cpus = ?host.logical_cpus,
        total_memory_bytes = ?host.total_memory_bytes,
        cloud_hypervisor = ?ch_version,
        "ch-agent starting"
    );

    // Build the VM manager over a real Cloud Hypervisor backend and the OVS
    // dataplane on the configured integration bridge (section 18).
    let layout = RuntimeLayout::new(&config.hypervisor.runtime_dir);
    let backend = Arc::new(CloudHypervisorBackend::new(
        config.hypervisor.binary.clone(),
    ));
    let network = Arc::new(network::OvsNetworkBackend::new(
        config.network.bridge.clone(),
    ));
    info!(
        backend = %config.network.backend,
        bridge = %config.network.bridge,
        "network dataplane configured"
    );
    let manager = Arc::new(VmManager::new(backend, network, layout));

    // Recover VMs that survived a previous agent instance (section 11).
    manager.recover().await;
    let recovered = manager.list().await;
    if !recovered.is_empty() {
        info!(
            count = recovered.len(),
            "recovered running VMs after restart"
        );
    }

    let service = AgentService::new(manager, config.agent.name.clone(), ch_version);

    let addr = config
        .grpc
        .listen
        .parse()
        .map_err(|e| anyhow::anyhow!("invalid grpc.listen '{}': {e}", config.grpc.listen))?;
    info!(%addr, "serving HostAgent gRPC API");

    Server::builder()
        .add_service(HostAgentServer::new(service))
        .serve_with_shutdown(addr, shutdown_signal())
        .await?;

    info!("ch-agent stopped");
    Ok(())
}

/// Resolve when the process receives Ctrl-C, so the gRPC server shuts down
/// cleanly. Note: this does **not** terminate managed VMs — they keep running
/// (section 11).
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}
