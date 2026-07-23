//! PostgreSQL persistence (design document, section 31).
//!
//! Explicit SQL via `sqlx` runtime queries — no ORM and no compile-time database
//! dependency. State transitions run inside the methods here; the `generation`
//! columns exist for optimistic concurrency as the controllers mature.

use ch_model::VirtualMachineSpec;
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

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

/// A virtual network row.
#[derive(Debug, Clone, serde::Serialize, FromRow)]
pub struct Network {
    pub id: Uuid,
    pub name: String,
    /// 802.1Q VLAN tag; `None` is a flat/untagged provider network.
    pub vlan: Option<i32>,
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
    pub hostname: Option<String>,
    pub architecture: Option<String>,
    pub kernel_version: Option<String>,
    pub cloud_hypervisor_version: Option<String>,
    pub logical_cpus: Option<i32>,
    pub cpu_model: Option<String>,
    pub total_memory_bytes: Option<i64>,
    pub available_memory_bytes: Option<i64>,
    pub vm_count: i32,
}

/// The persistence layer.
#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

type Result<T> = std::result::Result<T, sqlx::Error>;

impl Store {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
                available_memory_bytes=$9, vm_count=$10, last_heartbeat=$11, updated_at=$11
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
        .execute(&self.pool)
        .await
        .map(|_| ())
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

    pub async fn insert_vm(&self, name: &str, spec: &VirtualMachineSpec) -> Result<Vm> {
        let now = Utc::now();
        let id = Uuid::new_v4();
        sqlx::query_as::<_, Vm>(
            "INSERT INTO virtual_machines (id, name, spec, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)
             RETURNING *",
        )
        .bind(id)
        .bind(name)
        .bind(Json(spec))
        .bind(now)
        .fetch_one(&self.pool)
        .await
    }

    pub async fn get_vm(&self, id: Uuid) -> Result<Option<Vm>> {
        sqlx::query_as::<_, Vm>("SELECT * FROM virtual_machines WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn list_vms(&self) -> Result<Vec<Vm>> {
        sqlx::query_as::<_, Vm>("SELECT * FROM virtual_machines ORDER BY created_at")
            .fetch_all(&self.pool)
            .await
    }

    /// VMs whose observed state has not caught up with desired state and are
    /// therefore candidates for reconciliation (section 32).
    pub async fn list_vms_to_reconcile(&self) -> Result<Vec<Vm>> {
        sqlx::query_as::<_, Vm>(
            "SELECT * FROM virtual_machines
             WHERE phase NOT IN ('Running', 'Stopped', 'Failed')
                OR generation <> observed_generation
             ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await
    }

    /// Set the desired power state on an existing VM (bumps generation so the
    /// controller reconciles it).
    pub async fn set_vm_spec(&self, id: Uuid, spec: &VirtualMachineSpec) -> Result<Option<Vm>> {
        sqlx::query_as::<_, Vm>(
            "UPDATE virtual_machines
             SET spec=$2, generation=generation+1, updated_at=$3
             WHERE id=$1
             RETURNING *",
        )
        .bind(id)
        .bind(Json(spec))
        .bind(Utc::now())
        .fetch_optional(&self.pool)
        .await
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
    ) -> Result<()> {
        sqlx::query(
            "UPDATE virtual_machines
             SET phase=$2, observed_generation=$3, message=$4, updated_at=$5
             WHERE id=$1",
        )
        .bind(id)
        .bind(phase)
        .bind(observed_generation)
        .bind(message)
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
        sqlx::query("DELETE FROM virtual_machines WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map(|_| ())
    }

    // ---- networks --------------------------------------------------------

    pub async fn insert_network(&self, name: &str, vlan: Option<i32>) -> Result<Network> {
        let now = Utc::now();
        sqlx::query_as::<_, Network>(
            "INSERT INTO networks (id, name, vlan, created_at, updated_at)
             VALUES ($1, $2, $3, $4, $4)
             RETURNING *",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(vlan)
        .bind(now)
        .fetch_one(&self.pool)
        .await
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
