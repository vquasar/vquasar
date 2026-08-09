//! `vquasar-control` — the control-plane binary (design document, section 8).
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
mod cpucompat;
mod crypto;
mod ipam;
mod lease;
mod metrics;
mod netalloc;
mod overlay;
mod quota;
mod rbac;
mod reconcile;
mod recovery;
mod scheduler;
mod scoped;
mod segments;
mod store;

use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;
use sqlx::postgres::PgPoolOptions;
use tokio::signal::unix::{signal, SignalKind};
use tracing::info;

use crate::config::ControlConfig;
use crate::store::Store;

/// vquasar control plane.
#[derive(Debug, Parser)]
#[command(name = "vquasar-control", version, about)]
struct Cli {
    /// Path to a TOML configuration file.
    #[arg(short, long, env = "VQUASAR_CONTROL_CONFIG")]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = ControlConfig::load(cli.config.as_deref())?;
    vquasar_common::telemetry::init(
        &config.logging.level,
        config.logging.format == "json",
        config.logging.otlp_endpoint.as_deref(),
        "vquasar-control",
    );

    // rustls 0.23 needs a process-wide crypto provider before any TLS use.
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Prometheus metrics recorder (design M17). Installed before the reconcile
    // loop so its counters have somewhere to record.
    let prom = metrics::install()?;

    // Configure mutual TLS to agents when certs are present (design M12a).
    if config.tls.enabled() {
        let ca = std::fs::read(config.tls.ca.as_ref().unwrap())?;
        let cert = std::fs::read(config.tls.cert.as_ref().unwrap())?;
        let key = std::fs::read(config.tls.key.as_ref().unwrap())?;
        crate::agent::init_client_tls(ca, cert, key);
        info!("agent connections use mutual TLS");
    }

