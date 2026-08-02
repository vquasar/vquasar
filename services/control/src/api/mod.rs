//! The public REST API (design document, section 14). Versioned under
//! `/api/v1`. Handlers are thin: they validate input, call the [`Store`], and
//! return typed errors — no infrastructure logic here (ADR-015).

pub mod error;
mod events;
mod hosts;
mod images;
mod networks;
mod tasks;
mod templates;
mod vms;

use axum::routing::{get, post};
use axum::Router;

use crate::store::Store;

/// Build the `/api/v1` router bound to `store`.
pub fn router(store: Store) -> Router {
    let v1 = Router::new()
        .route("/hosts", get(hosts::list).post(hosts::register))
        .route("/hosts/:id", get(hosts::get))
        .route("/vms", get(vms::list).post(vms::create))
        .route("/vms/from-template", post(vms::create_from_template))
        .route(
            "/vms/:id",
            get(vms::get).patch(vms::update).delete(vms::delete),
        )
        .route("/vms/:id/start", post(vms::start))
        .route("/vms/:id/stop", post(vms::stop))
        .route("/vms/:id/migrate", post(vms::migrate))
        .route("/vms/:id/console", get(crate::console::console_ws))
        .route("/networks", get(networks::list).post(networks::create))
        .route(
            "/networks/:id",
            get(networks::get)
                .patch(networks::update)
                .delete(networks::delete),
        )
        .route("/images", get(images::list).post(images::create))
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
        .route("/events", get(events::list))
        .with_state(store);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", v1)
}
