//! PostgreSQL persistence (design document, section 31).
//!
//! Explicit SQL via `sqlx` runtime queries — no ORM and no compile-time database
//! dependency. State transitions run inside the methods here; the `generation`
//! columns exist for optimistic concurrency as the controllers mature.

use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;
use vquasar_model::{BootSpec, CloudInitSpec, VirtualMachineSpec};

/// A registered host row.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Host {
    pub id: Uuid,
    pub name: String,
    pub endpoint: String,
    pub schedulable: bool,
    pub state: String,
    pub hostname: Option<String>,
    pub architecture: Option<String>,
    pub kernel_version: Option<String>,
    pub cloud_hypervisor_version: Option<String>,
    pub logical_cpus: Option<i32>,
    pub cpu_model: Option<String>,
    pub cpu_vendor: Option<String>,
    /// CN of this host's agent certificate, for IPsec peer pinning (M18b).
    #[sqlx(default)]
    pub cert_cn: Option<String>,
    /// VNIs this host currently carries an overlay bridge for (design §18).
    #[sqlx(default)]
    pub overlay_vnis: Vec<i32>,
    #[sqlx(default)]
    pub cpu_features: Vec<String>,
    pub total_memory_bytes: Option<i64>,
    pub available_memory_bytes: Option<i64>,
    pub vm_count: i32,
    pub last_heartbeat: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub generation: i64,
}

/// A virtual machine row (desired `spec` + observed columns).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Vm {
    pub id: Uuid,
    pub name: String,
    pub spec: Json<VirtualMachineSpec>,
    pub phase: String,
    pub host_id: Option<Uuid>,
    pub observed_generation: i64,
    pub message: Option<String>,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub generation: i64,
}

/// A project row (design §47, ADR-018).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What a project still owns, for the refusal message when deleting it.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ProjectContents {
    pub vms: i64,
    pub volumes: i64,
    pub templates: i64,
    pub security_groups: i64,
    pub networks: i64,
}

impl ProjectContents {
    pub fn is_empty(&self) -> bool {
        self.vms == 0
            && self.volumes == 0
            && self.templates == 0
            && self.security_groups == 0
            && self.networks == 0
    }

