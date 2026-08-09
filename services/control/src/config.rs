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
    #[serde(default)]
    pub network: NetworkPolicy,
    #[serde(default)]
    pub tenancy: TenancyConfig,
}

/// Multi-tenancy (design §47, ADR-018).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TenancyConfig {
    /// Whether requests are scoped to a project.
    ///
    /// **Off by default.** Disabled, every caller runs at platform scope and
    /// sees everything, which is exactly what a single-tenant deployment does
    /// today — the schema carries `project_id` either way, but nothing filters
    /// on it. Enabling it is the behavioural change, and it is an operator's
    /// decision because it can hide resources from a caller who could see them
    /// yesterday.
    #[serde(default)]
    pub enabled: bool,
}

/// Platform policy over network segments (design §18, ADR-016).
///
/// VLAN tags and VXLAN VNIs are facts about physical and overlay infrastructure,
/// not caller preferences: a chosen tag lands on whatever provider segment the
/// trunk carries. The platform therefore states which are permissible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    /// How per-NIC policy is resolved (ADR-017):
    ///
    /// * `legacy` — a NIC with no security groups of its own is unfiltered, and
    ///   the agent installs no flows for it. What deployed clusters do today.
    /// * `enforced` — effective policy is the network's default group unioned
    ///   with the NIC's own, so every NIC carries a policy object.
    ///
    /// Defaults to `legacy`: switching to `enforced` moves every existing NIC
    /// from no flows to conntrack flows. Reachability is unchanged (migration
    /// 0017 seeds each network an allow-any default), but it is a real
    /// dataplane change and must be an operator's decision.
    #[serde(default = "default_policy_mode")]
    pub policy_mode: String,
    /// Whether a filtered NIC's egress is default-deny (design §18).
    ///
    /// * `allow` — a guest may originate anything, and an egress rule would be
    ///   a no-op, so the API refuses to record one.
    /// * `enforced` — a guest may originate only what its groups allow. This is
    ///   what keeps a compromised guest off the management underlay, the
    ///   control plane, and other tenants' provider networks.
    ///
    /// Defaults to `allow`: switching cuts every filtered guest off from
    /// everything not explicitly allowed, including DNS and package mirrors,
    /// and that has to be an operator's decision rather than an upgrade's.
    #[serde(default = "default_egress_mode")]
    pub egress_mode: String,
    /// VLAN tags a `vlan` network may use, as inclusive ranges "100-200,300".
    /// Empty ⇒ no VLAN network may be created.
    #[serde(default)]
    pub provider_vlans: String,
    /// Uplink names a physical network may attach to.
    #[serde(default = "default_physical_networks")]
    pub physical_networks: Vec<String>,
    /// Whether the VXLAN underlay is encrypted (design §18, M18b).
    ///
    /// `none` (default) — cleartext tunnels. `reserve` — still cleartext, but
    /// the guest MTU already leaves room for ESP, which is the step that makes
    /// enabling encryption safe on a cluster with running VMs. `ipsec` —
    /// tunnels protected between host pairs.
    ///
    /// Go through `reserve` first: MTU is rendered into cloud-init at seed
    /// time, so a running VM never picks up a new value, and enabling IPsec
    /// before the guests have the smaller MTU blackholes every full-size packet
    /// on the overlay.
    #[serde(default)]
    pub overlay_encryption: crate::overlay::OverlayEncryption,
    /// MTU of the host link the tunnels traverse. The guest MTU is derived from
    /// it; set it for a jumbo-frame underlay.
    #[serde(default = "default_underlay_mtu")]
    pub underlay_mtu: u32,
    /// Whether ESP is wrapped in UDP for NAT traversal (a further 8 bytes).
    #[serde(default)]
    pub overlay_nat_traversal: bool,
    /// Inclusive VXLAN VNI range the allocator draws from.
    #[serde(default = "default_vni_start")]
    pub vni_start: u32,
    #[serde(default = "default_vni_end")]
    pub vni_end: u32,
    /// How long a released VNI is withheld before reuse, in seconds. Guards
    /// against a host that still carries the overlay bridge and tunnel mesh.
    #[serde(default = "default_segment_quarantine_secs")]
    pub segment_quarantine_secs: u64,
}

fn default_policy_mode() -> String {
    "legacy".to_string()
}

fn default_egress_mode() -> String {
    "allow".to_string()
}
fn default_underlay_mtu() -> u32 {
    1500
}
fn default_physical_networks() -> Vec<String> {
    vec!["default".to_string()]
}
fn default_vni_start() -> u32 {
    4096
}
fn default_vni_end() -> u32 {
    16_777_215
}
fn default_segment_quarantine_secs() -> u64 {
    3600
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            policy_mode: default_policy_mode(),
            egress_mode: default_egress_mode(),
            overlay_encryption: crate::overlay::OverlayEncryption::default(),
            underlay_mtu: default_underlay_mtu(),
            overlay_nat_traversal: false,
            provider_vlans: String::new(),
            physical_networks: default_physical_networks(),
            vni_start: default_vni_start(),
            vni_end: default_vni_end(),
            segment_quarantine_secs: default_segment_quarantine_secs(),
        }
    }
}

