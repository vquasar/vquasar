//! The public REST API (design document, section 14). Versioned under
//! `/api/v1`. Handlers are thin: they validate input, call the [`Store`], and
//! return typed errors — no infrastructure logic here (ADR-015).

pub mod enroll;
pub mod error;
mod events;
mod hosts;
mod iam;
mod images;
mod networks;
pub mod pathsafe;
pub mod redact;
mod security_groups;
mod tasks;
mod templates;
mod vms;
mod volumes;

use axum::routing::{get, post};
use axum::{Extension, Router};

use crate::authz::AuthState;
use crate::store::Store;

pub use enroll::EnrollmentState;

/// Build the `/api/v1` router bound to `store`, with auth wiring attached.
/// When `enrollment` is set (the issuing CA is configured), the token-based
/// agent auto-enrollment endpoints are mounted (design M16).
pub fn router(store: Store, auth: AuthState, enrollment: Option<EnrollmentState>) -> Router {
    let mut v1 = Router::new()
        .route("/auth-config", get(iam::auth_config))
        .route("/me", get(iam::me))
        .route("/permissions", get(iam::permissions))
        .route("/users", get(iam::list_users))
        .route("/users/:id/roles", axum::routing::put(iam::set_user_roles))
        .route("/roles", get(iam::list_roles).post(iam::create_role))
        .route(
            "/roles/:id",
            get(iam::get_role)
                .patch(iam::update_role)
                .delete(iam::delete_role),
        )
        .route(
            "/group-mappings",
            get(iam::list_group_roles).post(iam::add_group_role),
        )
        .route(
            "/group-mappings/:group/:role_id",
            axum::routing::delete(iam::remove_group_role),
        )
        .route("/hosts", get(hosts::list).post(hosts::register))
        .route("/hosts/:id", get(hosts::get).patch(hosts::update))
        .route("/hosts/:id/drain", post(hosts::drain))
        .route("/vms", get(vms::list).post(vms::create))
        .route("/vms/from-template", post(vms::create_from_template))
        .route("/vms/from-volume", post(vms::create_from_volume))
        .route(
            "/vms/:id",
            get(vms::get).patch(vms::update).delete(vms::delete),
        )
        .route("/vms/:id/start", post(vms::start))
        .route("/vms/:id/stop", post(vms::stop))
        .route("/vms/:id/migrate", post(vms::migrate))
        .route("/vms/:id/nics/:index", axum::routing::put(vms::change_nic))
        // Public: cloud-init phone_home IP-discovery fallback (design M13e).
        .route("/phone-home/:vm_id", post(vms::phone_home))
        .route("/vms/:id/console", get(crate::console::console_ws))
        .route("/vms/:id/metrics", get(vms::metrics))
        .route("/networks", get(networks::list).post(networks::create))
        .route(
            "/networks/:id",
            get(networks::get)
                .patch(networks::update)
                .delete(networks::delete),
        )
        .route("/networks/:id/allocations", get(networks::allocations))
        .route(
            "/security-groups",
            get(security_groups::list).post(security_groups::create),
        )
        .route(
            "/security-groups/:id",
            get(security_groups::get)
                .patch(security_groups::update)
                .delete(security_groups::delete),
        )
        .route(
            "/security-groups/:id/rules",
            post(security_groups::add_rule),
        )
        .route(
            "/security-groups/:id/rules/:rule_id",
            axum::routing::delete(security_groups::delete_rule),
        )
        .route("/volumes", get(volumes::list).post(volumes::create))
        .route("/volumes/:id", get(volumes::get).delete(volumes::delete))
        .route("/volumes/:id/attach", post(volumes::attach))
        .route("/volumes/:id/detach", post(volumes::detach))
        .route(
            "/volumes/:id/snapshots",
            get(volumes::list_snapshots).post(volumes::create_snapshot),
        )
        .route(
            "/volumes/:id/snapshots/:snap_id",
            axum::routing::delete(volumes::delete_snapshot),
        )
        .route(
            "/volumes/:id/snapshots/:snap_id/revert",
            post(volumes::revert_snapshot),
        )
        .route("/images", get(images::list).post(images::create))
        .route("/isos", get(images::list_isos))
        .route("/images/import", post(images::import))
        .route(
            "/images/upload",
            post(images::upload).route_layer(axum::extract::DefaultBodyLimit::disable()),
        )
        .route(
            "/images/:id",
            get(images::get)
                .patch(images::update)
                .delete(images::delete),
        )
        .route("/templates", get(templates::list).post(templates::create))
        .route(
            "/templates/:id",
            get(templates::get)
                .patch(templates::update)
                .delete(templates::delete),
        )
        .route("/tasks", get(tasks::list))
        .route("/tasks/:id", get(tasks::get))
        .route("/events", get(events::list));

    // Agent auto-enrollment (design M16), only when the issuing CA is present.
    // `/hosts/enroll` is RBAC-guarded; `/enroll/sign` is token-gated (no OIDC —
    // it runs before the agent has a client cert).
    if let Some(en) = enrollment {
        v1 = v1
            .route("/hosts/enroll", post(enroll::enroll))
            .route("/enroll/sign", post(enroll::bootstrap_sign))
            .layer(Extension(en));
    }

    // An unmatched /api/v1 path answers with the error envelope. Without this
    // it would reach the outer SPA fallback and a typo'd endpoint would return
    // an HTML page with status 200 — the worst possible answer for a client.
    let v1 = v1
        .fallback(|| async { error::ApiError::route_not_found() })
        .layer(Extension(auth))
        .with_state(store);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", v1)
}
