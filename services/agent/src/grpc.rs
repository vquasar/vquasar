//! The [`HostAgent`] gRPC service (design document, section 12).
//!
//! This is a thin adapter: it validates/parses requests, delegates to the
//! [`VmManager`], and maps results (and typed [`ManagerError`]s) onto the
//! generated protobuf types and gRPC status codes. No VM logic lives here.

// tonic's `Status` is a large error type used pervasively by the generated
// trait; boxing every return would fight the API for no benefit.
#![allow(clippy::result_large_err)]

use std::pin::Pin;
use std::sync::Arc;

use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status, Streaming};
use vquasar_model::{VirtualMachineSpec, VmId, VmPhase};
use vquasar_proto::agent::host_agent_server::HostAgent;
use vquasar_proto::agent::vm_observed_state::Phase;
use vquasar_proto::agent::{
    ConsoleClientMessage, ConsoleServerMessage, DeleteVmRequest, DeleteVolumeRequest,
    DiscardVmRequest, EnsureVmRequest, EnsureVmResponse, FinalizeReceiveRequest,
    GetHostInfoRequest, GetHostInfoResponse, GetVmMetricsRequest, GetVmRequest, GetVmResponse,
    ListVmsRequest, ListVmsResponse, OperationResponse, PrepareReceiveRequest,
    PrepareReceiveResponse, ProvisionVolumeRequest, ProvisionVolumeResponse, SendMigrationRequest,
    StartVmRequest, StopVmRequest, VmMetricsResponse, VmObservedState,
};

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
        request: Request<GetHostInfoRequest>,
    ) -> Result<Response<GetHostInfoResponse>, Status> {
        let probes = request.into_inner().pools;
        let host = inventory::collect();
        let vm_count = self.manager.list().await.len() as u32;
        // Observed, per tick, alongside the rest of the inventory: whether this
        // host can really use each pool the control plane knows about, and how
        // much room it has (ADR-023).
        let storage_pools = crate::pools::probe_all(&probes, &self.host_id).await;
        Ok(Response::new(GetHostInfoResponse {
            host_id: self.host_id.clone(),
            hostname: host.hostname.unwrap_or_default(),
            architecture: host.architecture.unwrap_or_default(),
            kernel_version: host.kernel_version.unwrap_or_default(),
            cloud_hypervisor_version: self.ch_version.clone().unwrap_or_default(),
            logical_cpus: host.logical_cpus.unwrap_or_default(),
            cpu_model: host.cpu_model.unwrap_or_default(),
            cpu_vendor: host.cpu_vendor.unwrap_or_default(),
            overlay_vnis: crate::network::overlay_vnis().await,
            cpu_features: host.cpu_features,
            total_memory_bytes: host.total_memory_bytes.unwrap_or_default(),
            available_memory_bytes: host.available_memory_bytes.unwrap_or_default(),
            vm_count,
            storage_pools,
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

    async fn get_vm_metrics(
        &self,
        request: Request<GetVmMetricsRequest>,
    ) -> Result<Response<VmMetricsResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        let m = self.manager.metrics(id).await;
        Ok(Response::new(VmMetricsResponse {
            running: m.running,
            cpu_pct: m.cpu_pct,
            mem_bytes: m.mem_bytes,
            disk_read_bytes: m.disk_read_bytes,
            disk_write_bytes: m.disk_write_bytes,
            disk_read_ops: m.disk_read_ops,
            disk_write_ops: m.disk_write_ops,
            net_rx_bytes: m.net_rx_bytes,
            net_tx_bytes: m.net_tx_bytes,
            net_rx_packets: m.net_rx_packets,
            net_tx_packets: m.net_tx_packets,
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
                vni: n.vni,
                overlay_peers: n.overlay_peers,
                encrypt_underlay: n.encrypt_underlay,
                overlay_peer_identities: n
                    .overlay_peer_identities
                    .into_iter()
                    .map(|p| crate::network::OverlayPeerId {
                        underlay_ip: p.underlay_ip,
                        cert_cn: p.cert_cn,
                    })
                    .collect(),
                filtered: n.filtered,
                ingress_rules: n.ingress_rules.into_iter().map(sec_rule).collect(),
                egress_rules: n.egress_rules.into_iter().map(sec_rule).collect(),
                egress_default_deny: n.egress_default_deny,
            })
            .collect();
        let network_config = Some(req.network_config).filter(|s| !s.is_empty());
        let phone_home_token = Some(req.phone_home_token).filter(|s| !s.is_empty());
        let obs = self
            .manager
            .ensure(
                id,
                req.name,
                spec,
                bindings,
                network_config,
                phone_home_token,
            )
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

    async fn prepare_receive(
        &self,
        request: Request<PrepareReceiveRequest>,
    ) -> Result<Response<PrepareReceiveResponse>, Status> {
        let req = request.into_inner();
        let id = Self::parse_id(&req.vm_id)?;
        let spec: VirtualMachineSpec = serde_json::from_slice(&req.spec_json)
            .map_err(|e| Status::invalid_argument(format!("invalid spec_json: {e}")))?;
        let migration_url = self
            .manager
            .prepare_receive(id, req.name, spec)
            .await
            .map_err(to_status)?;
        Ok(Response::new(PrepareReceiveResponse { migration_url }))
    }

    async fn send_migration(
        &self,
        request: Request<SendMigrationRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let req = request.into_inner();
        let id = Self::parse_id(&req.vm_id)?;
        self.manager
            .send_migration(id, &req.destination_url)
            .await
            .map_err(to_status)?;
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "migration sent".to_string(),
        }))
    }

    async fn finalize_receive(
        &self,
        request: Request<FinalizeReceiveRequest>,
    ) -> Result<Response<EnsureVmResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        let obs = self.manager.finalize_receive(id).await.map_err(to_status)?;
        Ok(Response::new(EnsureVmResponse {
            state: Some(to_proto(obs)),
        }))
    }

    async fn discard_vm(
        &self,
        request: Request<DiscardVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = Self::parse_id(&request.into_inner().vm_id)?;
        self.manager.discard(id).await.map_err(to_status)?;
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "discarded".to_string(),
        }))
    }

    type VmConsoleStream = Pin<Box<dyn Stream<Item = Result<ConsoleServerMessage, Status>> + Send>>;

    async fn vm_console(
        &self,
        request: Request<Streaming<ConsoleClientMessage>>,
    ) -> Result<Response<Self::VmConsoleStream>, Status> {
        let mut inbound = request.into_inner();
        // The first message selects the VM.
        let first = inbound
            .message()
            .await?
            .ok_or_else(|| Status::invalid_argument("empty console stream"))?;
        let id = Self::parse_id(&first.vm_id)?;
        let (output, input) = self
            .manager
            .console(id)
            .await
            .ok_or_else(|| Status::not_found(format!("vm not found: {id}")))?;

        // Pump client input (keystrokes) to the guest.
        tokio::spawn(async move {
            if !first.input.is_empty() {
                let _ = input.send(first.input).await;
            }
            while let Ok(Some(msg)) = inbound.message().await {
                if !msg.input.is_empty() && input.send(msg.input).await.is_err() {
                    break;
                }
            }
        });

        // Stream guest serial output back, skipping lag gaps.
        let stream = BroadcastStream::new(output).filter_map(|item| match item {
            Ok(bytes) => Some(Ok(ConsoleServerMessage { output: bytes })),
            Err(_lagged) => None,
        });
        Ok(Response::new(Box::pin(stream)))
    }
    async fn provision_volume(
        &self,
        request: Request<ProvisionVolumeRequest>,
    ) -> Result<Response<ProvisionVolumeResponse>, Status> {
        let r = request.into_inner();
        let size_bytes = self
            .manager
            .storage()
            .provision_volume(
                std::path::Path::new(&r.path),
                &r.format,
                r.size_bytes,
                (!r.source_path.is_empty()).then(|| std::path::Path::new(&r.source_path)),
                Some(r.preallocation.as_str()).filter(|p| !p.is_empty()),
            )
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(ProvisionVolumeResponse { size_bytes }))
    }

    async fn delete_volume(
        &self,
        request: Request<DeleteVolumeRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let r = request.into_inner();
        self.manager
            .storage()
            .delete_volume(std::path::Path::new(&r.path))
            .await
            .map_err(|e| Status::internal(e.to_string()))?;
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "removed".into(),
        }))
    }
}

