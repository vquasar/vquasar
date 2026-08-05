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
    pub enrollment: EnrollmentConfig,
    #[serde(default)]
    pub auth: AuthConfig,
    #[serde(default)]
    pub encryption: EncryptionConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Field-level encryption of sensitive data at rest (design M12c). Encryption
/// is on only when `key` is set; otherwise sensitive fields are stored in
/// plaintext (backward compatible).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EncryptionConfig {
    /// Active AES-256 key, 32 bytes base64-encoded (`openssl rand -base64 32`).
    #[serde(default)]
    pub key: Option<String>,
    /// Identifier stamped into sealed values so keys can be rotated. Defaults
    /// to "default" when a key is set without an id.
    #[serde(default)]
    pub key_id: Option<String>,
    /// Retired decrypt-only keys during rotation, as "id:base64,id2:base64".
    #[serde(default)]
    pub old_keys: Option<String>,
}

/// OIDC authentication + RBAC bootstrap (design M12b). Auth is enforced only
/// when an `issuer` is set and `disabled` is false — the dev escape hatch that
/// keeps a fresh install open until an identity provider is wired up.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default)]
    pub issuer: String,
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub audience: String,
    #[serde(default = "default_groups_claim")]
    pub groups_claim: String,
    /// Identity (email or OIDC subject) granted `admin` on first login.
    #[serde(default)]
    pub bootstrap_admin: Option<String>,
    /// Extra CA (PEM) to trust when reaching the OIDC provider — for an IdP
    /// behind an internal CA (e.g. Keycloak on our own CA). Added on top of the
    /// system roots; omit for a publicly-trusted provider.
    #[serde(default)]
    pub ca: Option<String>,
    /// Explicitly disable auth (dev only). Production installs must not set this.
    #[serde(default)]
    pub disabled: bool,
}

fn default_groups_claim() -> String {
    "groups".to_string()
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            issuer: String::new(),
            client_id: String::new(),
            audience: String::new(),
            groups_claim: default_groups_claim(),
            bootstrap_admin: None,
            ca: None,
            disabled: false,
        }
    }
}

impl AuthConfig {
    /// Whether requests must carry a valid token.
    pub fn enabled(&self) -> bool {
        !self.disabled && !self.issuer.is_empty()
    }
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
    /// Intermediate issuing-CA certificate (PEM) used to sign agent certs at
    /// enrollment (design M16). Signed by the offline root (`ca`); presented in
    /// the agent's chain. Only the intermediate lives on control, never the root
    /// key.
    #[serde(default)]
    pub issuer_cert: Option<String>,
    /// Intermediate issuing-CA private key (PEM). Enables in-process agent-cert
    /// signing; keep it 0600. Absent ⇒ auto-enrollment is disabled.
    #[serde(default)]
    pub issuer_key: Option<String>,
}

impl TlsConfig {
    pub fn enabled(&self) -> bool {
        self.ca.is_some() && self.cert.is_some() && self.key.is_some()
    }

    /// Whether the control plane can sign agent certificates (auto-enrollment).
    pub fn can_issue(&self) -> bool {
        self.enabled() && self.issuer_cert.is_some() && self.issuer_key.is_some()
    }
}

/// Token-based agent auto-enrollment (design M16). Only active when the TLS
/// issuing CA is configured (`tls.can_issue()`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollmentConfig {
    /// HTTPS URL agents use to reach this control plane's enrollment endpoint,
    /// e.g. `https://control.lab:8080`. Returned to operators at enroll time.
    #[serde(default)]
    pub control_url: Option<String>,
    /// One-time enrollment-token lifetime in seconds (default 1h).
    #[serde(default = "default_token_ttl")]
    pub token_ttl_secs: u64,
}

fn default_token_ttl() -> u64 {
    3600
}

