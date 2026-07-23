//! The [`HostAgent`] gRPC service (design document, section 12).
//!
//! This is a thin adapter: it validates/parses requests, delegates to the
//! [`VmManager`], and maps results (and typed [`ManagerError`]s) onto the
//! generated protobuf types and gRPC status codes. No VM logic lives here.

// tonic's `Status` is a large error type used pervasively by the generated
// trait; boxing every return would fight the API for no benefit.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use ch_model::{VirtualMachineSpec, VmId, VmPhase};
use ch_proto::agent::host_agent_server::HostAgent;
use ch_proto::agent::vm_observed_state::Phase;
use ch_proto::agent::{
    DeleteVmRequest, EnsureVmRequest, EnsureVmResponse, GetHostInfoRequest, GetHostInfoResponse,
    GetVmRequest, GetVmResponse, ListVmsRequest, ListVmsResponse, OperationResponse,
    StartVmRequest, StopVmRequest, VmObservedState,
};
use tonic::{Request, Response, Status};

use crate::inventory;
use crate::manager::{ManagerError, ObservedVm, VmManager};
use crate::network::NicBinding;

/// gRPC front end for one host's [`VmManager`].
pub struct AgentService {
    manager: Arc<VmManager>,
    host_id: String,
    ch_version: Option<String>,
}

impl AgentService {
    pub fn new(manager: Arc<VmManager>, host_id: String, ch_version: Option<String>) -> Self {
        Self {
            manager,
            host_id,
            ch_version,
        }
    }

    fn parse_id(raw: &str) -> Result<VmId, Status> {
        raw.parse::<VmId>()
            .map_err(|_| Status::invalid_argument(format!("invalid vm_id: {raw}")))
    }
}

#[tonic::async_trait]
impl HostAgent for AgentService {
    async fn get_host_info(
        &self,
        _request: Request<GetHostInfoRequest>,
    ) -> Result<Response<GetHostInfoResponse>, Status> {
        let host = inventory::collect();
        let vm_count = self.manager.list().await.len() as u32;
        Ok(Response::new(GetHostInfoResponse {
            host_id: self.host_id.clone(),
            hostname: host.hostname.unwrap_or_default(),
            architecture: host.architecture.unwrap_or_default(),
            kernel_version: host.kernel_version.unwrap_or_default(),
            cloud_hypervisor_version: self.ch_version.clone().unwrap_or_default(),
            logical_cpus: host.logical_cpus.unwrap_or_default(),
            cpu_model: host.cpu_model.unwrap_or_default(),
            total_memory_bytes: host.total_memory_bytes.unwrap_or_default(),
            available_memory_bytes: host.available_memory_bytes.unwrap_or_default(),
            vm_count,
        }))
    }

    async fn get_vm(
        &self,
        request: Request<GetVmRequest>,
    ) -> Result<Response<GetVmResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        let obs = self.manager.get(id).await.map_err(to_status)?;
        Ok(Response::new(GetVmResponse {
            state: Some(to_proto(obs)),
        }))
    }

    async fn list_vms(
        &self,
        _request: Request<ListVmsRequest>,
    ) -> Result<Response<ListVmsResponse>, Status> {
        let vms = self
            .manager
            .list()
            .await
            .into_iter()
            .map(to_proto)
            .collect();
        Ok(Response::new(ListVmsResponse { vms }))
    }

    async fn ensure_vm(
        &self,
        request: Request<EnsureVmRequest>,
    ) -> Result<Response<EnsureVmResponse>, Status> {
        let req = request.into_inner();
        let id = Self::parse_id(&req.vm_id)?;
        let spec: VirtualMachineSpec = serde_json::from_slice(&req.spec_json)
            .map_err(|e| Status::invalid_argument(format!("invalid spec_json: {e}")))?;
        let bindings = req
            .networks
            .into_iter()
            .map(|n| NicBinding {
                mac: n.mac,
                vlan: n.vlan as u16,
            })
            .collect();
        let obs = self
            .manager
            .ensure(id, req.name, spec, bindings)
            .await
            .map_err(to_status)?;
        Ok(Response::new(EnsureVmResponse {
            state: Some(to_proto(obs)),
        }))
    }

    async fn start_vm(
        &self,
        request: Request<StartVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        let obs = self.manager.start(id).await.map_err(to_status)?;
        Ok(Response::new(accepted(obs)))
    }

    async fn stop_vm(
        &self,
        request: Request<StopVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        let obs = self.manager.stop(id).await.map_err(to_status)?;
        Ok(Response::new(accepted(obs)))
    }

    async fn delete_vm(
        &self,
        request: Request<DeleteVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        self.manager.delete(id).await.map_err(to_status)?;
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "deleted".to_string(),
        }))
    }
}