impl NetworkPolicy {
    /// Whether `tag` is inside the configured allowlist.
    pub fn permits_vlan(&self, tag: i32) -> bool {
        if !(1..=4094).contains(&tag) {
            return false;
        }
        self.provider_vlans.split(',').any(|part| {
            let part = part.trim();
            match part.split_once('-') {
                Some((lo, hi)) => match (lo.trim().parse::<i32>(), hi.trim().parse::<i32>()) {
                    (Ok(lo), Ok(hi)) => (lo..=hi).contains(&tag),
                    _ => false,
                },
                None => part.parse::<i32>().map(|v| v == tag).unwrap_or(false),
            }
        })
    }

    pub fn describe_vlans(&self) -> String {
        if self.provider_vlans.trim().is_empty() {
            "none configured — set [network] provider_vlans".to_string()
        } else {
            self.provider_vlans.clone()
        }
    }

    /// The MTU an overlay guest should use, given the underlay and whether ESP
    /// headroom is reserved (design §18).
    pub fn overlay_guest_mtu(&self) -> u32 {
        crate::overlay::guest_mtu(
            self.underlay_mtu,
            self.overlay_encryption,
            self.overlay_nat_traversal,
        )
    }

    /// Whether a NIC's policy includes its network's default group.
    pub fn policy_enforced(&self) -> bool {
        self.policy_mode == "enforced"
    }

    /// Whether a filtered NIC's egress is default-deny (design §18).
    pub fn egress_enforced(&self) -> bool {
        self.egress_mode == "enforced"
    }

    pub fn permits_uplink(&self, name: &str) -> bool {
        self.physical_networks.iter().any(|n| n == name)
    }

    pub fn segments(&self) -> crate::segments::SegmentPolicy {
        crate::segments::SegmentPolicy {
            vni_start: self.vni_start,
            vni_end: self.vni_end,
            quarantine: std::time::Duration::from_secs(self.segment_quarantine_secs),
        }
    }
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
    /// Roots that a caller-supplied host path (disk, kernel, firmware, image
    /// source) must sit under. The agent opens these files with privilege, so
    /// without confinement `vm:create` is a read primitive over the host
    /// filesystem — including the agent's own key material (design §30).
    #[serde(default = "default_allowed_paths")]
    pub allowed_paths: Vec<String>,
    /// What to do about files in a pool whose owning row is gone (#41).
    ///
    /// `report` (the default) says what is leaking and deletes nothing;
    /// `delete` reclaims; `off` does not look. Deleting files on an operator's
    /// behalf is opted into, not assumed.
    #[serde(default)]
    pub orphan_reclaim: crate::orphans::Policy,
    /// How often to sweep. Reading a shared directory is not free, and an
    /// orphan is not urgent.
    #[serde(default = "default_orphan_sweep_secs")]
    pub orphan_sweep_secs: u64,
    /// How long a file must have gone untouched before it is a candidate. The
    /// guard against sweeping something still being written.
    #[serde(default = "default_orphan_min_age_secs")]
    pub orphan_min_age_secs: u64,
}

fn default_orphan_sweep_secs() -> u64 {
    3600
}

fn default_orphan_min_age_secs() -> u64 {
    3600
}

fn default_allowed_paths() -> Vec<String> {
    vec!["/var/lib/vquasar".to_string()]
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            shared_volumes_dir: "/var/lib/vquasar/shared/volumes".to_string(),
            allowed_paths: default_allowed_paths(),
            orphan_reclaim: crate::orphans::Policy::default(),
            orphan_sweep_secs: default_orphan_sweep_secs(),
            orphan_min_age_secs: default_orphan_min_age_secs(),
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
    /// What this instance calls itself, in the controller lease and on the work
    /// it owns (ADR-021). Defaults to the hostname.
    ///
    /// **Stable across restarts on purpose.** It is how a restarted instance
    /// recognises its own orphaned work — and its own lease, so a restart
    /// resumes leadership immediately instead of waiting out the TTL. The cost
    /// is that two control planes on one host must be given distinct ids, or
    /// they will each believe the other's lease and work are their own.
    #[serde(default)]
    pub instance_id: Option<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8080".to_string(),
            ui_dir: None,
            instance_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// PostgreSQL connection URL.
    pub url: String,
    /// TLS mode for the database connection: `disable`, `allow`, `prefer`,
    /// `require`, `verify-ca` or `verify-full`.
    ///
    /// **Defaults to `require`**: the connection carries every secret the
    /// platform holds, so it is encrypted unless someone deliberately says
    /// otherwise. libpq's own default is `prefer`, which silently falls back to
    /// plaintext when the server does not offer TLS — an exposure that looks
    /// identical to success.
    ///
    /// `require` guarantees encryption but not the server's identity. Set
    /// `verify-full` and a `ca` for that, which is what a real deployment
    /// should do; it needs the certificate's SAN to match the host in `url`.
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

/// Encryption is the default. Overriding it is a deliberate act, and the
/// startup log says so when the effective mode permits plaintext.
const DEFAULT_SSL_MODE: &str = "require";

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

        // `effective_ssl_mode` already resolves explicit config → URL → our
        // default, so applying it unconditionally is both simpler and the only
        // way the default actually reaches the driver.
        let effective = Some(self.effective_ssl_mode());
        if let Some(mode) = &effective {
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
    /// setting, else the URL's `sslmode`, else our `require` default.
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
            .unwrap_or_else(|| DEFAULT_SSL_MODE.to_string())
    }

    /// Whether the effective mode allows the connection to end up unencrypted.
    pub fn tls_is_optional(&self) -> bool {
        PLAINTEXT_CAPABLE_SSL_MODES.contains(&self.effective_ssl_mode().as_str())
    }
}