impl Default for EnrollmentConfig {
    fn default() -> Self {
        Self {
            control_url: None,
            token_ttl_secs: default_token_ttl(),
        }
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
            shared_volumes_dir: "/var/lib/vquasar/shared/volumes".to_string(),
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
    /// PostgreSQL connection URL.
    pub url: String,
    /// TLS mode for the database connection: `disable`, `allow`, `prefer`,
    /// `require`, `verify-ca` or `verify-full`. Unset ⇒ whatever the URL says,
    /// which in turn defaults to libpq's `prefer` — TLS if the server offers it,
    /// **silent plaintext otherwise**. Set `verify-full` for a real deployment.
    #[serde(default)]
    pub ssl_mode: Option<String>,
    /// CA certificate (PEM) that signed the PostgreSQL server certificate.
    /// Required for `verify-ca`/`verify-full` against a private CA; without it
    /// verification falls back to the system trust roots.
    #[serde(default)]
    pub ca: Option<String>,
    /// Client certificate (PEM) for PostgreSQL certificate authentication.
    #[serde(default)]
    pub cert: Option<String>,
    /// Client private key (PEM) matching `cert`. Keep it 0600.
    #[serde(default)]
    pub key: Option<String>,
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            url: "postgres://localhost/vquasar".to_string(),
            ssl_mode: None,
            ca: None,
            cert: None,
            key: None,
        }
    }
}

/// Database TLS modes that permit an unencrypted connection.
const PLAINTEXT_CAPABLE_SSL_MODES: &[&str] = &["disable", "allow", "prefer"];

impl DatabaseConfig {
    /// Build the sqlx connection options: the URL first (so `sslmode=` and
    /// friends in the URL keep working), then the explicit `[database]` TLS
    /// settings on top, which win where both are given.
    pub fn connect_options(&self) -> anyhow::Result<sqlx::postgres::PgConnectOptions> {
        use std::str::FromStr;

        let mut opts = sqlx::postgres::PgConnectOptions::from_str(&self.url)
            .map_err(|e| anyhow::anyhow!("invalid [database] url: {e}"))?;

        if let Some(mode) = &self.ssl_mode {
            let parsed = sqlx::postgres::PgSslMode::from_str(mode).map_err(|_| {
                anyhow::anyhow!(
                    "invalid [database] ssl_mode {mode:?} — expected one of \
                     disable, allow, prefer, require, verify-ca, verify-full"
                )
            })?;
            opts = opts.ssl_mode(parsed);
        }
        // Read the files here rather than handing sqlx a path, so a missing or
        // unreadable certificate is a startup error naming the file instead of
        // a connection error much later.
        if let Some(ca) = &self.ca {
            let pem = std::fs::read(ca)
                .map_err(|e| anyhow::anyhow!("reading [database] ca {ca}: {e}"))?;
            opts = opts.ssl_root_cert_from_pem(pem);
        }
        match (&self.cert, &self.key) {
            (Some(cert), Some(key)) => {
                let cert_pem = std::fs::read(cert)
                    .map_err(|e| anyhow::anyhow!("reading [database] cert {cert}: {e}"))?;
                let key_pem = std::fs::read(key)
                    .map_err(|e| anyhow::anyhow!("reading [database] key {key}: {e}"))?;
                opts = opts
                    .ssl_client_cert_from_pem(cert_pem)
                    .ssl_client_key_from_pem(key_pem);
            }
            (None, None) => {}
            _ => anyhow::bail!("[database] cert and key must be set together"),
        }
        Ok(opts)
    }

    /// The TLS mode that will actually be used, for logging: the explicit
    /// setting, else the URL's `sslmode`, else sqlx's `prefer` default.
    pub fn effective_ssl_mode(&self) -> String {
        if let Some(mode) = &self.ssl_mode {
            return mode.to_ascii_lowercase();
        }
        self.url
            .split_once('?')
            .map(|(_, q)| q)
            .and_then(|q| {
                q.split('&').find_map(|kv| {
                    let (k, v) = kv.split_once('=')?;
                    matches!(k, "sslmode" | "ssl-mode").then(|| v.to_ascii_lowercase())
                })
            })
            .unwrap_or_else(|| "prefer".to_string())
    }

    /// Whether the effective mode allows the connection to end up unencrypted.
    pub fn tls_is_optional(&self) -> bool {
        PLAINTEXT_CAPABLE_SSL_MODES.contains(&self.effective_ssl_mode().as_str())
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
    /// Log output format: "text" (default) or "json" for structured export
    /// (design M17).
    #[serde(default = "default_log_format")]
    pub format: String,
    /// OpenTelemetry collector endpoint for OTLP/gRPC span export, e.g.
    /// `http://collector:4317`. Unset ⇒ no span export (design M17).
    #[serde(default)]
    pub otlp_endpoint: Option<String>,
}

fn default_log_format() -> String {
    "text".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: default_log_format(),
            otlp_endpoint: None,
        }
    }
}

