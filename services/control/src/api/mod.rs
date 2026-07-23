//! The public REST API (design document, section 14). Versioned under
//! `/api/v1`. Handlers are thin: they validate input, call the [`Store`], and
//! return typed errors — no infrastructure logic here (ADR-015).

pub mod error;
mod events;
mod hosts;
mod networks;
mod tasks;
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
        .route("/vms/:id", get(vms::get).delete(vms::delete))
        .route("/vms/:id/start", post(vms::start))
        .route("/vms/:id/stop", post(vms::stop))
        .route("/vms/:id/migrate", post(vms::migrate))
        .route("/vms/:id/console", get(crate::console::console_ws))
        .route("/networks", get(networks::list).post(networks::create))
        .route("/networks/:id", get(networks::get).delete(networks::delete))
        .route("/tasks", get(tasks::list))
        .route("/tasks/:id", get(tasks::get))
        .route("/events", get(events::list))
        .with_state(store);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .nest("/api/v1", v1)
}
