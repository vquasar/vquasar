//! The [`Hypervisor`] abstraction (design document, sections 10 and 43).
//!
//! Each `Hypervisor` value is a handle to **one** VM instance. This mirrors
//! Cloud Hypervisor's own model — one `cloud-hypervisor` process, with one API
//! socket, per VM.
//!
//! The trait exists for testing, mocking, and isolating Cloud Hypervisor
//! version changes. It is *not* an attempt to support other VMMs (section 10).
//! Cloud Hypervisor-specific request/response types never appear in this
//! interface (ADR-013).

use async_trait::async_trait;
use ch_model::VirtualMachineSpec;

use crate::error::Result;

/// A handle to a single VM instance managed by a VMM.
#[async_trait]
pub trait Hypervisor: Send + Sync {
    /// Translate `spec` and create the VM in the VMM (does not boot it).
    ///
    /// Must be idempotent: creating an already-created VM with the same spec
    /// succeeds without producing a second VM (section 22).
    async fn create(&self, spec: &VirtualMachineSpec) -> Result<()>;

    /// Boot a previously created VM. Idempotent when already running.
    async fn boot(&self) -> Result<()>;

    /// Request an orderly shutdown of the guest. Idempotent when already off.
    async fn shutdown(&self) -> Result<()>;

    /// Query the VM's observed state.
    async fn info(&self) -> Result<HypervisorVmInfo>;
}

/// VMM-facing observed state for a VM (the orchestration model translates this
/// into a [`ch_model::VmPhase`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HypervisorVmInfo {
    pub state: HypervisorState,
}

/// The lifecycle state a VMM reports for a VM.
///
/// These map 1:1 onto Cloud Hypervisor's `VmState` enum
/// (`Created`, `Running`, `Shutdown`, `Paused`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HypervisorState {
    Created,
    Running,
    Shutdown,
    Paused,
}
