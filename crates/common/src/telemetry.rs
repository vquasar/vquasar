//! Structured-tracing initialisation shared by the `ch-control` and `ch-agent`
//! binaries.
//!
//! Every service emits structured logs via `tracing`. Log verbosity is taken
//! from the `RUST_LOG` environment variable, falling back to the level the
//! service passes in.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise the global tracing subscriber.
///
/// `default_level` is used when `RUST_LOG` is unset (e.g. `"info"`). When
/// `json` is true, logs are emitted as structured JSON lines (one event per
/// line) for ingestion by log/trace aggregators (design M17); otherwise the
/// human-readable text format is used. Safe to call once per process; a second
/// call returns an error from the global dispatcher, which callers may ignore
/// in tests.
pub fn init(default_level: &str, json: bool) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    let registry = tracing_subscriber::registry().with(filter);
    // Ignore the error when a subscriber is already installed (e.g. repeated
    // initialisation across tests).
    if json {
        let _ = registry
            .with(fmt::layer().json().with_target(true).with_current_span(true))
            .try_init();
    } else {
        let _ = registry
            .with(fmt::layer().with_target(true).with_level(true))
            .try_init();
    }
}
