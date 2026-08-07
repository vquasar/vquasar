//! Per-project quotas: admission control on committed intent (ADR-019).
//!
//! A quota is a ceiling on what a project has *asked for*, not on what the
//! fleet has managed to build. A resource consumes quota from the moment its
//! row exists until the row is gone — including while `Pending`, `Failed` or
//! `Deleting`. That is deliberately unlike the scheduler's per-host commitment
//! model, which excludes `Deleting`: the scheduler asks "what will this host be
//! running", a quota asks "what has this project committed to".
//!
//! Enforcement happens **only at API admission**, inside the transaction that
//! persists the intent. The reconcile loop never rejects work for quota
//! reasons; if it did, a request could be accepted and then stranded, and the
//! loop would become a second authority on what is admissible.
//!
//! Usage is derived, never stored. [`admit`] locks the project row, aggregates
//! from the owning tables, compares and lets the caller insert — all in one
//! transaction. That serialises writes per project, and only per project.

use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

/// The dimensions a project is capped on.
///
/// Storage counts volumes *and* disks a VM spec asks the agent to provision.
/// Counting only volumes would leave the cap trivially bypassable by asking for
/// the space as a VM disk instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amounts {
    pub vms: i64,
    pub vcpus: i64,
    pub memory_mib: i64,
    pub volumes: i64,
    pub storage_bytes: i64,
}

/// Limits for one project. `None` is unlimited in that dimension — a project
/// can be capped on memory without having to declare a VM count.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_vms: Option<i64>,
    pub max_vcpus: Option<i64>,
    pub max_memory_mib: Option<i64>,
    pub max_volumes: Option<i64>,
    pub max_storage_bytes: Option<i64>,
}

/// The quota row as PostgreSQL hands it back. Counts are `INTEGER` and sizes
/// are `BIGINT`; [`Limits`] widens everything to `i64` so the comparison
/// arithmetic has one type.
pub(crate) type LimitRow = (
    Option<i32>,
    Option<i32>,
    Option<i64>,
    Option<i32>,
    Option<i64>,
);

pub(crate) const LIMIT_COLUMNS: &str =
    "max_vms, max_vcpus, max_memory_mib, max_volumes, max_storage_bytes";

impl From<LimitRow> for Limits {
    fn from((vms, vcpus, mem, vols, bytes): LimitRow) -> Self {
        Self {
            max_vms: vms.map(i64::from),
            max_vcpus: vcpus.map(i64::from),
            max_memory_mib: mem,
            max_volumes: vols.map(i64::from),
            max_storage_bytes: bytes,
        }
    }
}

impl Limits {
    fn dimensions(&self) -> [(&'static str, Option<i64>); 5] {
        [
            ("vms", self.max_vms),
            ("vcpus", self.max_vcpus),
            ("memory_mib", self.max_memory_mib),
            ("volumes", self.max_volumes),
            ("storage_bytes", self.max_storage_bytes),
        ]
    }
}

impl Amounts {
    fn dimensions(&self) -> [(&'static str, i64); 5] {
        [
            ("vms", self.vms),
            ("vcpus", self.vcpus),
            ("memory_mib", self.memory_mib),
            ("volumes", self.volumes),
            ("storage_bytes", self.storage_bytes),
        ]
    }

    /// A VM's contribution: one VM, its `max_vcpus` (the ceiling hot-plug can
    /// reach, which is what was committed to), its memory, and any disk the
    /// spec asks to have provisioned.
    pub fn of_vm(spec: &vquasar_model::VirtualMachineSpec) -> Self {
        Self {
            vms: 1,
            vcpus: i64::from(spec.cpu.max_vcpus),
            memory_mib: spec
                .memory
                .max_size_mib
                .unwrap_or(spec.memory.size_mib)
                .min(i64::MAX as u64) as i64,
            volumes: 0,
            storage_bytes: spec
                .disks
                .iter()
                .filter_map(|d| d.size_bytes)
                .map(|b| b.min(i64::MAX as u64) as i64)
                .sum(),
        }
    }

    pub fn of_volume(size_bytes: i64) -> Self {
        Self {
            volumes: 1,
            storage_bytes: size_bytes,
            ..Self::default()
        }
    }

    /// The change from one shape to another, for an in-place edit. Negative
    /// components are how shrinking a VM frees quota.
    pub fn delta(before: &Self, after: &Self) -> Self {
        Self {
            vms: after.vms - before.vms,
            vcpus: after.vcpus - before.vcpus,
            memory_mib: after.memory_mib - before.memory_mib,
            volumes: after.volumes - before.volumes,
            storage_bytes: after.storage_bytes - before.storage_bytes,
        }
    }
}

/// A refused admission, naming the dimension and the arithmetic behind it.
///
/// The numbers are the point: "over quota" without them sends an operator to
/// the database to work out which limit and by how much.
#[derive(Debug, thiserror::Error)]
pub struct Exceeded {
    pub dimension: &'static str,
    pub limit: i64,
    pub in_use: i64,
    pub requested: i64,
}

impl std::fmt::Display for Exceeded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "project quota exceeded: {} — limit {}, {} in use, this request asks for {}",
            self.dimension, self.limit, self.in_use, self.requested
        )
    }
}