// There is deliberately no control-plane gRPC listener. A `[grpc] listen` was
// defined here, documented in control.toml and asserted in a test — and never
// bound by anything: the control plane *dials* the agents, and nothing dials it
// over gRPC. Removed rather than left as a value an operator can set, because
// the visible harm is a firewall rule opened for a port nothing answers on
// (ADR-024 in spirit: accepted and ignored is a lie).

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
        assert_eq!(cfg.logging.level, "info");
    }

    /// Absent config must not change how we connect — the default stays
    /// whatever the URL/libpq says, so existing deployments are unaffected.
    #[test]
    fn vlan_allowlist_parses_ranges_and_singletons() {
        let p = NetworkPolicy {
            provider_vlans: "100-200, 300, 400-401".to_string(),
            ..Default::default()
        };
        assert!(p.permits_vlan(100) && p.permits_vlan(200) && p.permits_vlan(150));
        assert!(p.permits_vlan(300) && p.permits_vlan(401));
        assert!(!p.permits_vlan(99) && !p.permits_vlan(201) && !p.permits_vlan(301));
        // Out-of-spec tags are never permitted, whatever the config says.
        assert!(!p.permits_vlan(0) && !p.permits_vlan(4095));
    }

    /// An upgrade must not change the dataplane: policy enforcement is opt-in,
    /// exactly like port security on the agent.
    #[test]
    fn policy_enforcement_is_opt_in() {
        assert!(!NetworkPolicy::default().policy_enforced());
        let p = NetworkPolicy {
            policy_mode: "enforced".to_string(),
            ..Default::default()
        };
        assert!(p.policy_enforced());
    }

    /// Absent config permits nothing: a VLAN tag has to be a deliberate
    /// statement about the physical switch.
    #[test]
    fn no_vlan_is_permitted_by_default() {
        let p = NetworkPolicy::default();
        assert!(!p.permits_vlan(100));
        assert!(p.describe_vlans().contains("none configured"));
        assert!(p.permits_uplink("default"));
        assert!(!p.permits_uplink("dmz"));
    }

    /// Encryption is the default: this connection carries every secret the
    /// platform holds, so plaintext has to be asked for explicitly.
    #[test]
    fn database_tls_is_required_by_default() {
        let cfg = DatabaseConfig::default();
        assert_eq!(cfg.effective_ssl_mode(), "require");
        assert!(!cfg.tls_is_optional());
        let url = sqlx::ConnectOptions::to_url_lossy(&cfg.connect_options().unwrap()).to_string();
        assert!(url.contains("sslmode=require"), "got {url}");
    }

    /// Opting out is still possible, but it is a deliberate act.
    #[test]
    fn plaintext_must_be_asked_for_explicitly() {
        let cfg = DatabaseConfig {
            ssl_mode: Some("disable".to_string()),
            ..Default::default()
        };
        assert!(cfg.tls_is_optional());
        let url = sqlx::ConnectOptions::to_url_lossy(&cfg.connect_options().unwrap()).to_string();
        assert!(url.contains("sslmode=disable"), "got {url}");
    }

    /// A `sslmode=` already in the URL keeps working and is not overridden.
    #[test]
    fn a_url_sslmode_still_wins_over_the_default() {
        let cfg = DatabaseConfig {
            url: "postgres://u:p@db/vquasar?sslmode=disable".to_string(),
            ..Default::default()
        };
        assert_eq!(cfg.effective_ssl_mode(), "disable");
        let url = sqlx::ConnectOptions::to_url_lossy(&cfg.connect_options().unwrap()).to_string();
        assert!(url.contains("sslmode=disable"), "got {url}");
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
