//! The [`Host`] resource: a virtualization host running `ch-agent` and Cloud
//! Hypervisor (design document, section 9).
//!
//! Most host fields are *observed* inventory reported by the agent. The model
//! deliberately makes the richer fields (NUMA, PCI, IOMMU, GPU, SEV-SNP/TDX)
//! optional so they can be populated incrementally without a schema change
//! (section 9: "the model should allow them").

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::ids::HostId;
use crate::meta::Metadata;

/// A managed virtualization host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub id: HostId,
    #[serde(flatten)]
    pub meta: Metadata,
    pub spec: HostSpec,
    pub status: HostStatus,
}

/// Administrative intent for a host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostSpec {
    /// Whether the scheduler may place new VMs here. An operator can set this
    /// `false` to drain a host for maintenance.
    pub schedulable: bool,
}

impl Default for HostSpec {
    fn default() -> Self {
        Self { schedulable: true }
    }
}

/// Observed host inventory and health.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostStatus {
    pub state: HostState,
    pub hostname: Option<String>,
    pub architecture: Option<String>,
    pub kernel_version: Option<String>,
    pub cloud_hypervisor_version: Option<String>,
    pub logical_cpus: Option<u32>,
    pub cpu_model: Option<String>,
    /// CPU vendor id (e.g. `GenuineIntel`), used with `cpu_features` to gate
    /// live migration between hosts with different CPUs (design M15,
    /// cross-CPU migration).
    pub cpu_vendor: Option<String>,
    /// Curated guest-visible CPU ISA feature flags (a subset of
    /// `/proc/cpuinfo` flags), sorted. Migration to a host that lacks a
    /// feature the source guest could use would fault the guest, so the
    /// control plane refuses it (Cloud Hypervisor cannot mask CPUID).
    #[serde(default)]
    pub cpu_features: Vec<String>,
    pub total_memory_bytes: Option<u64>,
    pub available_memory_bytes: Option<u64>,
    /// Number of VMs currently placed on this host.
    #[serde(default)]
    pub vm_count: u32,
    pub last_heartbeat: Option<DateTime<Utc>>,
}

impl HostStatus {
    /// Whether the scheduler may consider this host (section 17: "filter
    /// unavailable hosts").
    pub fn is_ready(&self) -> bool {
        self.state == HostState::Ready
    }
}

/// Host availability state (section 26).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum HostState {
    Ready,
    /// Heartbeats missed. Note: VMs are **not** relocated on `NotReady` — that
    /// requires fencing (ADR-014, section 27).
    #[default]
    NotReady,
    Maintenance,
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newly_seen_host_defaults_to_not_ready() {
        let status = HostStatus::default();
        assert_eq!(status.state, HostState::NotReady);
        assert!(!status.is_ready());
    }

    #[test]
    fn host_spec_defaults_to_schedulable() {
        assert!(HostSpec::default().schedulable);
    }

    #[test]
    fn host_id_is_typed() {
        let id = HostId::new();
        assert_ne!(id, HostId::new());
    }
}