/// Check `demand` against the project's quota, holding the project row.
///
/// Returns `Ok(())` when the project has no quota row: absence of a quota is
/// not a quota of zero, and every project that existed before quotas did has no
/// row (0023).
///
/// The `SELECT ... FOR UPDATE` on `projects` is what makes this correct under
/// concurrency: two simultaneous creates in the same project serialise, so they
/// cannot both read the same usage and both fit. Locking the project rather
/// than the resource tables keeps that serialisation per project — two projects
/// admitting at once do not contend.
pub async fn admit(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
    demand: Amounts,
) -> Result<(), AdmitError> {
    let Some(limits) = lock_and_load(tx, project).await? else {
        return Ok(());
    };
    // Nothing to check if every dimension is unlimited — skip the aggregation.
    if limits.dimensions().iter().all(|(_, l)| l.is_none()) {
        return Ok(());
    }
    let usage = usage_in(tx, project).await?;
    Ok(check(&limits, &usage, &demand)?)
}

/// Why an admission did not happen. Kept distinct from a database failure so
/// the API can render one as a `409` and the other as a `500`; collapsing them
/// would turn every quota refusal into an internal error.
#[derive(Debug, thiserror::Error)]
pub enum AdmitError {
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Exceeded(#[from] Exceeded),
}

/// Pure comparison, split out so it is testable without a database.
pub fn check(limits: &Limits, usage: &Amounts, demand: &Amounts) -> Result<(), Exceeded> {
    for ((dim, limit), ((_, used), (_, want))) in limits
        .dimensions()
        .into_iter()
        .zip(usage.dimensions().into_iter().zip(demand.dimensions()))
    {
        let Some(limit) = limit else { continue };
        // A request that frees resources is always admissible, even from over
        // quota — otherwise lowering a limit would trap a project above it with
        // no way down.
        if want <= 0 {
            continue;
        }
        if used + want > limit {
            return Err(Exceeded {
                dimension: dim,
                limit,
                in_use: used,
                requested: want,
            });
        }
    }
    Ok(())
}

async fn lock_and_load(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
) -> Result<Option<Limits>, sqlx::Error> {
    sqlx::query("SELECT id FROM projects WHERE id = $1 FOR UPDATE")
        .bind(project)
        .fetch_optional(&mut **tx)
        .await?;
    let row: Option<LimitRow> = sqlx::query_as(&format!(
        "SELECT {LIMIT_COLUMNS} FROM project_quotas WHERE project_id = $1"
    ))
    .bind(project)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(Limits::from))
}

