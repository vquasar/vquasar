//! The `ch-orchestrator` domain model.
//!
//! This crate defines the *stable orchestration model* (design document,
//! section 6). It is intentionally independent of Cloud Hypervisor's own
//! configuration format (ADR-013): the orchestration model must remain stable
//! even when the underlying VMM changes.
//!
//! Two rules from the design are encoded structurally here:
//!
//! * Persistent resources carry `id` / `name` / `created_at` / `updated_at` /
//!   `generation` ([`Metadata`]).
//! * Reconciled resources separate desired state (`spec`) from observed state
//!   (`status`) — see [`VirtualMachine`] (section 7).

pub mod host;
pub mod ids;
pub mod meta;
pub mod validation;
pub mod vm;

pub use host::{Host, HostSpec, HostState, HostStatus};
pub use ids::{HostId, NetworkId, TaskId, VmId};
pub use meta::{Generation, Metadata};
pub use validation::ValidationError;
pub use vm::{
    BootSpec, CloudInitSpec, CpuSpec, DesiredPowerState, DiskImageType, DiskSpec, MemorySpec,
    NetworkInterfaceSpec, PlacementSpec, VirtualMachine, VirtualMachineSpec, VirtualMachineStatus,
    VmPhase,
};
