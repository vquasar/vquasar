//! Cross-cutting concerns shared by every `ch-orchestrator` crate.
//!
//! Kept deliberately small: a stable domain-error taxonomy (see the design
//! document, section 37) and telemetry initialisation. Business logic lives in
//! `ch-model` and above; this crate must not depend on them.

pub mod error;
pub mod telemetry;

pub use error::{DomainError, ErrorCode};