/// Current usage, aggregated from the owning tables.
///
/// The VM figures come out of the spec JSON rather than denormalized columns.
/// That keeps the domain model the single description of a VM's shape — a
/// column would be a second one, and the two would disagree the first time a
/// spec was edited without the column being updated.
pub async fn usage_in(
    tx: &mut Transaction<'_, Postgres>,
    project: Uuid,
) -> Result<Amounts, sqlx::Error> {
    let (vms, vcpus, memory_mib, vm_disk_bytes): (i64, i64, i64, i64) = sqlx::query_as(
        r#"SELECT
             count(*),
             -- SUM over bigint returns numeric in PostgreSQL; cast back, or
             -- decoding fails at runtime with a type mismatch.
             COALESCE(SUM((spec->'cpu'->>'max_vcpus')::bigint), 0)::bigint,
             COALESCE(SUM(COALESCE((spec->'memory'->>'max_size_mib')::bigint,
                                   (spec->'memory'->>'size_mib')::bigint)), 0)::bigint,
             COALESCE(SUM((
                 SELECT COALESCE(SUM((d->>'size_bytes')::bigint), 0)
                   FROM jsonb_array_elements(COALESCE(spec->'disks', '[]'::jsonb)) d
                  WHERE d ? 'size_bytes'
             )), 0)::bigint
           FROM virtual_machines WHERE project_id = $1"#,
    )
    .bind(project)
    .fetch_one(&mut **tx)
    .await?;

    let (volumes, volume_bytes): (i64, i64) = sqlx::query_as(
        "SELECT count(*), COALESCE(SUM(size_bytes), 0)::bigint
           FROM volumes WHERE project_id = $1",
    )
    .bind(project)
    .fetch_one(&mut **tx)
    .await?;

    Ok(Amounts {
        vms,
        vcpus,
        memory_mib,
        volumes,
        storage_bytes: vm_disk_bytes + volume_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> Limits {
        Limits {
            max_vms: Some(2),
            max_vcpus: Some(8),
            max_memory_mib: Some(4096),
            max_volumes: None,
            max_storage_bytes: Some(1000),
        }
    }

    #[test]
    fn a_request_that_fits_is_admitted() {
        let usage = Amounts {
            vms: 1,
            vcpus: 4,
            memory_mib: 2048,
            ..Default::default()
        };
        let want = Amounts {
            vms: 1,
            vcpus: 4,
            memory_mib: 2048,
            ..Default::default()
        };
        assert!(check(&limits(), &usage, &want).is_ok());
    }

    /// Exactly at the limit fits; one more does not. Off-by-one here is the
    /// difference between a quota of N and a quota of N-1.
    #[test]
    fn the_boundary_is_inclusive() {
        let at = Amounts {
            vms: 2,
            ..Default::default()
        };
        let one_more = Amounts {
            vms: 1,
            ..Default::default()
        };
        assert!(check(&limits(), &at, &Amounts::default()).is_ok());
        let e = check(&limits(), &at, &one_more).unwrap_err();
        assert_eq!(e.dimension, "vms");
        assert_eq!((e.limit, e.in_use, e.requested), (2, 2, 1));
    }

    #[test]
    fn an_unlimited_dimension_never_refuses() {
        let usage = Amounts {
            volumes: 1_000_000,
            ..Default::default()
        };
        let want = Amounts {
            volumes: 1,
            ..Default::default()
        };
        assert!(check(&limits(), &usage, &want).is_ok());
    }

    /// Lowering a quota below current usage is allowed (ADR-019): it blocks new
    /// commitments. A project trapped above its limit must still be able to
    /// shrink, or the only way out would be a database edit.
    #[test]
    fn shrinking_is_admissible_from_over_quota() {
        let over = Amounts {
            vms: 5,
            vcpus: 40,
            memory_mib: 99_999,
            ..Default::default()
        };
        let freeing = Amounts {
            vms: -1,
            vcpus: -8,
            memory_mib: -2048,
            ..Default::default()
        };
        assert!(check(&limits(), &over, &freeing).is_ok());
    }

    /// A zero limit is a real limit — freeze the project — not "unset".
    #[test]
    fn zero_is_a_limit_not_an_absence() {
        let frozen = Limits {
            max_vms: Some(0),
            ..Default::default()
        };
        let want = Amounts {
            vms: 1,
            ..Default::default()
        };
        assert!(check(&frozen, &Amounts::default(), &want).is_err());
    }

    /// `max_vcpus` and `max_size_mib` are the committed ceiling: a VM that can
    /// hot-plug up to 16 vCPUs has committed 16, whatever it boots with.
    #[test]
    fn a_vm_commits_its_hot_plug_ceiling() {
        let spec: vquasar_model::VirtualMachineSpec = serde_json::from_value(serde_json::json!({
            "desired_power_state": "Running",
            "cpu": {"boot_vcpus": 2, "max_vcpus": 16},
            "memory": {"size_mib": 1024, "max_size_mib": 8192},
            "boot": {"type": "direct_kernel", "kernel": "/x/vmlinuz"},
            "disks": [{"path": "/x/a.qcow2", "size_bytes": 500},
                      {"path": "/x/b.qcow2"}],
            "network_interfaces": [], "placement": {}
        }))
        .unwrap();
        let a = Amounts::of_vm(&spec);
        assert_eq!(a.vcpus, 16);
        assert_eq!(a.memory_mib, 8192);
        // A disk with no size is one that already exists; it is not this
        // request asking for space.
        assert_eq!(a.storage_bytes, 500);
        assert_eq!(a.volumes, 0);
    }

    #[test]
    fn a_delta_is_the_difference_between_two_shapes() {
        let before = Amounts {
            vcpus: 8,
            memory_mib: 4096,
            ..Default::default()
        };
        let after = Amounts {
            vcpus: 4,
            memory_mib: 8192,
            ..Default::default()
        };
        let d = Amounts::delta(&before, &after);
        assert_eq!(d.vcpus, -4);
        assert_eq!(d.memory_mib, 4096);
    }
}
