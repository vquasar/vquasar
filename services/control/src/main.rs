//! `ch-control` — the control-plane binary.
//!
//! Milestone 0 scaffold: it loads configuration, initialises telemetry, and
//! logs what it *would* serve. The REST API, scheduler, task engine and agent
//! gRPC channel arrive in later milestones (design document, section 42).

mod config;

use std::path::PathBuf;

use clap::Parser;
use tracing::info;

use crate::config::ControlConfig;

/// ch-orchestrator control plane.
#[derive(Debug, Parser)]
#[command(name = "ch-control", version, about)]
struct Cli {
    /// Path to a TOML configuration file.
    #[arg(short, long, env = "CH_CONTROL_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = ControlConfig::load(cli.config.as_deref())?;

    ch_common::telemetry::init(&config.logging.level);

    info!(
        api = %config.server.listen,
        grpc = %config.grpc.listen,
        database = %config.database.url,
        "ch-control starting (Milestone 0 scaffold)"
    );
    info!("REST API, scheduler and agent channel not yet implemented");

    // Nothing to serve yet; exit cleanly so `cargo run` is a no-op success.
    Ok(())
}
