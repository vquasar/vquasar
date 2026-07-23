//! Structured-tracing initialisation shared by the `ch-control` and `ch-agent`
//! binaries.
//!
//! Every service emits structured logs via `tracing`. Log verbosity is taken
//! from the `RUST_LOG` environment variable, falling back to the level the
//! service passes in.

use tracing_subscriber::{fmt, prelude::*, EnvFilter};

/// Initialise the global tracing subscriber.
///
/// `default_level` is used when `RUST_LOG` is unset (e.g. `"info"`). Safe to
/// call once per process; a second call returns an error from the global
/// dispatcher, which callers may ignore in tests.
pub fn init(default_level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));

    let fmt_layer = fmt::layer().with_target(true).with_level(true);

    // Ignore the error when a subscriber is already installed (e.g. repeated
    // initialisation across tests).
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt_layer)
        .try_init();
}
