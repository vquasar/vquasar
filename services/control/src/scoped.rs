//! Tenant-scoped persistence (design §47, ADR-018).
//!
//! # Why a separate handle rather than a predicate per call site
//!
//! Scoping that each query has to remember is scoping that will eventually be
//! forgotten, and the failure is silent: one missing predicate is one endpoint
//! quietly returning another project's rows. Moving the tenant-scoped queries
//! onto a handle that *cannot be built without a caller* turns that from a
//! review problem into a compile error — the method simply is not on [`Store`].
//!
//! # The predicate
//!
//! Every scoped query uses the same shape:
//!
//! ```sql
//! AND ($n::uuid IS NULL OR project_id = $n)          -- owned
//! AND (project_id IS NULL OR $n::uuid IS NULL OR project_id = $n)  -- shareable
//! ```
//!
//! One bind, no dynamic SQL, and platform scope is a bound NULL rather than a
//! different statement — so there is one query and one plan per method, and the
//! literal SQL stays greppable (§31).
//!
//! Crucially the same predicate goes in the `WHERE` of updates and deletes, so
//! a cross-project write affects zero rows and surfaces as *not found*. There
//! is no separate authorization step to forget, and no existence oracle.

use uuid::Uuid;
use vquasar_model::Scope;

use crate::store::{Store, Vm, Volume};

type Result<T> = std::result::Result<T, sqlx::Error>;

/// A [`Store`] bound to one caller's view of the world.
#[derive(Clone)]
pub struct ScopedStore {
    store: Store,
    scope: Scope,
}

impl ScopedStore {
    /// Bind a store to a scope.
    ///
    /// Deliberately not public beyond the crate: a `ScopedStore` should come
    /// from an authenticated request or from the reconcile loop, never be
    /// conjured mid-handler.
    pub(crate) fn new(store: Store, scope: Scope) -> Self {
        Self { store, scope }
    }

    fn filter(&self) -> Option<Uuid> {
        self.scope.project_filter()
    }

    // ---- virtual machines -------------------------------------------------

    pub async fn list_vms(&self) -> Result<Vec<Vm>> {
        let vms = sqlx::query_as::<_, Vm>(
            "SELECT * FROM virtual_machines
              WHERE ($1::uuid IS NULL OR project_id = $1)
              ORDER BY created_at",
        )
        .bind(self.filter())
        .fetch_all(self.store.pool())
        .await?;
        self.store.open_vms_public(vms)
    }

    pub async fn get_vm(&self, id: Uuid) -> Result<Option<Vm>> {
        let vm = sqlx::query_as::<_, Vm>(
            "SELECT * FROM virtual_machines
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_optional(self.store.pool())
        .await?;
        self.store.open_vm_opt_public(vm)
    }

    // ---- volumes ----------------------------------------------------------

    pub async fn list_volumes(&self) -> Result<Vec<Volume>> {
        sqlx::query_as::<_, Volume>(
            "SELECT * FROM volumes
              WHERE ($1::uuid IS NULL OR project_id = $1)
              ORDER BY created_at",
        )
        .bind(self.filter())
        .fetch_all(self.store.pool())
        .await
    }

    pub async fn get_volume(&self, id: Uuid) -> Result<Option<Volume>> {
        sqlx::query_as::<_, Volume>(
            "SELECT * FROM volumes
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_optional(self.store.pool())
        .await
    }

    // ---- shareable catalogues --------------------------------------------
    //
    // A NULL project_id means platform-shared: visible and usable from every
    // project. That is what keeps a fleet's curated images and its provider
    // networks working once a second project exists.

    /// Whether a network is visible in this scope (shared, or ours).
    pub async fn network_visible(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM networks
              WHERE id = $1
                AND (project_id IS NULL OR $2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_one(self.store.pool())
        .await?
            > 0)
    }

    /// Whether an image is visible in this scope.
    pub async fn image_visible(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM images
              WHERE id = $1
                AND (project_id IS NULL OR $2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_one(self.store.pool())
        .await?
            > 0)
    }

    /// Whether every one of these security groups is in this scope.
    ///
    /// Security groups are project-owned, not shareable: a NIC referencing one
    /// from another project would apply that project's policy to this VM.
    pub async fn security_groups_in_scope(&self, ids: &[Uuid]) -> Result<bool> {
        if ids.is_empty() {
            return Ok(true);
        }
        let found: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM security_groups
              WHERE id = ANY($1) AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(ids)
        .bind(self.filter())
        .fetch_one(self.store.pool())
        .await?;
        Ok(found as usize == ids.len())
    }

    /// Whether a template is in this scope.
    pub async fn template_in_scope(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM templates
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_one(self.store.pool())
        .await?
            > 0)
    }
}