    /// A human-readable summary, so the caller learns *what* is in the way.
    pub fn summary(&self) -> String {
        [
            (self.vms, "VM"),
            (self.volumes, "volume"),
            (self.templates, "template"),
            (self.security_groups, "security group"),
            (self.networks, "network"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}{}", if *n == 1 { "" } else { "s" }))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// A virtual network row.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Network {
    pub id: Uuid,
    pub name: String,
    /// What this network is, and therefore what it isolates (design §18):
    /// `provider` | `vlan` | `tenant`. See [`vquasar_model::NetworkKind`].
    #[sqlx(default)]
    pub kind: String,
    /// Uplink name for a physical (provider/vlan) network.
    #[sqlx(default)]
    pub physical_network: Option<String>,
    /// The L2 segment this network occupies; unique fleet-wide. `None` for a
    /// network predating the kind model — grandfathered, possibly sharing a
    /// broadcast domain with another (ADR-016).
    #[sqlx(default)]
    pub segment_key: Option<String>,
    /// Predates the kind model: its segment is not guaranteed distinct.
    #[sqlx(default)]
    pub legacy_segment: bool,
    /// Policy applied to every NIC on this network, unioned with the NIC's own
    /// groups (ADR-017). `None` only for a network created before 0017.
    #[sqlx(default)]
    pub default_security_group_id: Option<Uuid>,
    /// 802.1Q VLAN tag; `None` is a flat/untagged provider network.
    pub vlan: Option<i32>,
    /// VXLAN VNI (design M13b): `Some` ⇒ this network is a VXLAN overlay,
    /// isolated by VNI and spanning hosts over the underlay. Mutually exclusive
    /// with `vlan`.
    pub vni: Option<i32>,
    // IPAM (design M13a): a family is managed (static, control-plane IPAM) when
    // its cidr is set; otherwise that family is left to external DHCP.
    pub cidr_v4: Option<String>,
    pub gateway_v4: Option<String>,
    pub cidr_v6: Option<String>,
    pub gateway_v6: Option<String>,
    pub dns: Vec<String>,
    pub pool_v4_start: Option<String>,
    pub pool_v4_end: Option<String>,
    pub pool_v6_start: Option<String>,
    pub pool_v6_end: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Network {
    /// Whether any family is under control-plane IPAM (vs external DHCP).
    pub fn is_managed(&self) -> bool {
        self.cidr_v4.is_some() || self.cidr_v6.is_some()
    }

    /// Whether this network is a VXLAN overlay (design M13b).
    pub fn is_overlay(&self) -> bool {
        self.vni.is_some()
    }
}

/// IPAM fields for creating/updating a network (design M13a).
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct NetworkIpam {
    pub cidr_v4: Option<String>,
    pub gateway_v4: Option<String>,
    pub cidr_v6: Option<String>,
    pub gateway_v6: Option<String>,
    #[serde(default)]
    pub dns: Vec<String>,
    pub pool_v4_start: Option<String>,
    pub pool_v4_end: Option<String>,
    pub pool_v6_start: Option<String>,
    pub pool_v6_end: Option<String>,
}

/// A persisted IP assignment (design M13a).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct IpAllocation {
    pub id: Uuid,
    pub network_id: Uuid,
    pub ip: String,
    pub family: i16,
    pub vm_id: Option<Uuid>,
    pub nic_index: i32,
    pub mac: String,
    pub created_at: DateTime<Utc>,
}

/// A first-class volume (design M14a).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Volume {
    pub id: Uuid,
    pub name: String,
    pub size_bytes: i64,
    pub format: String,
    pub attached_vm_id: Option<Uuid>,
    pub attached_serial: Option<i32>,
    /// Image this volume was cloned from (design M14d); `Some` ⇒ bootable.
    pub source_image_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A point-in-time volume snapshot (design M14c).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct VolumeSnapshot {
    pub id: Uuid,
    pub volume_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
}

/// A security group (design M13c).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct SecurityGroup {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One rule in a security group (design M13c).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct SecurityGroupRule {
    pub id: Uuid,
    pub security_group_id: Uuid,
    pub direction: String,
    pub ethertype: String,
    pub protocol: String,
    pub port_min: Option<i32>,
    pub port_max: Option<i32>,
    pub remote_cidr: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A base image row: a read-only golden disk + boot recipe (design M9).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Image {
    pub id: Uuid,
    pub name: String,
    pub source_path: String,
    pub format: String,
    pub boot: Json<BootSpec>,
    pub default_size_bytes: Option<i64>,
    pub cloud_init: bool,
    pub os: Option<String>,
    /// Lifecycle status (design M14b): `ready` | `importing` | `failed`.
    pub status: String,
    /// Whether the platform owns the backing file (imported vs registered).
    pub managed: bool,
    pub size_bytes: Option<i64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A VM template row: a reusable preset instantiated into a spec (design M9).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Template {
    pub id: Uuid,
    pub name: String,
    pub image_id: Uuid,
    pub boot_vcpus: i32,
    pub max_vcpus: i32,
    pub memory_mib: i64,
    pub disk_size_bytes: Option<i64>,
    pub disk_format: String,
    pub network_id: Option<Uuid>,
    pub cloud_init: Option<Json<CloudInitSpec>>,
    /// Machine profile for VMs created from this template: "standard" or
    /// "microvm" (design M15).
    #[sqlx(default)]
    pub machine_type: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A user row (identity mirrored from OIDC; roles hang off it — design M12b).
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub subject: String,
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub is_active: bool,
    pub last_login: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A role row.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Role {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub builtin: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An asynchronous task row.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Task {
    pub id: Uuid,
    pub task_type: String,
    pub state: String,
    pub progress: i32,
    pub vm_id: Option<Uuid>,
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A live-migration record.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Migration {
    pub id: Uuid,
    pub vm_id: Uuid,
    pub source_host_id: Option<Uuid>,
    pub target_host_id: Uuid,
    pub state: String,
    pub migration_url: Option<String>,
    pub task_id: Option<Uuid>,
    pub message: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An event row.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Event {
    pub id: Uuid,
    pub ts: DateTime<Utc>,
    pub resource_type: String,
    pub resource_id: Option<Uuid>,
    pub event_type: String,
    pub severity: String,
    pub message: String,
    pub metadata: Option<serde_json::Value>,
}

/// Inventory reported by an agent's `GetHostInfo`, used to refresh a host row.
#[derive(Debug, Clone, Default)]
pub struct HostInventory {
    /// VNIs the host reports carrying an overlay bridge for (design §18).
    pub overlay_vnis: Vec<i32>,
    pub hostname: Option<String>,
    pub architecture: Option<String>,
    pub kernel_version: Option<String>,
    pub cloud_hypervisor_version: Option<String>,
    pub logical_cpus: Option<i32>,
    pub cpu_model: Option<String>,
    pub cpu_vendor: Option<String>,
    pub cpu_features: Vec<String>,
    pub total_memory_bytes: Option<i64>,
    pub available_memory_bytes: Option<i64>,
    pub vm_count: i32,
}

/// The persistence layer.
#[derive(Clone)]
pub struct Store {
    pool: PgPool,
    /// Shared-storage directory for per-VM provisioned volumes (design M9).
    shared_volumes_dir: std::sync::Arc<str>,
    /// Field-encryption keyring (design M12c); `None` = plaintext at rest.
    crypto: Option<std::sync::Arc<crate::crypto::Cryptor>>,
    /// Roots a caller-supplied host path must sit under (design §30).
    allowed_paths: std::sync::Arc<[String]>,
    /// Platform policy over network segments (design §18).
    network_policy: std::sync::Arc<crate::config::NetworkPolicy>,
}

type Result<T> = std::result::Result<T, sqlx::Error>;

fn crypto_err(e: crate::crypto::CryptoError) -> sqlx::Error {
    sqlx::Error::Protocol(format!("field encryption: {e}"))
}

/// Whether any sensitive cloud-init field is still stored in plaintext.
fn needs_sealing(ci: &CloudInitSpec) -> bool {
    ci.password
        .as_deref()
        .is_some_and(|v| !crate::crypto::is_sealed(v))
        || ci
            .user_data
            .as_deref()
            .is_some_and(|v| !crate::crypto::is_sealed(v))
        || ci
            .ssh_authorized_keys
            .iter()
            .any(|v| !crate::crypto::is_sealed(v))
}

impl Store {
    pub fn new(pool: PgPool, shared_volumes_dir: impl Into<String>) -> Self {
        Self {
            pool,
            shared_volumes_dir: shared_volumes_dir.into().into(),
            crypto: None,
            allowed_paths: vec!["/var/lib/vquasar".to_string()].into(),
            network_policy: std::sync::Arc::new(crate::config::NetworkPolicy::default()),
        }
    }

    /// Platform policy over VLAN tags, uplinks and VNI allocation (design §18).
    pub fn with_network_policy(mut self, policy: crate::config::NetworkPolicy) -> Self {
        self.network_policy = std::sync::Arc::new(policy);
        self
    }

    pub fn network_policy(&self) -> &crate::config::NetworkPolicy {
        &self.network_policy
    }

    /// Unseal a VM for a caller (design M12c). Exposed to the scoped layer so
    /// it runs the same query-then-open path as the unscoped one.
    pub(crate) fn open_vms_public(&self, vms: Vec<Vm>) -> Result<Vec<Vm>> {
        self.open_vms(vms)
    }

    pub(crate) fn open_vm_opt_public(&self, vm: Option<Vm>) -> Result<Option<Vm>> {
        self.open_vm_opt(vm)
    }

    /// The connection pool, for helpers that own their own SQL (segments).
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Begin a transaction, for work that must be atomic with a row insert
    /// (segment allocation).
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, sqlx::Postgres>> {
        self.pool.begin().await
    }

    /// Seed a network's default policy group (ADR-017).
    ///
    /// A tenant network is self-contained, so its default is deny-ingress: the
    /// segment already isolates it, and anything more open should be asked for.
    /// A physical network's default is deny too — a brand-new segment has no
    /// reason to be open — but it is created empty of allow rules either way;
    /// the difference is only the description an operator reads.
    pub async fn insert_default_group(
        &self,
        _network: Uuid,
        network_name: &str,
        tenant: bool,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();
        let now = Utc::now();
        let scope = if tenant { "tenant" } else { "provider" };
        sqlx::query(
            "INSERT INTO security_groups (id, name, description, managed, created_at, updated_at)
             VALUES ($1,$2,$3,true,$4,$4)",
        )
        .bind(id)
        .bind(format!("default-{network_name}"))
        .bind(format!(
            "Default policy for the {scope} network {network_name}: default-deny ingress. \
             Applies to every NIC on this network, unioned with the NIC's own groups."
        ))
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// Restrict caller-supplied host paths to these roots (design §30).
    pub fn with_allowed_paths(mut self, roots: Vec<String>) -> Self {
        self.allowed_paths = roots.into();
        self
    }

    /// The roots a caller-supplied host path must sit under.
    pub fn allowed_paths(&self) -> &[String] {
        &self.allowed_paths
    }

    /// Attach a field-encryption keyring (design M12c).
    pub fn with_crypto(mut self, crypto: Option<crate::crypto::Cryptor>) -> Self {
        self.crypto = crypto.map(std::sync::Arc::new);
        self
    }

    /// The configured shared-storage directory for provisioned volumes.
    pub fn shared_volumes_dir(&self) -> &str {
        &self.shared_volumes_dir
    }

    // ---- field encryption at rest (design M12c) --------------------------

    /// Return a copy of `spec` with sensitive cloud-init fields sealed (no-op
    /// when encryption is disabled).
    fn seal_spec(&self, spec: &VirtualMachineSpec) -> Result<VirtualMachineSpec> {
        let mut out = spec.clone();
        if let (Some(c), Some(ci)) = (&self.crypto, out.cloud_init.as_mut()) {
            c.seal_cloud_init(ci).map_err(crypto_err)?;
        }
        Ok(out)
    }

    /// Decrypt a VM's sensitive cloud-init fields in place.
    fn open_vm(&self, vm: &mut Vm) -> Result<()> {
        if let (Some(c), Some(ci)) = (&self.crypto, vm.spec.0.cloud_init.as_mut()) {
            c.open_cloud_init(ci).map_err(crypto_err)?;
        }
        Ok(())
    }

    fn open_vm_opt(&self, vm: Option<Vm>) -> Result<Option<Vm>> {
        match vm {
            Some(mut v) => {
                self.open_vm(&mut v)?;
                Ok(Some(v))
            }
            None => Ok(None),
        }
    }

    fn open_vms(&self, mut vms: Vec<Vm>) -> Result<Vec<Vm>> {
        for v in vms.iter_mut() {
            self.open_vm(v)?;
        }
        Ok(vms)
    }

    /// Return a sealed copy of an optional cloud-init spec for a template.
    fn seal_ci(&self, ci: Option<&CloudInitSpec>) -> Result<Option<CloudInitSpec>> {
        match (&self.crypto, ci) {
            (Some(c), Some(ci)) => {
                let mut out = ci.clone();
                c.seal_cloud_init(&mut out).map_err(crypto_err)?;
                Ok(Some(out))
            }
            (_, other) => Ok(other.cloned()),
        }
    }

    fn open_template(&self, t: &mut Template) -> Result<()> {
        if let (Some(c), Some(ci)) = (&self.crypto, t.cloud_init.as_mut().map(|j| &mut j.0)) {
            c.open_cloud_init(ci).map_err(crypto_err)?;
        }
        Ok(())
    }

    fn open_template_opt(&self, t: Option<Template>) -> Result<Option<Template>> {
        match t {
            Some(mut t) => {
                self.open_template(&mut t)?;
                Ok(Some(t))
            }
            None => Ok(None),
        }
    }

    /// One-time sweep: seal any VM/template rows whose sensitive cloud-init
    /// fields are still stored in plaintext (design M12c). Idempotent —
    /// already-sealed values are skipped — and it does not bump `generation`,
    /// so it won't trigger a reconcile. Returns the number of rows sealed.
    pub async fn encrypt_existing(&self) -> Result<usize> {
        let Some(c) = self.crypto.clone() else {
            return Ok(0);
        };
        let mut sealed = 0usize;

        // VMs — read raw (no open) so we can tell plaintext from ciphertext.
        let vms = sqlx::query_as::<_, Vm>("SELECT * FROM virtual_machines")
            .fetch_all(&self.pool)
            .await?;
        for vm in vms {
            let Some(ci) = vm.spec.0.cloud_init.as_ref() else {
                continue;
            };
            if !needs_sealing(ci) {
                continue;
            }
            let mut spec = vm.spec.0.clone();
            c.seal_cloud_init(spec.cloud_init.as_mut().unwrap())
                .map_err(crypto_err)?;
            sqlx::query("UPDATE virtual_machines SET spec=$2 WHERE id=$1")
                .bind(vm.id)
                .bind(Json(&spec))
                .execute(&self.pool)
                .await?;
            sealed += 1;
        }

        // Templates.
        let templates = sqlx::query_as::<_, Template>("SELECT * FROM templates")
            .fetch_all(&self.pool)
            .await?;
        for t in templates {
            let Some(ci) = t.cloud_init.as_ref().map(|j| &j.0) else {
                continue;
            };
            if !needs_sealing(ci) {
                continue;
            }
            let mut ci2 = ci.clone();
            c.seal_cloud_init(&mut ci2).map_err(crypto_err)?;
            sqlx::query("UPDATE templates SET cloud_init=$2 WHERE id=$1")
                .bind(t.id)
                .bind(Json(&ci2))
                .execute(&self.pool)
                .await?;
            sealed += 1;
        }
        Ok(sealed)
    }

    /// Apply embedded migrations.
    pub async fn migrate(&self) -> anyhow::Result<()> {
        sqlx::migrate!("../../migrations").run(&self.pool).await?;
        Ok(())
    }

    // ---- hosts -----------------------------------------------------------

    pub async fn register_host(&self, name: &str, endpoint: &str) -> Result<Host> {
        let now = Utc::now();
        let id = Uuid::new_v4();
        sqlx::query_as::<_, Host>(
            "INSERT INTO hosts (id, name, endpoint, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)
             RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(endpoint)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn get_host(&self, id: Uuid) -> Result<Option<Host>> {
        sqlx::query_as::<_, Host>("SELECT * FROM hosts WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_hosts(&self) -> Result<Vec<Host>> {
        sqlx::query_as::<_, Host>("SELECT * FROM hosts ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
    }

    /// Refresh a host's observed inventory and mark it `Ready`.
    pub async fn update_host_ready(&self, id: Uuid, inv: &HostInventory) -> Result<()> {
        let now = Utc::now();
        sqlx::query(
            "UPDATE hosts SET state='Ready', hostname=$2, architecture=$3, kernel_version=$4,
                cloud_hypervisor_version=$5, logical_cpus=$6, cpu_model=$7, total_memory_bytes=$8,
                available_memory_bytes=$9, vm_count=$10, last_heartbeat=$11, updated_at=$11,
                cpu_vendor=$12, cpu_features=$13, overlay_vnis=$14
             WHERE id=$1",
        )
        .bind(id)
        .bind(&inv.hostname)
        .bind(&inv.architecture)
        .bind(&inv.kernel_version)
        .bind(&inv.cloud_hypervisor_version)
        .bind(inv.logical_cpus)
        .bind(&inv.cpu_model)
        .bind(inv.total_memory_bytes)
        .bind(inv.available_memory_bytes)
        .bind(inv.vm_count)
        .bind(now)
        .bind(&inv.cpu_vendor)
        .bind(&inv.cpu_features)
        .bind(&inv.overlay_vnis)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Store a one-time enrollment token (SHA-256 hash) for a host (design M16).
    pub async fn insert_enrollment_token(
        &self,
        host_id: Uuid,
        token_hash: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO enrollment_tokens (id, host_id, token_hash, expires_at)
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(host_id)
        .bind(token_hash)
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Atomically consume a valid (unused, unexpired) enrollment token, marking
    /// it used and returning the host it enrolls. `None` if invalid (design M16).
    pub async fn consume_enrollment_token(&self, token_hash: &str) -> Result<Option<Uuid>> {
        let row: Option<(Uuid,)> = sqlx::query_as(
            "UPDATE enrollment_tokens SET used_at = now()
             WHERE token_hash = $1 AND used_at IS NULL AND expires_at > now()
             RETURNING host_id",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|(h,)| h))
    }

    /// Set a host's admin `schedulable` flag (design M15, host lifecycle:
    /// cordon/uncordon). `false` = maintenance mode — the scheduler places no
    /// new VMs here, but running VMs keep running until drained. Preserved
    /// across heartbeats (`update_host_ready` never touches this column).
    pub async fn set_host_schedulable(&self, id: Uuid, schedulable: bool) -> Result<bool> {
        let res = sqlx::query("UPDATE hosts SET schedulable=$2, updated_at=$3 WHERE id=$1")
            .bind(id)
            .bind(schedulable)
            .bind(Utc::now())
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Mark a host unreachable. VMs are **not** relocated (ADR-014, section 27).
    pub async fn mark_host_not_ready(&self, id: Uuid) -> Result<()> {
        sqlx::query("UPDATE hosts SET state='NotReady', updated_at=$2 WHERE id=$1")
            .bind(id)
            .bind(Utc::now())
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    /// Hosts eligible for scheduling: Ready and schedulable.
    pub async fn list_schedulable_hosts(&self) -> Result<Vec<Host>> {
        sqlx::query_as::<_, Host>(
            "SELECT * FROM hosts WHERE state='Ready' AND schedulable=TRUE ORDER BY name",
        )
        .fetch_all(&self.pool)
        .await
    }

    // ---- virtual machines ------------------------------------------------

    /// Insert a VM with a caller-chosen id. Used when the spec must reference the
    /// id before persistence (e.g. a provisioned volume path — design M9).
    /// Insert a VM into `project` (design §47). Callers pass the request's
    /// scope; platform scope means the default project, because a resource must
    /// belong to exactly one and "everything" is not a home.
    pub async fn insert_vm_with_id(
        &self,
        id: Uuid,
        name: &str,
        spec: &VirtualMachineSpec,
        project: Uuid,
    ) -> Result<Vm> {
        let now = Utc::now();
        let sealed = self.seal_spec(spec)?;
        let vm = sqlx::query_as::<_, Vm>(
            // The phone_home secret is issued here rather than lazily: a VM
            // that exists but has no token yet would have an unauthenticated
            // window on first boot, which is exactly the window that matters
            // (design M13e).
            "INSERT INTO virtual_machines
                (id, name, spec, phone_home_token, project_id, created_at, updated_at)
             VALUES ($1, $2, $3, $5, $6, $4, $4)
             RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(Json(&sealed))
        .bind(now)
        .bind(crate::crypto::random_token())
        .bind(project)
        .fetch_one(&self.pool)
        .await?;
        // Return plaintext to the caller (the row is sealed at rest).
        self.open_vm_opt(Some(vm)).map(|o| o.unwrap())
    }

    pub async fn get_vm(&self, id: Uuid) -> Result<Option<Vm>> {
        let vm = sqlx::query_as::<_, Vm>("SELECT * FROM virtual_machines WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        self.open_vm_opt(vm)
    }

    pub async fn list_vms(&self) -> Result<Vec<Vm>> {
        let vms = sqlx::query_as::<_, Vm>("SELECT * FROM virtual_machines ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        self.open_vms(vms)
    }

    /// VMs whose observed state has not caught up with desired state and are
    /// therefore candidates for reconciliation (section 32).
    pub async fn list_vms_to_reconcile(&self) -> Result<Vec<Vm>> {
        let vms = sqlx::query_as::<_, Vm>(
            "SELECT * FROM virtual_machines
             WHERE phase NOT IN ('Running', 'Stopped', 'Failed')
                OR generation <> observed_generation
             ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        // Reconciler needs plaintext cloud-init to build the seed ISO.
        self.open_vms(vms)
    }

    /// Set the desired power state on an existing VM (bumps generation so the
    /// controller reconciles it).
    pub async fn set_vm_spec(&self, id: Uuid, spec: &VirtualMachineSpec) -> Result<Option<Vm>> {
        let sealed = self.seal_spec(spec)?;
        let vm = sqlx::query_as::<_, Vm>(
            "UPDATE virtual_machines
             SET spec=$2, generation=generation+1, updated_at=$3
             WHERE id=$1
             RETURNING *",
        )
        .bind(id)
        .bind(Json(&sealed))
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await?;
        self.open_vm_opt(vm)
    }

    /// Update a VM's name and/or spec, bumping generation so the controller
    /// reconciles the change (design M10 editing). `name = None` keeps the name.
    pub async fn update_vm(
        &self,
        id: Uuid,
        name: Option<&str>,
        spec: &VirtualMachineSpec,
    ) -> Result<Option<Vm>> {
        let sealed = self.seal_spec(spec)?;
        let vm = sqlx::query_as::<_, Vm>(
            "UPDATE virtual_machines
             SET name = COALESCE($2, name), spec=$3, generation=generation+1, updated_at=$4
             WHERE id=$1
             RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(Json(&sealed))
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await?;
        self.open_vm_opt(vm)
    }

    pub async fn assign_vm_host(&self, id: Uuid, host_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE virtual_machines SET host_id=$2, phase='Scheduling', updated_at=$3 WHERE id=$1",
        )
        .bind(id)
        .bind(host_id)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Record observed state after reconciliation.
    pub async fn update_vm_observed(
        &self,
        id: Uuid,
        phase: &str,
        observed_generation: i64,
        message: Option<&str>,
        ip_address: Option<&str>,
    ) -> Result<()> {
        // Keep the last-known IP when this tick didn't discover one (COALESCE),
        // so a transient miss doesn't blank the address in the UI (design M11).
        sqlx::query(
            "UPDATE virtual_machines
             SET phase=$2, observed_generation=$3, message=$4,
                 ip_address=COALESCE($5, ip_address), updated_at=$6
             WHERE id=$1",
        )
        .bind(id)
        .bind(phase)
        .bind(observed_generation)
        .bind(message)
        .bind(ip_address)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Update a VM's discovered IP (design M11). No-op when unchanged, so it
    /// doesn't churn `updated_at` every tick.
    pub async fn set_vm_ip(&self, id: Uuid, ip: &str) -> Result<()> {
        sqlx::query(
            "UPDATE virtual_machines SET ip_address=$2, updated_at=$3
             WHERE id=$1 AND ip_address IS DISTINCT FROM $2",
        )
        .bind(id)
        .bind(ip)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn set_vm_phase(&self, id: Uuid, phase: &str) -> Result<()> {
        sqlx::query("UPDATE virtual_machines SET phase=$2, updated_at=$3 WHERE id=$1")
            .bind(id)
            .bind(phase)
            .bind(Utc::now())
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    pub async fn delete_vm_row(&self, id: Uuid) -> Result<()> {
        // Free any IP allocations this VM held (design M13a) before removing it.
        self.release_vm_allocations(id).await?;
        // Detach (but keep) any first-class volumes — they outlive the VM (M14a).
        sqlx::query(
            "UPDATE volumes SET attached_vm_id=NULL, attached_serial=NULL WHERE attached_vm_id=$1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        sqlx::query("DELETE FROM virtual_machines WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    // ---- networks --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_network(
        &self,
        name: &str,
        kind: &str,
        physical_network: Option<&str>,
        segment_key: Option<&str>,
        vlan: Option<i32>,
        vni: Option<i32>,
        ipam: &NetworkIpam,
    ) -> Result<Network> {
        let now = Utc::now();
        sqlx::query_as::<_, Network>(
            "INSERT INTO networks
                (id, name, kind, physical_network, segment_key, vlan, vni,
                 cidr_v4, gateway_v4, cidr_v6, gateway_v6, dns,
                 pool_v4_start, pool_v4_end, pool_v6_start, pool_v6_end,
                 created_at, updated_at)
             VALUES ($1,$2,$15,$16,$17,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$14)
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(vlan)
        .bind(vni)
        .bind(&ipam.cidr_v4)
        .bind(&ipam.gateway_v4)
        .bind(&ipam.cidr_v6)
        .bind(&ipam.gateway_v6)
        .bind(&ipam.dns)
        .bind(&ipam.pool_v4_start)
        .bind(&ipam.pool_v4_end)
        .bind(&ipam.pool_v6_start)
        .bind(&ipam.pool_v6_end)
        .bind(now)
        .bind(kind)
        .bind(physical_network)
        .bind(segment_key)
        .fetch_one(&self.pool)
        .await
    }

    // ---- projects (design §47, ADR-018) ----------------------------------

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        sqlx::query_as::<_, Project>("SELECT * FROM projects ORDER BY name")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_project(&self, id: Uuid) -> Result<Option<Project>> {
        sqlx::query_as::<_, Project>("SELECT * FROM projects WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn insert_project(&self, name: &str, description: Option<&str>) -> Result<Project> {
        let now = Utc::now();
        sqlx::query_as::<_, Project>(
            "INSERT INTO projects (id, name, description, is_default, created_at, updated_at)
             VALUES ($1,$2,$3,false,$4,$4) RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(description)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn update_project(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
    ) -> Result<Option<Project>> {
        sqlx::query_as::<_, Project>(
            "UPDATE projects SET name=$2, description=$3, updated_at=$4
              WHERE id=$1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn delete_project(&self, id: Uuid) -> Result<bool> {
        Ok(
            sqlx::query("DELETE FROM projects WHERE id=$1 AND NOT is_default")
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }

    /// What a project still owns.
    ///
    /// Deletion is refused while anything remains rather than cascading:
    /// deleting a project's VMs is a long, agent-touching, restartable
    /// operation, and a DELETE that quietly starts one would be the wrong shape
    /// entirely (design §7).
    pub async fn project_contents(&self, id: Uuid) -> Result<ProjectContents> {
        let row: (i64, i64, i64, i64, i64) = sqlx::query_as(
            "SELECT (SELECT count(*) FROM virtual_machines WHERE project_id=$1),
                    (SELECT count(*) FROM volumes          WHERE project_id=$1),
                    (SELECT count(*) FROM templates        WHERE project_id=$1),
                    (SELECT count(*) FROM security_groups  WHERE project_id=$1),
                    (SELECT count(*) FROM networks         WHERE project_id=$1)",
        )
        .bind(id)
        .fetch_one(&self.pool)
        .await?;
        Ok(ProjectContents {
            vms: row.0,
            volumes: row.1,
            templates: row.2,
            security_groups: row.3,
            networks: row.4,
        })
    }

    /// The phone_home secret for a VM, generated at creation (design M13e).
    pub async fn phone_home_token(&self, vm: Uuid) -> Result<Option<String>> {
        sqlx::query_scalar::<_, Option<String>>(
            "SELECT phone_home_token FROM virtual_machines WHERE id=$1",
        )
        .bind(vm)
        .fetch_optional(&self.pool)
        .await
        .map(Option::flatten)
    }

    /// Give a VM a phone_home secret if it does not have one yet, returning it.
    ///
    /// Idempotent so a VM created before this existed picks one up on its next
    /// reconcile rather than staying unauthenticated forever.
    pub async fn ensure_phone_home_token(&self, vm: Uuid) -> Result<String> {
        if let Some(existing) = self.phone_home_token(vm).await? {
            return Ok(existing);
        }
        let token = crate::crypto::random_token();
        sqlx::query("UPDATE virtual_machines SET phone_home_token=$2 WHERE id=$1")
            .bind(vm)
            .bind(&token)
            .execute(&self.pool)
            .await?;
        Ok(token)
    }

    /// Record the CN of a host's agent certificate, so overlay IPsec can pin
    /// this peer's identity (M18b).
    pub async fn set_host_cert_cn(&self, host: Uuid, cn: &str) -> Result<()> {
        sqlx::query("UPDATE hosts SET cert_cn = $2 WHERE id = $1")
            .bind(host)
            .bind(cn)
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    /// Attach a network's default policy group (ADR-017).
    pub async fn set_network_default_group(&self, network: Uuid, sg: Uuid) -> Result<()> {
        sqlx::query("UPDATE networks SET default_security_group_id=$2, updated_at=$3 WHERE id=$1")
            .bind(network)
            .bind(sg)
            .bind(Utc::now())
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    pub async fn get_network(&self, id: Uuid) -> Result<Option<Network>> {
        sqlx::query_as::<_, Network>("SELECT * FROM networks WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_networks(&self) -> Result<Vec<Network>> {
        sqlx::query_as::<_, Network>("SELECT * FROM networks ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn delete_network(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM networks WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    #[allow(clippy::too_many_arguments)]
    /// Give a grandfathered network a real L2 segment (design §18, ADR-016).
    ///
    /// Only ever called for a network whose `segment_key` is NULL. The unique
    /// index is what makes this meaningful: if another network already occupies
    /// the segment, this fails, which is the correct answer — they are the same
    /// broadcast domain and only one row can describe it.
    pub async fn adopt_network_segment(
        &self,
        id: Uuid,
        physical_network: &str,
        vlan: Option<i32>,
        segment_key: &str,
    ) -> Result<Option<Network>> {
        sqlx::query_as::<_, Network>(
            "UPDATE networks
                SET physical_network = $2, vlan = $3, segment_key = $4,
                    legacy_segment = false, updated_at = $5
              WHERE id = $1 AND segment_key IS NULL
              RETURNING *",
        )
        .bind(id)
        .bind(physical_network)
        .bind(vlan)
        .bind(segment_key)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn update_network(
        &self,
        id: Uuid,
        name: &str,
        vlan: Option<i32>,
        vni: Option<i32>,
        ipam: &NetworkIpam,
    ) -> Result<Option<Network>> {
        sqlx::query_as::<_, Network>(
            "UPDATE networks SET name=$2, vlan=$3, vni=$4, cidr_v4=$5, gateway_v4=$6,
                cidr_v6=$7, gateway_v6=$8, dns=$9, pool_v4_start=$10, pool_v4_end=$11,
                pool_v6_start=$12, pool_v6_end=$13, updated_at=$14
             WHERE id=$1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(vlan)
        .bind(vni)
        .bind(&ipam.cidr_v4)
        .bind(&ipam.gateway_v4)
        .bind(&ipam.cidr_v6)
        .bind(&ipam.gateway_v6)
        .bind(&ipam.dns)
        .bind(&ipam.pool_v4_start)
        .bind(&ipam.pool_v4_end)
        .bind(&ipam.pool_v6_start)
        .bind(&ipam.pool_v6_end)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    // ---- IP allocations (design M13a) ------------------------------------

    /// All allocations in a network (used to compute the taken-address set).
    pub async fn allocations_for_network(&self, network_id: Uuid) -> Result<Vec<IpAllocation>> {
        sqlx::query_as::<_, IpAllocation>(
            "SELECT * FROM ip_allocations WHERE network_id=$1 ORDER BY ip",
        )
        .bind(network_id)
        .fetch_all(&self.pool)
        .await
    }

    /// All addresses assigned to a VM (for rendering network-config + release).
    pub async fn allocations_for_vm(&self, vm_id: Uuid) -> Result<Vec<IpAllocation>> {
        sqlx::query_as::<_, IpAllocation>(
            "SELECT * FROM ip_allocations WHERE vm_id=$1 ORDER BY nic_index, family",
        )
        .bind(vm_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Persist one address assignment. The unique (network_id, ip) constraint
    /// makes a concurrent double-allocation fail rather than silently collide.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_allocation(
        &self,
        network_id: Uuid,
        ip: &str,
        family: i16,
        vm_id: Option<Uuid>,
        nic_index: i32,
        mac: &str,
    ) -> Result<IpAllocation> {
        sqlx::query_as::<_, IpAllocation>(
            "INSERT INTO ip_allocations
                (id, network_id, ip, family, vm_id, nic_index, mac, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(network_id)
        .bind(ip)
        .bind(family)
        .bind(vm_id)
        .bind(nic_index)
        .bind(mac)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await
    }

    /// Free every address held by a VM (called on VM deletion).
    pub async fn release_vm_allocations(&self, vm_id: Uuid) -> Result<u64> {
        let res = sqlx::query("DELETE FROM ip_allocations WHERE vm_id=$1")
            .bind(vm_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    /// Free the addresses held by a single NIC (on a NIC network change — M13d).
    pub async fn release_nic_allocations(&self, vm_id: Uuid, nic_index: i32) -> Result<u64> {
        let res = sqlx::query("DELETE FROM ip_allocations WHERE vm_id=$1 AND nic_index=$2")
            .bind(vm_id)
            .bind(nic_index)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected())
    }

    // ---- security groups (design M13c) -----------------------------------

    pub async fn list_security_groups(&self) -> Result<Vec<SecurityGroup>> {
        sqlx::query_as::<_, SecurityGroup>("SELECT * FROM security_groups ORDER BY name")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_security_group(&self, id: Uuid) -> Result<Option<SecurityGroup>> {
        sqlx::query_as::<_, SecurityGroup>("SELECT * FROM security_groups WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn create_security_group(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<SecurityGroup> {
        let now = Utc::now();
        sqlx::query_as::<_, SecurityGroup>(
            "INSERT INTO security_groups (id, name, description, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$4) RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(description)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn update_security_group(
        &self,
        id: Uuid,
        name: &str,
        description: Option<&str>,
    ) -> Result<Option<SecurityGroup>> {
        sqlx::query_as::<_, SecurityGroup>(
            "UPDATE security_groups SET name=$2, description=$3, updated_at=$4
             WHERE id=$1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    /// Whether this group is a network's managed default (ADR-017).
    pub async fn security_group_is_managed(&self, id: Uuid) -> Result<bool> {
        Ok(
            sqlx::query_scalar::<_, bool>("SELECT managed FROM security_groups WHERE id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .unwrap_or(false),
        )
    }

    pub async fn delete_security_group(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM security_groups WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_sg_rules(&self, sg_id: Uuid) -> Result<Vec<SecurityGroupRule>> {
        sqlx::query_as::<_, SecurityGroupRule>(
            "SELECT * FROM security_group_rules WHERE security_group_id=$1 ORDER BY created_at",
        )
        .bind(sg_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Rules for a set of security groups (the union applied to a NIC).
    pub async fn rules_for_groups(&self, sg_ids: &[Uuid]) -> Result<Vec<SecurityGroupRule>> {
        sqlx::query_as::<_, SecurityGroupRule>(
            "SELECT * FROM security_group_rules WHERE security_group_id = ANY($1)",
        )
        .bind(sg_ids)
        .fetch_all(&self.pool)
        .await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_sg_rule(
        &self,
        sg_id: Uuid,
        direction: &str,
        ethertype: &str,
        protocol: &str,
        port_min: Option<i32>,
        port_max: Option<i32>,
        remote_cidr: Option<&str>,
    ) -> Result<SecurityGroupRule> {
        sqlx::query_as::<_, SecurityGroupRule>(
            "INSERT INTO security_group_rules
                (id, security_group_id, direction, ethertype, protocol, port_min, port_max,
                 remote_cidr, created_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(sg_id)
        .bind(direction)
        .bind(ethertype)
        .bind(protocol)
        .bind(port_min)
        .bind(port_max)
        .bind(remote_cidr)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await
    }

    /// Bump the generation of every VM whose NICs reference `sg_id`, so the
    /// reconcile loop re-applies the (changed) firewall to running VMs (M13c).
    /// Returns how many VMs were touched.
    pub async fn touch_vms_using_security_group(&self, sg_id: Uuid) -> Result<u64> {
        let vms =
            sqlx::query_as::<_, Vm>("SELECT * FROM virtual_machines WHERE phase <> 'Deleting'")
                .fetch_all(&self.pool)
                .await?;
        let ids: Vec<Uuid> = vms
            .into_iter()
            .filter(|vm| {
                vm.spec
                    .0
                    .network_interfaces
                    .iter()
                    .any(|n| n.security_groups.contains(&sg_id))
            })
            .map(|vm| vm.id)
            .collect();
        if ids.is_empty() {
            return Ok(0);
        }
        let res = sqlx::query(
            "UPDATE virtual_machines SET generation=generation+1, updated_at=$2 WHERE id = ANY($1)",
        )
        .bind(&ids)
        .bind(Utc::now())
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    pub async fn delete_sg_rule(&self, sg_id: Uuid, rule_id: Uuid) -> Result<bool> {
        let res =
            sqlx::query("DELETE FROM security_group_rules WHERE id=$1 AND security_group_id=$2")
                .bind(rule_id)
                .bind(sg_id)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    // ---- volumes (design M14a) -------------------------------------------

    pub async fn get_volume(&self, id: Uuid) -> Result<Option<Volume>> {
        sqlx::query_as::<_, Volume>("SELECT * FROM volumes WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn create_volume(
        &self,
        id: Uuid,
        name: &str,
        size_bytes: i64,
        format: &str,
        source_image_id: Option<Uuid>,
    ) -> Result<Volume> {
        let now = Utc::now();
        sqlx::query_as::<_, Volume>(
            "INSERT INTO volumes (id, name, size_bytes, format, source_image_id, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$6) RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(size_bytes)
        .bind(format)
        .bind(source_image_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn delete_volume(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM volumes WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // ---- volume snapshots (design M14c) ----------------------------------

    pub async fn list_volume_snapshots(&self, volume_id: Uuid) -> Result<Vec<VolumeSnapshot>> {
        sqlx::query_as::<_, VolumeSnapshot>(
            "SELECT * FROM volume_snapshots WHERE volume_id=$1 ORDER BY created_at",
        )
        .bind(volume_id)
        .fetch_all(&self.pool)
        .await
    }

    pub async fn get_volume_snapshot(&self, id: Uuid) -> Result<Option<VolumeSnapshot>> {
        sqlx::query_as::<_, VolumeSnapshot>("SELECT * FROM volume_snapshots WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn create_volume_snapshot(
        &self,
        id: Uuid,
        volume_id: Uuid,
        name: &str,
    ) -> Result<VolumeSnapshot> {
        sqlx::query_as::<_, VolumeSnapshot>(
            "INSERT INTO volume_snapshots (id, volume_id, name, created_at)
             VALUES ($1,$2,$3,$4) RETURNING *",
        )
        .bind(id)
        .bind(volume_id)
        .bind(name)
        .bind(Utc::now())
        .fetch_one(&self.pool)
        .await
    }

    pub async fn delete_volume_snapshot(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM volume_snapshots WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Record a volume as attached to a VM at a disk serial (design M14a).
    pub async fn set_volume_attachment(
        &self,
        id: Uuid,
        vm_id: Option<Uuid>,
        serial: Option<i32>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE volumes SET attached_vm_id=$2, attached_serial=$3, updated_at=$4 WHERE id=$1",
        )
        .bind(id)
        .bind(vm_id)
        .bind(serial)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    // ---- images (design M9) ---------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_image(
        &self,
        name: &str,
        source_path: &str,
        format: &str,
        boot: &BootSpec,
        default_size_bytes: Option<i64>,
        cloud_init: bool,
        os: Option<&str>,
    ) -> Result<Image> {
        let now = Utc::now();
        sqlx::query_as::<_, Image>(
            "INSERT INTO images
                (id, name, source_path, format, boot, default_size_bytes, cloud_init, os, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $9)
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(source_path)
        .bind(format)
        .bind(Json(boot))
        .bind(default_size_bytes)
        .bind(cloud_init)
        .bind(os)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    /// Create an image record in `importing` state for an async URL import
    /// (design M14b); the platform owns the backing file at `source_path`.
    #[allow(clippy::too_many_arguments)]
    pub async fn insert_image_importing(
        &self,
        id: Uuid,
        name: &str,
        source_path: &str,
        format: &str,
        boot: &BootSpec,
        default_size_bytes: Option<i64>,
        cloud_init: bool,
        os: Option<&str>,
    ) -> Result<Image> {
        let now = Utc::now();
        sqlx::query_as::<_, Image>(
            "INSERT INTO images
                (id, name, source_path, format, boot, default_size_bytes, cloud_init, os,
                 status, managed, created_at, updated_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'importing',TRUE,$9,$9)
             RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(source_path)
        .bind(format)
        .bind(Json(boot))
        .bind(default_size_bytes)
        .bind(cloud_init)
        .bind(os)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    /// Update an image's lifecycle status after an import finishes (M14b).
    pub async fn set_image_status(
        &self,
        id: Uuid,
        status: &str,
        size_bytes: Option<i64>,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE images SET status=$2, size_bytes=$3, error=$4, updated_at=$5 WHERE id=$1",
        )
        .bind(id)
        .bind(status)
        .bind(size_bytes)
        .bind(error)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn get_image(&self, id: Uuid) -> Result<Option<Image>> {
        sqlx::query_as::<_, Image>("SELECT * FROM images WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_images(&self) -> Result<Vec<Image>> {
        sqlx::query_as::<_, Image>("SELECT * FROM images ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn delete_image(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM images WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_image(
        &self,
        id: Uuid,
        name: &str,
        source_path: &str,
        format: &str,
        boot: &BootSpec,
        default_size_bytes: Option<i64>,
        cloud_init: bool,
        os: Option<&str>,
    ) -> Result<Option<Image>> {
        sqlx::query_as::<_, Image>(
            "UPDATE images SET name=$2, source_path=$3, format=$4, boot=$5,
                default_size_bytes=$6, cloud_init=$7, os=$8, updated_at=$9
             WHERE id=$1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(source_path)
        .bind(format)
        .bind(Json(boot))
        .bind(default_size_bytes)
        .bind(cloud_init)
        .bind(os)
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
    }

    // ---- templates (design M9) ------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn insert_template(
        &self,
        name: &str,
        image_id: Uuid,
        boot_vcpus: i32,
        max_vcpus: i32,
        memory_mib: i64,
        disk_size_bytes: Option<i64>,
        disk_format: &str,
        network_id: Option<Uuid>,
        cloud_init: Option<&CloudInitSpec>,
        machine_type: &str,
    ) -> Result<Template> {
        let now = Utc::now();
        let sealed_ci = self.seal_ci(cloud_init)?;
        let t = sqlx::query_as::<_, Template>(
            "INSERT INTO templates
                (id, name, image_id, boot_vcpus, max_vcpus, memory_mib, disk_size_bytes,
                 disk_format, network_id, cloud_init, machine_type, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $12, $11, $11)
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(image_id)
        .bind(boot_vcpus)
        .bind(max_vcpus)
        .bind(memory_mib)
        .bind(disk_size_bytes)
        .bind(disk_format)
        .bind(network_id)
        .bind(sealed_ci.as_ref().map(Json))
        .bind(now)
        .bind(machine_type)
        .fetch_one(&self.pool)
        .await?;
        self.open_template_opt(Some(t)).map(|o| o.unwrap())
    }

    pub async fn get_template(&self, id: Uuid) -> Result<Option<Template>> {
        let t = sqlx::query_as::<_, Template>("SELECT * FROM templates WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        self.open_template_opt(t)
    }

    pub async fn list_templates(&self) -> Result<Vec<Template>> {
        let mut ts = sqlx::query_as::<_, Template>("SELECT * FROM templates ORDER BY created_at")
            .fetch_all(&self.pool)
            .await?;
        for t in ts.iter_mut() {
            self.open_template(t)?;
        }
        Ok(ts)
    }

    pub async fn delete_template(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM templates WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    // ---- IAM: users, roles, permissions (design M12b) --------------------

    /// Create or refresh a user from a validated token identity (JIT), stamping
    /// last_login.
    pub async fn upsert_user(
        &self,
        subject: &str,
        username: &str,
        email: Option<&str>,
        display_name: Option<&str>,
    ) -> Result<User> {
        let now = Utc::now();
        sqlx::query_as::<_, User>(
            "INSERT INTO users (id, subject, username, email, display_name, last_login, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6, $6)
             ON CONFLICT (subject) DO UPDATE
               SET username=$3, email=$4, display_name=$5, last_login=$6, updated_at=$6
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(subject)
        .bind(username)
        .bind(email)
        .bind(display_name)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn list_users(&self) -> Result<Vec<User>> {
        sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY username")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_user(&self, id: Uuid) -> Result<Option<User>> {
        sqlx::query_as::<_, User>("SELECT * FROM users WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    /// Roles directly assigned to a user (not counting group-derived roles).
    pub async fn roles_for_user(&self, user_id: Uuid) -> Result<Vec<Role>> {
        sqlx::query_as::<_, Role>(
            "SELECT r.* FROM roles r
             JOIN user_roles ur ON ur.role_id = r.id
             WHERE ur.user_id = $1 ORDER BY r.name",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
    }

    /// Replace a user's direct role assignments.
    pub async fn set_user_roles(&self, user_id: Uuid, role_ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM user_roles WHERE user_id=$1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;
        for rid in role_ids {
            sqlx::query(
                "INSERT INTO user_roles (user_id, role_id) VALUES ($1,$2) ON CONFLICT DO NOTHING",
            )
            .bind(user_id)
            .bind(rid)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }

    /// Grant a role by name (used for the first-admin bootstrap).
    pub async fn grant_role_by_name(&self, user_id: Uuid, role: &str) -> Result<()> {
        sqlx::query(
            "INSERT INTO user_roles (user_id, role_id)
             SELECT $1, id FROM roles WHERE name=$2
             ON CONFLICT DO NOTHING",
        )
        .bind(user_id)
        .bind(role)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// The effective permission set for a user: the union over their directly
    /// assigned roles and any roles mapped from the token's `groups`.
    pub async fn effective_permissions(
        &self,
        user_id: Uuid,
        groups: &[String],
    ) -> Result<std::collections::HashSet<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            r#"SELECT DISTINCT rp.permission FROM role_permissions rp
               WHERE rp.role_id IN (
                   SELECT role_id FROM user_roles WHERE user_id = $1
                   UNION
                   SELECT role_id FROM group_roles WHERE "group" = ANY($2)
               )"#,
        )
        .bind(user_id)
        .bind(groups)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(p,)| p).collect())
    }

    pub async fn list_roles(&self) -> Result<Vec<Role>> {
        sqlx::query_as::<_, Role>("SELECT * FROM roles ORDER BY name")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn get_role(&self, id: Uuid) -> Result<Option<Role>> {
        sqlx::query_as::<_, Role>("SELECT * FROM roles WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn role_permissions(&self, role_id: Uuid) -> Result<Vec<String>> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT permission FROM role_permissions WHERE role_id=$1 ORDER BY permission",
        )
        .bind(role_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|(p,)| p).collect())
    }

    /// Create a custom role with a permission set.
    pub async fn create_role(
        &self,
        name: &str,
        description: Option<&str>,
        permissions: &[String],
    ) -> Result<Role> {
        let now = Utc::now();
        let id = Uuid::new_v4();
        let mut tx = self.pool.begin().await?;
        let role = sqlx::query_as::<_, Role>(
            "INSERT INTO roles (id, name, description, builtin, created_at, updated_at)
             VALUES ($1,$2,$3,false,$4,$4) RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        for p in permissions {
            sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES ($1,$2)")
                .bind(id)
                .bind(p)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(role)
    }

    /// Update a custom role's description + permissions (name kept).
    pub async fn update_role_permissions(
        &self,
        id: Uuid,
        description: Option<&str>,
        permissions: &[String],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE roles SET description=$2, updated_at=$3 WHERE id=$1")
            .bind(id)
            .bind(description)
            .bind(Utc::now())
            .execute(&mut *tx)
            .await?;
        sqlx::query("DELETE FROM role_permissions WHERE role_id=$1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        for p in permissions {
            sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES ($1,$2)")
                .bind(id)
                .bind(p)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await
    }

    /// Delete a non-builtin role. Returns false if it was builtin or absent.
    pub async fn delete_role(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query("DELETE FROM roles WHERE id=$1 AND builtin=false")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// group -> role name mappings (for display/management).
    pub async fn list_group_roles(&self) -> Result<Vec<(String, String)>> {
        sqlx::query_as(
            r#"SELECT gr."group", r.name FROM group_roles gr
               JOIN roles r ON r.id = gr.role_id ORDER BY gr."group""#,
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn add_group_role(&self, group: &str, role_id: Uuid) -> Result<()> {
        sqlx::query(
            r#"INSERT INTO group_roles ("group", role_id) VALUES ($1,$2) ON CONFLICT DO NOTHING"#,
        )
        .bind(group)
        .bind(role_id)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn remove_group_role(&self, group: &str, role_id: Uuid) -> Result<bool> {
        let res = sqlx::query(r#"DELETE FROM group_roles WHERE "group"=$1 AND role_id=$2"#)
            .bind(group)
            .bind(role_id)
            .execute(&self.pool)
            .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Re-sync built-in roles from code: ensure each exists (builtin=true) and
    /// its permission set matches the catalog. Runs at startup.
    pub async fn sync_builtin_roles(&self, roles: &[crate::rbac::BuiltinRole]) -> Result<()> {
        let now = Utc::now();
        for r in roles {
            let mut tx = self.pool.begin().await?;
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO roles (id, name, description, builtin, created_at, updated_at)
                 VALUES ($1,$2,$3,true,$4,$4)
                 ON CONFLICT (name) DO UPDATE SET description=$3, builtin=true, updated_at=$4
                 RETURNING id",
            )
            .bind(Uuid::new_v4())
            .bind(r.name)
            .bind(r.description)
            .bind(now)
            .fetch_one(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM role_permissions WHERE role_id=$1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            for p in &r.permissions {
                sqlx::query("INSERT INTO role_permissions (role_id, permission) VALUES ($1,$2)")
                    .bind(id)
                    .bind(*p)
                    .execute(&mut *tx)
                    .await?;
            }
            tx.commit().await?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_template(
        &self,
        id: Uuid,
        name: &str,
        image_id: Uuid,
        boot_vcpus: i32,
        max_vcpus: i32,
        memory_mib: i64,
        disk_size_bytes: Option<i64>,
        disk_format: &str,
        network_id: Option<Uuid>,
        cloud_init: Option<&CloudInitSpec>,
        machine_type: &str,
    ) -> Result<Option<Template>> {
        let sealed_ci = self.seal_ci(cloud_init)?;
        let t = sqlx::query_as::<_, Template>(
            "UPDATE templates SET name=$2, image_id=$3, boot_vcpus=$4, max_vcpus=$5,
                memory_mib=$6, disk_size_bytes=$7, disk_format=$8, network_id=$9,
                cloud_init=$10, updated_at=$11, machine_type=$12
             WHERE id=$1 RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(image_id)
        .bind(boot_vcpus)
        .bind(max_vcpus)
        .bind(memory_mib)
        .bind(disk_size_bytes)
        .bind(disk_format)
        .bind(network_id)
        .bind(sealed_ci.as_ref().map(Json))
        .bind(Utc::now())
        .bind(machine_type)
        .fetch_optional(&self.pool)
        .await?;
        self.open_template_opt(t)
    }

    // ---- tasks -----------------------------------------------------------

    pub async fn insert_task(&self, task_type: &str, vm_id: Option<Uuid>) -> Result<Task> {
        let now = Utc::now();
        let id = Uuid::new_v4();
        sqlx::query_as::<_, Task>(
            "INSERT INTO tasks (id, task_type, vm_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)
             RETURNING *",
        )
        .bind(id)
        .bind(task_type)
        .bind(vm_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn get_task(&self, id: Uuid) -> Result<Option<Task>> {
        sqlx::query_as::<_, Task>("SELECT * FROM tasks WHERE id=$1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_tasks(&self) -> Result<Vec<Task>> {
        sqlx::query_as::<_, Task>("SELECT * FROM tasks ORDER BY created_at DESC")
            .fetch_all(&self.pool)
            .await
    }

    pub async fn update_task(
        &self,
        id: Uuid,
        state: &str,
        progress: i32,
        message: Option<&str>,
    ) -> Result<()> {
        sqlx::query("UPDATE tasks SET state=$2, progress=$3, message=$4, updated_at=$5 WHERE id=$1")
            .bind(id)
            .bind(state)
            .bind(progress)
            .bind(message)
            .bind(Utc::now())
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    /// Find the newest in-flight task for a VM (to advance it during reconcile).
    pub async fn latest_open_task_for_vm(&self, vm_id: Uuid) -> Result<Option<Task>> {
        sqlx::query_as::<_, Task>(
            "SELECT * FROM tasks
             WHERE vm_id=$1 AND state IN ('Pending','Running')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(vm_id)
        .fetch_optional(&self.pool)
        .await
    }

    // ---- migrations ------------------------------------------------------

    pub async fn insert_migration(
        &self,
        vm_id: Uuid,
        source_host_id: Option<Uuid>,
        target_host_id: Uuid,
        task_id: Uuid,
    ) -> Result<Migration> {
        let now = Utc::now();
        sqlx::query_as::<_, Migration>(
            "INSERT INTO migrations (id, vm_id, source_host_id, target_host_id, task_id, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $6)
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(vm_id)
        .bind(source_host_id)
        .bind(target_host_id)
        .bind(task_id)
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    /// Migrations still in flight (not Completed or Failed).
    pub async fn list_active_migrations(&self) -> Result<Vec<Migration>> {
        sqlx::query_as::<_, Migration>(
            "SELECT * FROM migrations WHERE state NOT IN ('Completed', 'Failed') ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
    }

    pub async fn active_migration_for_vm(&self, vm_id: Uuid) -> Result<Option<Migration>> {
        sqlx::query_as::<_, Migration>(
            "SELECT * FROM migrations
             WHERE vm_id = $1 AND state NOT IN ('Completed', 'Failed')
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(vm_id)
        .fetch_optional(&self.pool)
        .await
    }

    pub async fn update_migration(
        &self,
        id: Uuid,
        state: &str,
        migration_url: Option<&str>,
        message: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE migrations
             SET state=$2,
                 migration_url=COALESCE($3, migration_url),
                 message=$4,
                 updated_at=$5
             WHERE id=$1",
        )
        .bind(id)
        .bind(state)
        .bind(migration_url)
        .bind(message)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    /// Move a VM to a new host (after a successful migration).
    pub async fn set_vm_host_running(&self, id: Uuid, host_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE virtual_machines SET host_id=$2, phase='Running', updated_at=$3 WHERE id=$1",
        )
        .bind(id)
        .bind(host_id)
        .bind(Utc::now())
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    // ---- events ----------------------------------------------------------

    pub async fn insert_event(
        &self,
        resource_type: &str,
        resource_id: Option<Uuid>,
        event_type: &str,
        severity: &str,
        message: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO events (id, ts, resource_type, resource_id, event_type, severity, message)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(Uuid::new_v4())
        .bind(Utc::now())
        .bind(resource_type)
        .bind(resource_id)
        .bind(event_type)
        .bind(severity)
        .bind(message)
        .execute(&self.pool)
        .await
        .map(|_| ())
    }

    pub async fn list_events(&self, limit: i64) -> Result<Vec<Event>> {
        sqlx::query_as::<_, Event>("SELECT * FROM events ORDER BY ts DESC LIMIT $1")
            .bind(limit)
            .fetch_all(&self.pool)
            .await
    }
}