fn accepted(obs: ObservedVm) -> OperationResponse {
    OperationResponse {
        accepted: true,
        message: format!("{:?}", obs.phase),
    }
}

fn to_proto(obs: ObservedVm) -> VmObservedState {
    VmObservedState {
        vm_id: obs.id.to_string(),
        phase: phase_to_proto(obs.phase) as i32,
        message: obs.message.unwrap_or_default(),
    }
}

fn phase_to_proto(phase: VmPhase) -> Phase {
    match phase {
        VmPhase::Pending | VmPhase::Scheduling => Phase::Pending,
        VmPhase::Creating => Phase::Creating,
        VmPhase::Stopped => Phase::Stopped,
        VmPhase::Starting => Phase::Starting,
        // The proto has no Migrating variant yet; report the VM as running.
        VmPhase::Running | VmPhase::Migrating => Phase::Running,
        VmPhase::Stopping => Phase::Stopping,
        VmPhase::Failed => Phase::Failed,
        VmPhase::Deleting => Phase::Deleting,
    }
}

fn to_status(err: ManagerError) -> Status {
    match err {
        ManagerError::NotFound(id) => Status::not_found(format!("vm not found: {id}")),
        ManagerError::InvalidSpec(msg) => Status::invalid_argument(msg),
        ManagerError::Hypervisor(e) => Status::internal(e.to_string()),
        ManagerError::Network(e) => Status::internal(e.to_string()),
        ManagerError::Io(e) => Status::internal(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use ch_model::{
        BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec, VirtualMachineSpec,
    };

    use crate::backend::FakeBackend;
    use crate::runtime::RuntimeLayout;

    use super::*;

    fn service(dir: &std::path::Path) -> AgentService {
        let backend = Arc::new(FakeBackend::new());
        let network = Arc::new(crate::network::NoopNetworkBackend);
        let manager = Arc::new(VmManager::new(backend, network, RuntimeLayout::new(dir)));
        AgentService::new(manager, "host-test".into(), Some("v53.0".into()))
    }

    fn spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 1,
                max_vcpus: 1,
            },
            memory: MemorySpec { size_mib: 512 },
            boot: BootSpec::DirectKernel {
                kernel: "/boot/vmlinux".into(),
                initramfs: None,
                cmdline: None,
            },
            disks: vec![],
            network_interfaces: vec![],
            placement: PlacementSpec::default(),
        }
    }

    #[tokio::test]
    async fn full_vm_lifecycle_over_grpc() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let id = VmId::new();

        // EnsureVm creates and boots.
        let ensure = svc
            .ensure_vm(Request::new(EnsureVmRequest {
                vm_id: id.to_string(),
                name: "web-1".into(),
                spec_json: serde_json::to_vec(&spec()).unwrap(),
                networks: vec![],
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(ensure.state.unwrap().phase, Phase::Running as i32);

        // Host info reflects the CH version and VM count.
        let info = svc
            .get_host_info(Request::new(GetHostInfoRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(info.cloud_hypervisor_version, "v53.0");
        assert_eq!(info.vm_count, 1);
        assert_eq!(info.host_id, "host-test");

        // Stop then start.
        let stopped = svc
            .stop_vm(Request::new(StopVmRequest {
                vm_id: id.to_string(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert!(stopped.accepted);

        let listed = svc
            .list_vms(Request::new(ListVmsRequest {}))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(listed.vms.len(), 1);
        assert_eq!(listed.vms[0].phase, Phase::Stopped as i32);

        svc.start_vm(Request::new(StartVmRequest {
            vm_id: id.to_string(),
        }))
        .await
        .unwrap();

        // Delete, then it is gone.
        svc.delete_vm(Request::new(DeleteVmRequest {
            vm_id: id.to_string(),
        }))
        .await
        .unwrap();
        let missing = svc
            .get_vm(Request::new(GetVmRequest {
                vm_id: id.to_string(),
            }))
            .await;
        assert_eq!(missing.unwrap_err().code(), tonic::Code::NotFound);
    }

    #[tokio::test]
    async fn invalid_spec_json_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let err = svc
            .ensure_vm(Request::new(EnsureVmRequest {
                vm_id: VmId::new().to_string(),
                name: "bad".into(),
                spec_json: b"not json".to_vec(),
                networks: vec![],
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