impl ControlConfig {
    /// Load configuration from an optional TOML file, then apply environment
    /// overrides prefixed with `VQUASAR_CONTROL_` (e.g. `VQUASAR_CONTROL_SERVER__LISTEN`).
    pub fn load(path: Option<&Path>) -> anyhow::Result<Self> {
        // Seed with the full default config so partial file/env overrides (a
        // single leaf key) merge cleanly instead of failing on missing fields.
        let mut figment = Figment::from(Serialized::defaults(ControlConfig::default()));
        if let Some(path) = path {
            figment = figment.merge(Toml::file(path));
        }
        let config = figment
            .merge(Env::prefixed("VQUASAR_CONTROL_").split("__"))
            .extract()?;
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::result_large_err)]
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let cfg = ControlConfig::default();
        assert_eq!(cfg.server.listen, "0.0.0.0:8080");
        assert_eq!(cfg.grpc.listen, "0.0.0.0:9443");
        assert_eq!(cfg.logging.level, "info");
    }

    /// Absent config must not change how we connect — the default stays
    /// whatever the URL/libpq says, so existing deployments are unaffected.
    #[test]
    fn database_tls_defaults_to_prefer_and_is_optional() {
        let cfg = DatabaseConfig::default();
        assert_eq!(cfg.effective_ssl_mode(), "prefer");
        assert!(cfg.tls_is_optional());
        assert!(cfg.connect_options().is_ok());
    }

    #[test]
    fn database_url_sslmode_is_honoured() {
        let cfg = DatabaseConfig {
            url: "postgres://u:p@db/vquasar?sslmode=verify-full".to_string(),
            ..Default::default()
        };
        assert_eq!(cfg.effective_ssl_mode(), "verify-full");
        assert!(!cfg.tls_is_optional());
    }

    #[test]
    fn explicit_ssl_mode_wins_over_the_url() {
        let cfg = DatabaseConfig {
            url: "postgres://u:p@db/vquasar?sslmode=disable".to_string(),
            ssl_mode: Some("verify-full".to_string()),
            ..Default::default()
        };
        assert_eq!(cfg.effective_ssl_mode(), "verify-full");
        assert!(!cfg.tls_is_optional());
        // And it reaches the connection options, not just the log line.
        let url = sqlx::ConnectOptions::to_url_lossy(&cfg.connect_options().unwrap()).to_string();
        assert!(url.contains("sslmode=verify-full"), "got {url}");
    }

    #[test]
    fn require_and_verify_modes_are_not_optional() {
        for mode in ["require", "verify-ca", "verify-full", "VERIFY-FULL"] {
            let cfg = DatabaseConfig {
                ssl_mode: Some(mode.to_string()),
                ..Default::default()
            };
            assert!(!cfg.tls_is_optional(), "{mode} should require TLS");
            assert!(cfg.connect_options().is_ok(), "{mode} should parse");
        }
        for mode in ["disable", "allow", "prefer"] {
            let cfg = DatabaseConfig {
                ssl_mode: Some(mode.to_string()),
                ..Default::default()
            };
            assert!(cfg.tls_is_optional(), "{mode} permits plaintext");
        }
    }

    #[test]
    fn bad_ssl_mode_is_a_startup_error() {
        let cfg = DatabaseConfig {
            ssl_mode: Some("verify".to_string()),
            ..Default::default()
        };
        let err = cfg.connect_options().unwrap_err().to_string();
        assert!(err.contains("invalid [database] ssl_mode"), "got {err}");
    }

    #[test]
    fn missing_ca_file_is_a_startup_error_naming_the_file() {
        let cfg = DatabaseConfig {
            ssl_mode: Some("verify-full".to_string()),
            ca: Some("/nonexistent/pg-ca.pem".to_string()),
            ..Default::default()
        };
        let err = cfg.connect_options().unwrap_err().to_string();
        assert!(err.contains("/nonexistent/pg-ca.pem"), "got {err}");
    }

    #[test]
    fn client_cert_without_key_is_rejected() {
        let cfg = DatabaseConfig {
            cert: Some("/tmp/client.crt".to_string()),
            ..Default::default()
        };
        let err = cfg.connect_options().unwrap_err().to_string();
        assert!(err.contains("must be set together"), "got {err}");
    }

    #[test]
    fn env_overrides_apply() {
        figment::Jail::expect_with(|jail| {
            jail.set_env("VQUASAR_CONTROL_SERVER__LISTEN", "127.0.0.1:9000");
            let cfg = ControlConfig::load(None).unwrap();
            assert_eq!(cfg.server.listen, "127.0.0.1:9000");
            Ok(())
        });
    }
}
