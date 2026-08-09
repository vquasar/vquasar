//! The control plane's own configuration, read-only (design §36).
//!
//! Built as a purpose-made struct rather than by serialising [`ControlConfig`]
//! and removing fields. Redaction by subtraction is one forgotten `skip` away
//! from publishing a database URL or an OIDC client secret; here a value can
//! only appear because somebody wrote a line to put it there, and adding a
//! secret would mean writing that line on purpose.
//!
//! Nothing here is writable. A console that renders a control it cannot write
//! is worse than one that shows a value and says where it comes from — so this
//! endpoint exists to answer "what is this cluster configured to do", and the
//! answer to "change it" stays `control.toml` and a restart.

use axum::extract::State;
use axum::Json;

use crate::api::error::ApiResult;
use crate::authz::AuthUser;
use crate::store::Store;

/// What the console is allowed to know about how this control plane is set up.
#[derive(Debug, serde::Serialize)]
pub struct ConfigView {
    /// The build this binary came from, so a console can say which one it is
    /// talking to without an operator reading a systemd unit.
    ///
    /// `git describe` when the build knew — which is every packaged build — and
    /// the crate version otherwise. A binary that cannot say what it is, is one
    /// nobody can support.
    pub version: &'static str,
    pub reconcile: ReconcileView,
    pub network: NetworkView,
    pub storage: StorageView,
    pub security: SecurityView,
    pub tenancy: TenancyView,
}

#[derive(Debug, serde::Serialize)]
pub struct ReconcileView {
    pub interval_secs: u64,
    /// When the leader last finished a reconcile pass. A loop that has stopped
    /// is otherwise invisible: every VM simply stays as it was, which looks the
    /// same as a fleet with nothing to do.
    pub last_pass_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, serde::Serialize)]
pub struct NetworkView {
    /// `legacy` | `enforced` (ADR-017).
    pub policy_mode: String,
    /// `allow` | `enforced` (ADR-024).
    pub egress_mode: String,
    pub overlay_encryption: &'static str,
    pub physical_networks: Vec<String>,
    pub provider_vlans: String,
}

#[derive(Debug, serde::Serialize)]
pub struct StorageView {
    /// Roots a caller-supplied disk/kernel/firmware path must sit under (§30).
    pub allowed_paths: Vec<String>,
    /// `off` | `report` | `delete` (#41).
    pub orphan_reclaim: crate::orphans::Policy,
    pub orphan_sweep_secs: u64,
    pub orphan_min_age_secs: u64,
}

/// Whether each protection is *on*, never how it is configured. "Encryption is
/// enabled" is what an operator needs to see on a dashboard; the key is not.
#[derive(Debug, serde::Serialize)]
pub struct SecurityView {
    pub authentication: bool,
    pub encryption_at_rest: bool,
    pub agent_mtls: bool,
    pub database_tls: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct TenancyView {
    pub enabled: bool,
}

/// `GET /config` — how this control plane is set up.
///
/// Guarded by `host:read`: it describes the platform, which is fleet inventory
/// rather than any tenant's business.
pub async fn get(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<ConfigView>> {
    user.require("host:read")?;
    let cfg = store.config_view();
    let net = store.network_policy();
    let storage = store.storage_config();
    Ok(Json(ConfigView {
        version: crate::BUILD,
        reconcile: ReconcileView {
            interval_secs: cfg.reconcile_interval_secs,
            last_pass_at: crate::lease::last_pass_at(store.pool()).await?,
        },
        network: NetworkView {
            policy_mode: net.policy_mode.clone(),
            egress_mode: net.egress_mode.clone(),
            overlay_encryption: net.overlay_encryption.as_str(),
            physical_networks: net.physical_networks.clone(),
            provider_vlans: net.provider_vlans.clone(),
        },
        storage: StorageView {
            allowed_paths: store.allowed_paths().to_vec(),
            orphan_reclaim: storage.orphan_reclaim,
            orphan_sweep_secs: storage.orphan_sweep_secs,
            orphan_min_age_secs: storage.orphan_min_age_secs,
        },
        security: SecurityView {
            authentication: cfg.authentication,
            encryption_at_rest: cfg.encryption_at_rest,
            agent_mtls: cfg.agent_mtls,
            database_tls: cfg.database_tls,
        },
        tenancy: TenancyView {
            enabled: cfg.tenancy,
        },
    }))
}
