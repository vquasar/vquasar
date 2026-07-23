//! `ch-control` — the control-plane binary (design document, section 8).
//!
//! Milestone 3: persists desired state in PostgreSQL, serves the public REST
//! API, and runs the reconcile loop that polls host agents and drives VMs to
//! their desired state.

mod agent;
mod api;
mod config;
mod reconcile;
mod scheduler;
mod store;

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

use crate::config::ControlConfig;
use crate::store::Store;

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

    info!(database = %redact(&config.database.url), "connecting to PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database.url)
        .await?;
    let store = Store::new(pool);
    store.migrate().await?;
    info!("migrations applied");

    // Reconcile loop (host polling + VM reconciliation).
    let interval = Duration::from_secs(config.reconcile.interval_secs);
    let reconcile_store = store.clone();
    tokio::spawn(async move {
        reconcile::run(reconcile_store, interval).await;
    });
    info!(
        interval_secs = config.reconcile.interval_secs,
        "reconcile loop started"
    );

    // Public REST API.
    let app = api::router(store);
    let listener = tokio::net::TcpListener::bind(&config.server.listen).await?;
    info!(api = %config.server.listen, "serving REST API at /api/v1");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("ch-control stopped");
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

/// Hide credentials in a connection URL before logging it.
fn redact(url: &str) -> String {
    match url.split_once('@') {
        Some((_creds, host)) => format!("postgres://***@{host}"),
        None => url.to_string(),
    }
}