/// One wire rule to one firewall rule. Direction is the list it came from, so
/// both directions share this.
fn sec_rule(r: vquasar_proto::agent::SecurityRule) -> crate::firewall::SecRule {
    crate::firewall::SecRule {
        ipv6: r.ipv6,
        protocol: r.protocol,
        port_min: r.port_min as u16,
        port_max: r.port_max as u16,
        remote_cidr: r.remote_cidr,
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
        ip_address: obs.ip.unwrap_or_default(),
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
        ManagerError::Storage(e) => Status::internal(e.to_string()),
        ManagerError::Io(e) => Status::internal(e.to_string()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use vquasar_model::{
        BootSpec, CpuSpec, DesiredPowerState, MemorySpec, PlacementSpec, VirtualMachineSpec,
    };

    use crate::backend::FakeBackend;
    use crate::runtime::RuntimeLayout;

    use super::*;

    pub(crate) fn service(dir: &std::path::Path) -> AgentService {
        let backend = Arc::new(FakeBackend::new());
        let network = Arc::new(crate::network::NoopNetworkBackend);
        let migration = crate::manager::MigrationSettings {
            transport: "unix".to_string(),
            advertise_host: String::new(),
            port_min: 9600,
            port_max: 9700,
            socket_dir: dir.join("migrations"),
        };
        let manager = Arc::new(VmManager::new(
            backend,
            network,
            crate::storage::StorageProvisioner::new(dir.join("shared")),
            crate::ipdiscovery::IpDiscovery::new("br-int"),
            RuntimeLayout::new(dir),
            migration,
        ));
        AgentService::new(manager, "host-test".into(), Some("v53.0".into()))
    }

    fn spec() -> VirtualMachineSpec {
        VirtualMachineSpec {
            desired_power_state: DesiredPowerState::Running,
            cpu: CpuSpec {
                boot_vcpus: 1,
                max_vcpus: 1,
            },
            memory: MemorySpec {
                size_mib: 512,
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
    async fn full_vm_lifecycle_over_grpc() {
        let dir = tempfile::tempdir().unwrap();
        let svc = service(dir.path());
        let id = VmId::new();

        // EnsureVm creates and boots.
        let ensure = svc
            .ensure_vm(Request::new(EnsureVmRequest {
                phone_home_token: String::new(),
                vm_id: id.to_string(),
                name: "web-1".into(),
                spec_json: serde_json::to_vec(&spec()).unwrap(),
                networks: vec![],
                network_config: String::new(),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(ensure.state.unwrap().phase, Phase::Running as i32);

        // Host info reflects the CH version and VM count.
        let info = svc
            .get_host_info(Request::new(GetHostInfoRequest::default()))
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
                phone_home_token: String::new(),
                vm_id: VmId::new().to_string(),
                name: "bad".into(),
                spec_json: b"not json".to_vec(),
                networks: vec![],
                network_config: String::new(),
            }))
            .await
            .unwrap_err();
        assert_eq!(err.code(), tonic::Code::InvalidArgument);
    }
}
