//! Errors produced by the Cloud Hypervisor client.

use ch_common::DomainError;
use thiserror::Error;

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, ChError>;

/// A failure interacting with a Cloud Hypervisor instance.
#[derive(Debug, Error)]
pub enum ChError {
    /// Failed to spawn or manage the `cloud-hypervisor` process.
    #[error("failed to launch cloud-hypervisor: {0}")]
    Spawn(#[source] std::io::Error),

    /// The API socket did not become ready within the timeout.
    #[error("timed out waiting for the Cloud Hypervisor API socket")]
    SocketTimeout,

    /// The VMM returned a non-success HTTP status.
    #[error("cloud-hypervisor API returned {status}: {body}")]
    Api { status: u16, body: String },

    /// A transport-level failure talking to the API socket.
    #[error("cloud-hypervisor API transport error: {0}")]
    Transport(String),

    /// (De)serialising an API request or response failed.
    #[error("cloud-hypervisor API serialization error: {0}")]
    Serialization(#[source] serde_json::Error),

    /// Low-level I/O failure (socket connect, file handling).
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),

    /// The operation is not valid for the VM's current state.
    #[error("invalid hypervisor state: {0}")]
    InvalidState(String),
}

impl From<ChError> for DomainError {
    fn from(e: ChError) -> Self {
        // Everything the hypervisor layer produces is surfaced to the public
        // API as a single opaque class (design document, section 37): never
        // leak transport or socket detail verbatim.
        DomainError::HypervisorError(e.to_string())
    }
}
