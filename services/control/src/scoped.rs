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

use crate::store::{Image, Network, SecurityGroup, Store, Template, Vm, Volume};

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

    // ---- catalogues, read through the same predicate ----------------------

    pub async fn list_templates(&self) -> Result<Vec<Template>> {
        sqlx::query_as::<_, Template>(
            "SELECT * FROM templates
              WHERE ($1::uuid IS NULL OR project_id = $1)
              ORDER BY name",
        )
        .bind(self.filter())
        .fetch_all(self.store.pool())
        .await
    }

    pub async fn get_template(&self, id: Uuid) -> Result<Option<Template>> {
        sqlx::query_as::<_, Template>(
            "SELECT * FROM templates
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_optional(self.store.pool())
        .await
    }

    pub async fn list_security_groups(&self) -> Result<Vec<SecurityGroup>> {
        sqlx::query_as::<_, SecurityGroup>(
            "SELECT * FROM security_groups
              WHERE ($1::uuid IS NULL OR project_id = $1)
              ORDER BY name",
        )
        .bind(self.filter())
        .fetch_all(self.store.pool())
        .await
    }

    pub async fn get_security_group(&self, id: Uuid) -> Result<Option<SecurityGroup>> {
        sqlx::query_as::<_, SecurityGroup>(
            "SELECT * FROM security_groups
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_optional(self.store.pool())
        .await
    }

    // Shareable: NULL project means platform-shared and visible everywhere.

    pub async fn list_networks(&self) -> Result<Vec<Network>> {
        sqlx::query_as::<_, Network>(
            "SELECT * FROM networks
              WHERE (project_id IS NULL OR $1::uuid IS NULL OR project_id = $1)
              ORDER BY name",
        )
        .bind(self.filter())
        .fetch_all(self.store.pool())
        .await
    }

    pub async fn get_network(&self, id: Uuid) -> Result<Option<Network>> {
        sqlx::query_as::<_, Network>(
            "SELECT * FROM networks
              WHERE id = $1
                AND (project_id IS NULL OR $2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .fetch_optional(self.store.pool())
        .await
    }

    pub async fn list_images(&self) -> Result<Vec<Image>> {
        sqlx::query_as::<_, Image>(
            "SELECT * FROM images
              WHERE (project_id IS NULL OR $1::uuid IS NULL OR project_id = $1)
              ORDER BY name",
        )
        .bind(self.filter())
        .fetch_all(self.store.pool())
        .await
    }

    // ---- writes -----------------------------------------------------------
    //
    // The same predicate goes in the WHERE of an update or delete, so a
    // cross-project write matches zero rows and the handler renders it as *not
    // found* — the same answer an unknown id gets. There is no separate
    // authorization step that could be forgotten, and no existence oracle.

    pub async fn delete_template(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query(
            "DELETE FROM templates
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .execute(self.store.pool())
        .await?
        .rows_affected()
            > 0)
    }

    pub async fn delete_security_group(&self, id: Uuid) -> Result<bool> {
        Ok(sqlx::query(
            "DELETE FROM security_groups
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)",
        )
        .bind(id)
        .bind(self.filter())
        .execute(self.store.pool())
        .await?
        .rows_affected()
            > 0)
    }

    /// Whether a shareable-catalogue row may be *written* from this scope.
    ///
    /// Stricter than the matching `_visible` check on purpose: a platform-shared
    /// image or network (NULL) is readable from every project and editable from
    /// none of them. Sharing a resource must not hand every project the ability
    /// to delete it out from under the others.
    pub async fn image_writable(&self, id: Uuid) -> Result<bool> {
        self.owned_row("images", id).await
    }

    pub async fn network_writable(&self, id: Uuid) -> Result<bool> {
        self.owned_row("networks", id).await
    }

    async fn owned_row(&self, table: &'static str, id: Uuid) -> Result<bool> {
        // `table` is a literal chosen here, never caller input.
        Ok(sqlx::query_scalar::<_, i64>(&format!(
            "SELECT count(*) FROM {table}
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)"
        ))
        .bind(id)
        .bind(self.filter())
        .fetch_one(self.store.pool())
        .await?
            > 0)
    }

    pub async fn delete_image(&self, id: Uuid) -> Result<bool> {
        self.delete_owned("images", id).await
    }

    pub async fn delete_network(&self, id: Uuid) -> Result<bool> {
        self.delete_owned("networks", id).await
    }

    pub async fn delete_volume(&self, id: Uuid) -> Result<bool> {
        self.delete_owned("volumes", id).await
    }

    async fn delete_owned(&self, table: &'static str, id: Uuid) -> Result<bool> {
        Ok(sqlx::query(&format!(
            "DELETE FROM {table}
              WHERE id = $1 AND ($2::uuid IS NULL OR project_id = $2)"
        ))
        .bind(id)
        .bind(self.filter())
        .execute(self.store.pool())
        .await?
        .rows_affected()
            > 0)
    }

    /// The project a resource created in this scope belongs to.
    ///
    /// Platform scope means the default project: a resource has to belong to
    /// exactly one, and "everything" is not a home. That is also what keeps
    /// behaviour identical while tenancy is off.
    pub fn owning_project(&self) -> Uuid {
        self.filter().unwrap_or(vquasar_model::DEFAULT_PROJECT_ID)
    }

    /// The owner to stamp on a newly created row in a *shareable* catalogue
    /// (images, networks), where NULL means platform-shared.
    ///
    /// Deliberately not `owning_project()`: with tenancy off there is no
    /// project context, and stamping the default project would quietly make
    /// every image and network created today invisible to every other project
    /// the day tenancy is switched on. NULL keeps them shared, which is exactly
    /// what the migration's backfill did to the rows that already existed.
    pub fn shareable_owner(&self) -> Option<Uuid> {
        self.filter()
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