    // Database connection, including TLS to PostgreSQL. Unset ⇒ libpq's
    // `prefer`, which silently accepts plaintext — warn rather than fail so
    // existing deployments keep working, but make the exposure visible.
    let db_ssl_mode = config.database.effective_ssl_mode();
    if config.database.tls_is_optional() {
        tracing::warn!(
            ssl_mode = %db_ssl_mode,
            "database connection is NOT required to be encrypted — set \
             [database] ssl_mode = \"verify-full\" (and ca) to enforce TLS"
        );
    } else {
        info!(ssl_mode = %db_ssl_mode, "database connection requires TLS");
    }
    info!(database = %redact(&config.database.url), "connecting to PostgreSQL");
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(config.database.connect_options()?)
        .await?;
    // Field encryption at rest (design M12c). Disabled -> plaintext.
    let cryptor = crate::crypto::Cryptor::from_config(&config.encryption)
        .map_err(|e| anyhow::anyhow!("encryption config: {e}"))?;
    if cryptor.is_some() {
        info!("field encryption enabled (sensitive cloud-init sealed at rest)");
    } else {
        info!("field encryption DISABLED — set [encryption] key to enable");
    }
    let store = Store::new(pool, config.storage.shared_volumes_dir.clone())
        .with_crypto(cryptor.clone())
        .with_allowed_paths(config.storage.allowed_paths.clone())
        .with_network_policy(config.network.clone());
    info!(
        roots = ?config.storage.allowed_paths,
        "caller-supplied disk/kernel/firmware paths confined to these roots"
    );
    // Overlay encryption state (design §18, M18b). Cleartext tunnels are a
    // production blocker, so say so at every start rather than only in docs.
    let enc = config.network.overlay_encryption;
    let mtu = config.network.overlay_guest_mtu();
    if enc.is_encrypted() {
        info!(overlay_mtu = mtu, "VXLAN underlay is IPsec-protected");
    } else {
        tracing::warn!(
            overlay_encryption = enc.as_str(),
            overlay_mtu = mtu,
            "VXLAN underlay is CLEARTEXT — anyone on it can read overlay traffic \
             and inject frames into any VNI. Roll out [network] overlay_encryption \
             = \"reserve\" (MTU only), verify, then \"ipsec\"."
        );
    }
    store.migrate().await?;
    info!("migrations applied");
    store
        .sync_builtin_roles(&crate::rbac::builtin_roles())
        .await?;
    // The `default` pool, seeded once from [storage] shared_volumes_dir so a
    // cluster that predates pools keeps working and none of its paths move
    // (ADR-023). Deliberately not re-synced: config must not overrule an
    // operator who has since renamed or repointed it.
    store.ensure_default_pool().await?;
    // Volumes that predate pools join the default one. Here rather than in the
    // migration because the pool's id is generated at first boot, and a cluster
    // upgrading straight from pre-pool has no row to point at when 0030 runs.
    match store.adopt_poolless_volumes().await? {
        0 => {}
        n => info!(
            volumes = n,
            "adopted pre-pool volumes into the default pool"
        ),
    }
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
            tenancy_enabled: config.tenancy.enabled,
        }
    } else {
        info!("authentication DISABLED (dev mode) — set [auth] issuer to enforce");
        crate::authz::AuthState {
            tenancy_enabled: config.tenancy.enabled,
            ..crate::authz::AuthState::disabled()
        }
    };
    if config.tenancy.enabled {
        info!("tenancy enabled — requests are scoped to a project");
    }

    // The controller lease. Every instance serves the API; only the holder runs
    // the loops, so several control planes over one database is safe (ADR-021).
    let instance_id = config
        .server
        .instance_id
        .clone()
        .unwrap_or_else(lease::Lease::default_identity);

    // Stamp work this instance starts in a detached task, so a restart
    // reclaims its own orphans and never another instance's (ADR-021).
    let store = store.with_instance(&instance_id);

    // Anything this instance left mid-flight is orphaned: its detached task
    // died with the process that owned it. Reclaim before the API opens, so a
    // caller never sees a volume reservation that will never be finished
    // (design §7). After `with_instance`, because the sweep is scoped by owner.
    recovery::reclaim_orphaned_work(&store).await;
    let lease = std::sync::Arc::new(lease::Lease::new(store.pool().clone(), &instance_id));
    info!(
        identity = %lease.identity(),
        ttl_secs = lease::TTL.as_secs(),
        "contending for the controller lease"
    );
    tokio::spawn(lease.clone().run());

    // Reconcile loop (host polling + VM reconciliation).
    let interval = Duration::from_secs(config.reconcile.interval_secs);
    let reconcile_store = store.clone();
    let reconcile_lease = lease.clone();
    tokio::spawn(async move {
        reconcile::run(reconcile_store, interval, reconcile_lease).await;
    });
    info!(
        interval_secs = config.reconcile.interval_secs,
        "reconcile loop started"
    );

    // Public REST API, optionally serving the built web UI. Every file in the
    // UI directory is served from its own path (hashed bundles under /assets,
    // but also the favicons and brand marks that sit at the root); anything
    // that matches no file falls back to `index.html` so the single-page router
    // handles deep links like /hosts/:id.
    // Agent auto-enrollment (design M16): active only when the intermediate
    // issuing CA is configured.
    let enrollment = if config.tls.can_issue() {
        info!("agent auto-enrollment enabled (intermediate CA present)");
        Some(api::EnrollmentState {
            root_ca: config.tls.ca.clone().unwrap(),
            issuer_cert: config.tls.issuer_cert.clone().unwrap(),
            issuer_key: config.tls.issuer_key.clone().unwrap(),
            control_url: config.enrollment.control_url.clone(),
            token_ttl_secs: config.enrollment.token_ttl_secs,
        })
    } else {
        None
    };

    let mut app = api::router(store, auth_state, enrollment)
        // Prometheus scrape endpoint (unauthenticated, like /healthz — it
        // exposes cluster shape, not secrets; restrict by network policy).
        .route(
            "/metrics",
            axum::routing::get({
                let prom = prom.clone();
                move || {
                    let prom = prom.clone();
                    async move { prom.render() }
                }
            }),
        )
        .layer(axum::middleware::from_fn(metrics::track_http));
    if let Some(ui_dir) = &config.server.ui_dir {
        let dir = std::path::PathBuf::from(ui_dir);
        let index = dir.join("index.html");
        // ServeDir resolves a real file when one exists and otherwise serves
        // the SPA shell. Path traversal is ServeDir's problem, and it rejects
        // `..` before touching the filesystem.
        let spa = tower_http::services::ServeDir::new(dir)
            .fallback(tower_http::services::ServeFile::new(index));
        // The console fetches OIDC metadata and tokens straight from the
        // identity provider, so its origin is the one cross-origin destination
        // the policy has to allow.
        let idp_origin = origin_of(&config.auth.issuer);
        app = app
            .fallback_service(spa)
            .layer(axum::middleware::from_fn(move |req, next| {
                let idp = idp_origin.clone();
                async move { security_headers(idp, req, next).await }
            }));
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
        // Connect-info so the phone_home endpoint can read the guest's source IP.
        axum_server::bind_rustls(addr, tls)
            .handle(handle)
            .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
            .await?;
    } else {
        let listener = tokio::net::TcpListener::bind(&config.server.listen).await?;
        info!(api = %config.server.listen, "serving REST API at /api/v1 (plaintext — configure [tls])");
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    }

    // Hand the lease back rather than making the fleet wait out its TTL. A
    // crash skips this, which is what the TTL is for (ADR-021).
    lease.release().await;
    info!("vquasar-control stopped");
    Ok(())
}

