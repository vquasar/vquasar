//! [`FakeHypervisor`]: an in-memory [`Hypervisor`] for testing controllers and
//! the agent without `/dev/kvm` (design document, section 38).
//!
//! It implements the same state machine semantics the real VMM is expected to
//! honour, including the idempotency the reconciler relies on (section 22).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use vquasar_model::VirtualMachineSpec;

use crate::error::{ChError, Result};
use crate::hypervisor::{Hypervisor, HypervisorState, HypervisorVmInfo};

#[derive(Debug, Default)]
struct State {
    /// `None` until `create` is called.
    current: Option<HypervisorState>,
    created_calls: u32,
    boot_calls: u32,
    shutdown_calls: u32,
    /// When set, the next call to the matching method fails once.
    fail_next_boot: bool,
}

/// A cloneable, in-memory fake VMM handle. Clones share the same state, so a
/// test can keep a handle to inspect calls after handing another to the code
/// under test.
#[derive(Debug, Clone, Default)]
pub struct FakeHypervisor {
    state: Arc<Mutex<State>>,
}

impl FakeHypervisor {
    /// A fresh fake with no VM created.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of times `boot` has completed a state transition attempt.
    pub fn boot_calls(&self) -> u32 {
        self.state.lock().unwrap().boot_calls
    }

    /// Number of times `create` was invoked (including idempotent no-ops).
    pub fn create_calls(&self) -> u32 {
        self.state.lock().unwrap().created_calls
    }

    /// The current observed state, if any.
    pub fn current(&self) -> Option<HypervisorState> {
        self.state.lock().unwrap().current
    }

    /// Arrange for the next `boot` call to fail once (fault injection).
    pub fn fail_next_boot(&self) {
        self.state.lock().unwrap().fail_next_boot = true;
    }
}

#[async_trait]
impl Hypervisor for FakeHypervisor {
    async fn create(&self, _spec: &VirtualMachineSpec) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.created_calls += 1;
        // Idempotent: a second create does not replace an existing VM.
        if s.current.is_none() {
            s.current = Some(HypervisorState::Created);
        }
        Ok(())
    }

    async fn boot(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.boot_calls += 1;
        if s.fail_next_boot {
            s.fail_next_boot = false;
            return Err(ChError::InvalidState("injected boot failure".into()));
        }
        match s.current {
            None => Err(ChError::InvalidState("cannot boot: vm not created".into())),
            Some(HypervisorState::Running) => Ok(()), // idempotent
            Some(_) => {
                s.current = Some(HypervisorState::Running);
                Ok(())
            }
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let mut s = self.state.lock().unwrap();
        s.shutdown_calls += 1;
        match s.current {
            None | Some(HypervisorState::Shutdown) => Ok(()), // idempotent
            Some(_) => {
                s.current = Some(HypervisorState::Shutdown);
                Ok(())
            }
        }
    }

    async fn info(&self) -> Result<HypervisorVmInfo> {
        let s = self.state.lock().unwrap();
        match s.current {
            Some(state) => Ok(HypervisorVmInfo { state }),
            None => Err(ChError::InvalidState("vm not created".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use vquasar_model::{
        BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec, VirtualMachineSpec,
    };

    use super::*;

    fn spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 1,
                max_vcpus: 1,
            },
            memory: MemorySpec {
                size_mib: 128,
                max_size_mib: None,
            },
            boot: BootSpec::DirectKernel {
                kernel: "/boot/vmlinux".into(),
                initramfs: None,
                cmdline: None,
            },
            disks: vec![],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
            cloud_init: None,
            machine_type: vquasar_model::MachineType::Standard,
        }
    }

    #[tokio::test]
    async fn lifecycle_transitions() {
        let hv = FakeHypervisor::new();
        assert!(hv.info().await.is_err(), "no vm before create");

        hv.create(&spec()).await.unwrap();
        assert_eq!(hv.current(), Some(HypervisorState::Created));

        hv.boot().await.unwrap();
        assert_eq!(hv.info().await.unwrap().state, HypervisorState::Running);

        hv.shutdown().await.unwrap();
        assert_eq!(hv.current(), Some(HypervisorState::Shutdown));
    }

    #[tokio::test]
    async fn create_is_idempotent() {
        let hv = FakeHypervisor::new();
        hv.create(&spec()).await.unwrap();
        hv.boot().await.unwrap();
        // A repeated create (e.g. reconciliation running twice) must not reset
        // the running VM (section 22).
        hv.create(&spec()).await.unwrap();
        assert_eq!(hv.current(), Some(HypervisorState::Running));
        assert_eq!(hv.create_calls(), 2);
    }

    #[tokio::test]
    async fn boot_before_create_fails() {
        let hv = FakeHypervisor::new();
        assert!(hv.boot().await.is_err());
    }

    #[tokio::test]
    async fn boot_is_idempotent_when_running() {
        let hv = FakeHypervisor::new();
        hv.create(&spec()).await.unwrap();
        hv.boot().await.unwrap();
        hv.boot().await.unwrap();
        assert_eq!(hv.current(), Some(HypervisorState::Running));
    }

    #[tokio::test]
    async fn fault_injection_fails_once() {
        let hv = FakeHypervisor::new();
        hv.create(&spec()).await.unwrap();
        hv.fail_next_boot();
        assert!(hv.boot().await.is_err());
        // Recovers on retry.
        hv.boot().await.unwrap();
        assert_eq!(hv.current(), Some(HypervisorState::Running));
    }
}
