//! `ch-control` — the control-plane binary (design document, section 8).
//!
//! Milestone 3: persists desired state in PostgreSQL, serves the public REST
//! API, and runs the reconcile loop that polls host agents and drives VMs to
//! their desired state.

mod agent;
mod api;
mod authn;
mod authz;
mod config;
mod console;
mod crypto;
mod ipam;
mod netalloc;
mod rbac;
mod reconcile;
mod scheduler;
mod store;

use std::path::PathBuf;
use std::time::Duration;

use axum::response::IntoResponse;
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

    // rustls 0.23 needs a process-wide crypto provider before any TLS use.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Configure mutual TLS to agents when certs are present (design M12a).
    if config.tls.enabled() {
        let ca = std::fs::read(config.tls.ca.as_ref().unwrap())?;
        let cert = std::fs::read(config.tls.cert.as_ref().unwrap())?;
        let key = std::fs::read(config.tls.key.as_ref().unwrap())?;
        crate::agent::init_client_tls(ca, cert, key);
        info!("agent connections use mutual TLS");
    }

    info!(database = %redact(&config.database.url), "connecting to PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database.url)
        .await?;
    // Field encryption at rest (design M12c). Disabled -> plaintext.
    let cryptor = crate::crypto::Cryptor::from_config(&config.encryption)
        .map_err(|e| anyhow::anyhow!("encryption config: {e}"))?;
    if cryptor.is_some() {
        info!("field encryption enabled (sensitive cloud-init sealed at rest)");
    } else {
        info!("field encryption DISABLED — set [encryption] key to enable");
    }
    let store =
        Store::new(pool, config.storage.shared_volumes_dir.clone()).with_crypto(cryptor.clone());
    store.migrate().await?;
    info!("migrations applied");
    store
        .sync_builtin_roles(&crate::rbac::builtin_roles())
        .await?;
    // Seal any pre-existing plaintext secrets now that encryption is on.
    if cryptor.is_some() {
        let n = store.encrypt_existing().await?;
        if n > 0 {
            info!(rows = n, "sealed pre-existing plaintext cloud-init secrets");
        }
    }

    // Authentication / RBAC wiring (design M12b). Disabled -> dev superuser.
    let auth_state = if config.auth.enabled() {
        info!(issuer = %config.auth.issuer, "authentication enabled (OIDC)");
        let authn = crate::authn::Authenticator::discover(config.auth.clone())
            .await
            .map_err(|e| anyhow::anyhow!("OIDC discovery failed: {e}"))?;
        crate::authz::AuthState {
            authenticator: Some(std::sync::Arc::new(authn)),
            bootstrap_admin: config.auth.bootstrap_admin.clone(),
        }
    } else {
        info!("authentication DISABLED (dev mode) — set [auth] issuer to enforce");
        crate::authz::AuthState::disabled()
    };

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

    // Public REST API, optionally serving the built web UI. Static assets are
    // served from `/assets`; every other non-API path falls back to
    // `index.html` (200) so the single-page router handles deep links.
    let mut app = api::router(store, auth_state);
    if let Some(ui_dir) = &config.server.ui_dir {
        let dir = std::path::PathBuf::from(ui_dir);
        let index = dir.join("index.html");
        app = app
            .nest_service(
                "/assets",
                tower_http::services::ServeDir::new(dir.join("assets")),
            )
            .fallback(move || {
                let index = index.clone();
                async move {
                    match tokio::fs::read_to_string(&index).await {
                        Ok(html) => axum::response::Html(html).into_response(),
                        Err(e) => (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            format!("web UI not found: {e}"),
                        )
                            .into_response(),
                    }
                }
            });
        info!(ui_dir = %ui_dir, "serving web UI");
    }
    if config.tls.enabled() {
        // Serve the REST API + UI over HTTPS (design M12a).
        let addr: std::net::SocketAddr = config.server.listen.parse()?;
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            config.tls.cert.as_ref().unwrap(),
            config.tls.key.as_ref().unwrap(),
        )
        .await?;
        let handle = axum_server::Handle::new();
        let h = handle.clone();
        tokio::spawn(async move {
            shutdown_signal().await;
            h.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
        });
        info!(api = %config.server.listen, "serving REST API at /api/v1 over HTTPS");
        axum_server::bind_rustls(addr, tls)
            .handle(handle)
            .serve(app.into_make_service())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&config.server.listen).await?;
        info!(api = %config.server.listen, "serving REST API at /api/v1 (plaintext — configure [tls])");
        axum::serve(listener, app)
            .with_graceful_shutdown(shutdown_signal())
            .await?;
    }

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