/// Resolve when systemd (or a terminal) asks the process to stop.
///
/// **SIGTERM as well as SIGINT.** systemd sends SIGTERM, so a handler that
/// waits only on Ctrl-C never runs under the unit that actually ships: the
/// process is killed outright and every graceful step is skipped. That was true
/// here until a control-plane failover on the lab took the full lease TTL
/// instead of a renewal interval, because the lease was never handed back
/// (ADR-021).
async fn shutdown_signal() {
    let mut term = match signal(SignalKind::terminate()) {
        Ok(s) => s,
        Err(e) => {
            // Without SIGTERM this degrades to the old behaviour rather than
            // refusing to start: an ungraceful stop is worse than no stop.
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

/// Hide credentials in a connection URL before logging it.
fn redact(url: &str) -> String {
    match url.split_once('@') {
        Some((_creds, host)) => format!("postgres://***@{host}"),
        None => url.to_string(),
    }
}

/// Scheme + host of a URL, for use in a CSP source list. Returns `None` for an
/// empty or unparseable issuer (auth disabled), which simply leaves the policy
/// same-origin.
fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    let host = rest.split('/').next()?;
    if host.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

/// Baseline security headers for everything this server hands out (design
/// section 30: a browser holding an operator's token is part of the trust
/// boundary).
///
/// The console is entirely first-party — fonts, styles and scripts all ship in
/// the bundle — so the policy is `'self'` with two deliberate exceptions:
/// `style-src` allows inline styles because the component library injects them
/// at runtime, and `connect-src` allows the OIDC provider's origin because the
/// sign-in flow talks to it directly.
async fn security_headers(
    idp_origin: Option<String>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::header::{HeaderName, HeaderValue};

    let mut res = next.run(req).await;
    let connect = match &idp_origin {
        Some(o) => format!("'self' {o}"),
        None => "'self'".to_string(),
    };
    let csp = format!(
        "default-src 'self'; \
         script-src 'self'; \
         style-src 'self' 'unsafe-inline'; \
         font-src 'self'; \
         img-src 'self' data:; \
         connect-src {connect}; \
         frame-ancestors 'none'; \
         base-uri 'self'; \
         form-action 'self'; \
         object-src 'none'"
    );
    let headers = res.headers_mut();
    for (name, value) in [
        ("content-security-policy", csp.as_str()),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("x-frame-options", "DENY"),
        // The console needs no device APIs at all.
        (
            "permissions-policy",
            "camera=(), microphone=(), geolocation=(), payment=()",
        ),
    ] {
        if let (Ok(n), Ok(v)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            headers.insert(n, v);
        }
    }
    res
}
