//! `vquasar-agent` — the host-agent binary (design document, section 9).
//!
//! Milestone 2: the agent is the local authority for one host. It collects host
//! inventory, recovers any VMs already running (so a restart never kills them —
//! section 11), and serves the `HostAgent` gRPC API backed by a real Cloud
//! Hypervisor process manager.

mod backend;
mod config;
mod console;
mod epoch;
mod firewall;

/// What this binary is: the release it was built from, or the crate version
/// when nothing stamped it. Set by `make` and by the release workflow, and
/// reported to the control plane with the rest of the inventory so a fleet can
/// be asked what it is running.
pub const BUILD: &str = match option_env!("VQUASAR_BUILD") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
mod grpc;
mod hostfw;
mod inventory;
mod ipdiscovery;
mod ipsec;
mod manager;
mod metrics;
mod network;
mod peerid;
mod pools;
mod runtime;
mod storage;

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use tokio::signal::unix::{signal, SignalKind};
use tonic::transport::{Certificate, Identity, Server, ServerTlsConfig};
use tracing::info;
use vquasar_proto::agent::host_agent_server::HostAgentServer;

use crate::backend::CloudHypervisorBackend;
use crate::config::AgentConfig;
use crate::grpc::AgentService;
use crate::manager::VmManager;
use crate::runtime::RuntimeLayout;

/// vquasar host agent.
#[derive(Debug, Parser)]
#[command(name = "vquasar-agent", version, about)]
struct Cli {
    /// Path to a TOML configuration file.
    #[arg(short, long, env = "VQUASAR_AGENT_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = AgentConfig::load(cli.config.as_deref())?;

    vquasar_common::telemetry::init(
        &config.logging.level,
        config.logging.format == "json",
        config.logging.otlp_endpoint.as_deref(),
        "vquasar-agent",
    );

    // rustls 0.23 needs a process-wide crypto provider before any TLS use.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let host = inventory::collect();
    let ch_version = inventory::cloud_hypervisor_version(&config.hypervisor.binary);
    info!(
        build = BUILD,
        name = %config.agent.name,
        hostname = ?host.hostname,
        arch = ?host.architecture,
        logical_cpus = ?host.logical_cpus,
        total_memory_bytes = ?host.total_memory_bytes,
        cloud_hypervisor = ?ch_version,
        "vquasar-agent starting"
    );

