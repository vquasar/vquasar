//! The stable domain-error taxonomy (design document, section 37).
//!
//! These variants form a *stable, public* contract: each maps to a stable
//! machine-readable [`ErrorCode`] that the REST API surfaces to clients. Raw
//! internal errors (database, hypervisor transport, ...) must be translated
//! into one of these before crossing the public API boundary.

use std::fmt;

use serde::Serialize;
use thiserror::Error;

/// A domain-level error with a stable, client-facing classification.
#[derive(Debug, Error)]
pub enum DomainError {
    #[error("insufficient resources: {0}")]
    InsufficientResources(String),

    #[error("host unavailable: {0}")]
    HostUnavailable(String),

    #[error("virtual machine not found: {0}")]
    VmNotFound(String),

    #[error("virtual machine already running: {0}")]
    VmAlreadyRunning(String),

    #[error("network unavailable: {0}")]
    NetworkUnavailable(String),

    #[error("storage unavailable: {0}")]
    StorageUnavailable(String),

    #[error("hypervisor error: {0}")]
    HypervisorError(String),

    #[error("agent unavailable: {0}")]
    AgentUnavailable(String),

    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),

    /// An unexpected internal failure. Never leak the inner detail verbatim to
    /// API clients; the [`ErrorCode`] is intentionally opaque.
    #[error("internal error: {0}")]
    Internal(String),
}

impl DomainError {
    /// The stable, machine-readable code for this error.
    ///
    /// These strings are part of the public API contract and must not change
    /// without an API version bump.
    pub fn code(&self) -> ErrorCode {
        match self {
            DomainError::InsufficientResources(_) => ErrorCode::InsufficientResources,
            DomainError::HostUnavailable(_) => ErrorCode::HostUnavailable,
            DomainError::VmNotFound(_) => ErrorCode::VmNotFound,
            DomainError::VmAlreadyRunning(_) => ErrorCode::VmAlreadyRunning,
            DomainError::NetworkUnavailable(_) => ErrorCode::NetworkUnavailable,
            DomainError::StorageUnavailable(_) => ErrorCode::StorageUnavailable,
            DomainError::HypervisorError(_) => ErrorCode::HypervisorError,
            DomainError::AgentUnavailable(_) => ErrorCode::AgentUnavailable,
            DomainError::InvalidConfiguration(_) => ErrorCode::InvalidConfiguration,
            DomainError::Internal(_) => ErrorCode::Internal,
        }
    }
}

/// Stable machine-readable error codes surfaced through the public REST API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InsufficientResources,
    HostUnavailable,
    VmNotFound,
    VmAlreadyRunning,
    NetworkUnavailable,
    StorageUnavailable,
    HypervisorError,
    AgentUnavailable,
    InvalidConfiguration,
    Unauthorized,
    Forbidden,
    Internal,
}

impl ErrorCode {
    /// The wire representation, e.g. `HOST_UNAVAILABLE`.
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::InsufficientResources => "INSUFFICIENT_RESOURCES",
            ErrorCode::HostUnavailable => "HOST_UNAVAILABLE",
            ErrorCode::VmNotFound => "VM_NOT_FOUND",
            ErrorCode::VmAlreadyRunning => "VM_ALREADY_RUNNING",
            ErrorCode::NetworkUnavailable => "NETWORK_UNAVAILABLE",
            ErrorCode::StorageUnavailable => "STORAGE_UNAVAILABLE",
            ErrorCode::HypervisorError => "HYPERVISOR_ERROR",
            ErrorCode::AgentUnavailable => "AGENT_UNAVAILABLE",
            ErrorCode::InvalidConfiguration => "INVALID_CONFIGURATION",
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::Forbidden => "FORBIDDEN",
            ErrorCode::Internal => "INTERNAL",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_maps_to_stable_wire_string() {
        let err = DomainError::HostUnavailable("host-01".into());
        assert_eq!(err.code(), ErrorCode::HostUnavailable);
        assert_eq!(err.code().as_str(), "HOST_UNAVAILABLE");
    }

    #[test]
    fn every_variant_has_a_distinct_code() {
        let all = [
            DomainError::InsufficientResources(String::new()),
            DomainError::HostUnavailable(String::new()),
            DomainError::VmNotFound(String::new()),
            DomainError::VmAlreadyRunning(String::new()),
            DomainError::NetworkUnavailable(String::new()),
            DomainError::StorageUnavailable(String::new()),
            DomainError::HypervisorError(String::new()),
            DomainError::AgentUnavailable(String::new()),
            DomainError::InvalidConfiguration(String::new()),
            DomainError::Internal(String::new()),
        ];
        let mut codes: Vec<&str> = all.iter().map(|e| e.code().as_str()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), all.len(), "error codes must be unique");
    }
}