    // Build the VM manager over a real Cloud Hypervisor backend and the OVS
    // dataplane on the configured integration bridge (section 18).
    let layout = RuntimeLayout::new(&config.hypervisor.runtime_dir);
    let backend = Arc::new(CloudHypervisorBackend::new(
        config.hypervisor.binary.clone(),
        &config.hypervisor.serial_mode,
        &config.hypervisor.seccomp,
    ));
    let network = Arc::new(
        network::OvsNetworkBackend::new(config.network.bridge.clone())
            .with_port_security(config.network.port_security)
            .with_ipsec_credentials(
                match (
                    config.tls.cert.as_ref(),
                    config.tls.key.as_ref(),
                    config.tls.ca.as_ref(),
                ) {
                    (Some(c), Some(k), Some(a)) => Some((
                        c.to_string_lossy().into_owned(),
                        k.to_string_lossy().into_owned(),
                        a.to_string_lossy().into_owned(),
                    )),
                    _ => None,
                },
            ),
    );
    if !config.network.port_security {
        tracing::warn!(
            "MAC anti-spoofing is OFF — any guest on this bridge can source frames \
             as another VM's MAC. Enable [network] port_security once you have \
             checked no guest relies on a virtual MAC (VRRP/keepalived, nested \
             virtualisation, in-guest bridging)."
        );
    }
    info!(
        backend = %config.network.backend,
        bridge = %config.network.bridge,
        port_security = config.network.port_security,
        "network dataplane configured"
    );
    // cloud-init phone_home IP-discovery fallback (design M13e): inject our CA so
    // the guest trusts an internal-CA HTTPS control endpoint.
    let phone_home_ca = config
        .tls
        .ca
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok());
    let storage = crate::storage::StorageProvisioner::new(config.storage.shared_dir.clone())
        .with_phone_home(config.phone_home.url.clone(), phone_home_ca);
    if let Some(url) = &config.phone_home.url {
        info!(%url, "cloud-init phone_home enabled");
    }
    info!(shared_dir = %config.storage.shared_dir.display(), "storage provisioner configured");
    // Agentless guest-IP discovery via neighbor snooping on the bridge (M11).
    let ipdiscovery = crate::ipdiscovery::IpDiscovery::new(config.network.bridge.clone());
    ipdiscovery.start();
    let migration = crate::manager::MigrationSettings {
        transport: config.migration.transport.clone(),
        advertise_host: config.migration.advertise_host.clone(),
        port_min: config.migration.port_min,
        port_max: config.migration.port_max,
        socket_dir: config.migration.socket_dir.clone(),
    };
    let manager = Arc::new(VmManager::new(
        backend,
        network,
        storage,
        ipdiscovery,
        layout,
        migration,
    ));

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
    // Mutual TLS on the control-facing gRPC API when configured (design M12a):
    // present the agent certificate and require a client cert signed by our CA.
    let mut server = Server::builder();
    if config.tls.enabled() {
        let cert = std::fs::read(config.tls.cert.as_ref().unwrap())?;
        let key = std::fs::read(config.tls.key.as_ref().unwrap())?;
        let ca = std::fs::read(config.tls.ca.as_ref().unwrap())?;
        let tls = ServerTlsConfig::new()
            .identity(Identity::from_pem(cert, key))
            .client_ca_root(Certificate::from_pem(ca));
        server = server.tls_config(tls)?;
        info!(%addr, "serving HostAgent gRPC API with mutual TLS");
    } else {
        info!(%addr, "serving HostAgent gRPC API (plaintext — configure [tls] for mTLS)");
    }

    // Chaining to the CA is not identity — see peerid (design §30).
    let mut identity = crate::peerid::RequireControlIdentity::new(
        config.tls.enabled().then(|| config.tls.control_cn.clone()),
    );
    if config.tls.enabled() {
        info!(control_cn = %config.tls.control_cn, "requiring control-plane certificate identity");
        // Epoch fencing only where there is an identity to bind it to: without
        // mTLS the epoch is a number any caller can pick, and enforcing it
        // would imply a guarantee that is not there (ADR-022).
        let guard = std::sync::Arc::new(crate::epoch::EpochGuard::load(
            &config.grpc.controller_epoch_file,
            config.grpc.require_controller_epoch,
        ));
        info!(
            strict = config.grpc.require_controller_epoch,
            epoch = guard.highest(),
            path = %config.grpc.controller_epoch_file.display(),
            "refusing superseded controllers by lease epoch"
        );
        identity = identity.with_epoch(guard);
    } else if config.grpc.require_controller_epoch {
        // Say so rather than silently doing nothing: an operator who set this
        // has asked for a guarantee, and needs to know it is not in force.
        tracing::warn!(
            "[grpc] require_controller_epoch is set but [tls] is not configured; \
             epoch fencing is off (ADR-022)"
        );
    }

    server
        .add_service(HostAgentServer::with_interceptor(service, identity))
        .serve_with_shutdown(addr, shutdown_signal())
        .await?;

    info!("vquasar-agent stopped");
    Ok(())
}

/// Resolve when systemd (or a terminal) asks the process to stop, so the gRPC
/// server shuts down cleanly. Note: this does **not** terminate managed VMs —
/// they keep running (section 11).
///
/// SIGTERM as well as SIGINT: systemd sends SIGTERM, so waiting only on Ctrl-C
/// means the graceful path never runs under the unit that actually ships.
async fn shutdown_signal() {
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "cannot listen for SIGTERM; only Ctrl-C will stop gracefully");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
