//! Control-plane client for the host-agent gRPC API (design document, section
//! 12). One [`Agent`] targets one host's endpoint.
//!
//! A fresh channel is opened per call: agent traffic is low-frequency and this
//! keeps the control plane free of per-host connection state for now.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::OnceLock;

use tonic::service::interceptor::InterceptedService;
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Request, Status};
use vquasar_proto::agent::host_agent_client::HostAgentClient;
use vquasar_proto::agent::{
    DeleteVmRequest, DiscardVmRequest, EnsureVmRequest, FinalizeReceiveRequest, GetHostInfoRequest,
    GetHostInfoResponse, GetVmMetricsRequest, ListVmsRequest, NetworkBinding,
    PrepareReceiveRequest, SendMigrationRequest, VmMetricsResponse, VmObservedState,
};

/// Process-wide mTLS material for talking to agents (design M12a). Set once at
/// startup; `None` means plaintext. A global keeps every `Agent::new` call site
/// (and the console proxy) TLS-aware without threading config through each one.
static CLIENT_TLS: OnceLock<Option<ClientTls>> = OnceLock::new();

struct ClientTls {
    ca: Vec<u8>,
    cert: Vec<u8>,
    key: Vec<u8>,
}

/// Configure agent connections to use mutual TLS. Call once before serving.
pub fn init_client_tls(ca: Vec<u8>, cert: Vec<u8>, key: Vec<u8>) {
    let _ = CLIENT_TLS.set(Some(ClientTls { ca, cert, key }));
}

/// The epoch of the lease this instance currently holds, or 0 for "not leading".
///
/// Process-wide, in the same spirit as `CLIENT_TLS`: `Agent::new` is called from
/// a dozen places that have no business knowing about leases, and threading the
/// lease through each would put control-plane bookkeeping into every signature.
/// The lease loop is the single writer.
static CURRENT_EPOCH: AtomicI64 = AtomicI64::new(0);

/// Record the epoch of the lease term this instance is serving (ADR-022).
///
/// Zero means "not leading, or not yet known" and stamps nothing, which reads to
/// an agent exactly like a controller that predates this — accepted while it is
/// lenient, refused once it is strict. That is the correct answer for a
/// non-leader: it should not be issuing agent RPCs at all.
pub fn set_current_epoch(epoch: i64) {
    CURRENT_EPOCH.store(epoch, Ordering::Release);
}

/// Stamp each outgoing request with the current lease epoch, so the agent can
/// refuse this instance once someone else has taken over (ADR-022).
///
/// An interceptor rather than thirteen call sites: it cannot be forgotten when
/// an RPC is added, which is the failure mode that matters for a fencing token.
#[derive(Clone, Copy)]
pub struct StampEpoch;

impl tonic::service::Interceptor for StampEpoch {
    fn call(&mut self, mut req: Request<()>) -> Result<Request<()>, Status> {
        let epoch = CURRENT_EPOCH.load(Ordering::Acquire);
        if epoch > 0 {
            if let Ok(v) = epoch.to_string().parse() {
                req.metadata_mut().insert(EPOCH_KEY, v);
            }
        }
        Ok(req)
    }
}

/// Must match the agent's `epoch::EPOCH_KEY`. Duplicated rather than shared
/// because the two services share no crate for it, and a gRPC metadata key is
/// wire format — a change on one side is a change to the protocol either way.
const EPOCH_KEY: &str = "x-vquasar-controller-epoch";

/// A client channel with the epoch stamp applied.
pub type AgentClient = HostAgentClient<InterceptedService<Channel, StampEpoch>>;

/// Connect to a host agent, applying mutual TLS when configured. The endpoint's
/// scheme is upgraded to https and its host must match the agent cert SAN.
pub async fn connect_host_agent(endpoint: &str) -> Result<AgentClient, AgentError> {
    let channel = connect_channel(endpoint).await?;
    Ok(HostAgentClient::with_interceptor(channel, StampEpoch))
}

async fn connect_channel(endpoint: &str) -> Result<Channel, AgentError> {
    match CLIENT_TLS.get().and_then(|o| o.as_ref()) {
        Some(tls) => {
            let url = endpoint.replacen("http://", "https://", 1);
            let cfg = ClientTlsConfig::new()
                .ca_certificate(Certificate::from_pem(&tls.ca))
                .identity(Identity::from_pem(&tls.cert, &tls.key));
            let endpoint = Endpoint::from_shared(url.clone())
                .and_then(|ep| ep.tls_config(cfg))
                .map_err(|source| AgentError::Connect {
                    endpoint: url.clone(),
                    source,
                })?;
            let channel = endpoint
                .connect()
                .await
                .map_err(|source| AgentError::Connect {
                    endpoint: url,
                    source,
                })?;
            Ok(channel)
        }
        None => Endpoint::from_shared(endpoint.to_string())
            .map_err(|source| AgentError::Connect {
                endpoint: endpoint.to_string(),
                source,
            })?
            .connect()
            .await
            .map_err(|source| AgentError::Connect {
                endpoint: endpoint.to_string(),
                source,
            }),
    }
}

/// A failure talking to a host agent.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error("connect to agent {endpoint}: {source}")]
    Connect {
        endpoint: String,
        #[source]
        source: tonic::transport::Error,
    },
    #[error("agent rpc failed: {0}")]
    Rpc(#[from] tonic::Status),
}

/// A handle to one host's agent.
pub struct Agent {
    endpoint: String,
}

impl Agent {
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
        }
    }

    async fn client(&self) -> Result<AgentClient, AgentError> {
        connect_host_agent(&self.endpoint).await
    }

    pub async fn get_host_info(&self) -> Result<GetHostInfoResponse, AgentError> {
        let mut client = self.client().await?;
        Ok(client
            .get_host_info(GetHostInfoRequest {})
            .await?
            .into_inner())
    }

    /// Observed state of every VM the host currently manages (used to refresh
    /// live fields like the discovered IP each tick — design M11).
    pub async fn list_vms(&self) -> Result<Vec<VmObservedState>, AgentError> {
        let mut client = self.client().await?;
        Ok(client.list_vms(ListVmsRequest {}).await?.into_inner().vms)
    }

    pub async fn get_vm_metrics(&self, vm_id: String) -> Result<VmMetricsResponse, AgentError> {
        let mut client = self.client().await?;
        Ok(client
            .get_vm_metrics(GetVmMetricsRequest { vm_id })
            .await?
            .into_inner())
    }

    /// Reconcile a VM towards `spec_json` (the JSON-encoded orchestration spec)
    /// with resolved per-NIC dataplane bindings.
    pub async fn ensure_vm(
        &self,
        vm_id: String,
        name: String,
        spec_json: Vec<u8>,
        networks: Vec<NetworkBinding>,
        network_config: String,
        phone_home_token: String,
    ) -> Result<VmObservedState, AgentError> {
        let mut client = self.client().await?;
        let resp = client
            .ensure_vm(EnsureVmRequest {
                phone_home_token,
                vm_id,
                name,
                spec_json,
                networks,
                network_config,
            })
            .await?
            .into_inner();
        Ok(resp.state.unwrap_or_default())
    }

    pub async fn delete_vm(&self, vm_id: String) -> Result<(), AgentError> {
        let mut client = self.client().await?;
        client.delete_vm(DeleteVmRequest { vm_id }).await?;
        Ok(())
    }

    // ---- live migration (section 28) ------------------------------------

    /// Destination: launch a receiver and return the URL the source sends to.
    pub async fn prepare_receive(
        &self,
        vm_id: String,
        name: String,
        spec_json: Vec<u8>,
    ) -> Result<String, AgentError> {
        let mut client = self.client().await?;
        let resp = client
            .prepare_receive(PrepareReceiveRequest {
                vm_id,
                name,
                spec_json,
            })
            .await?
            .into_inner();
        Ok(resp.migration_url)
    }

    /// Source: send the VM's live state to `destination_url`.
    pub async fn send_migration(
        &self,
        vm_id: String,
        destination_url: String,
    ) -> Result<(), AgentError> {
        let mut client = self.client().await?;
        client
            .send_migration(SendMigrationRequest {
                vm_id,
                destination_url,
            })
            .await?;
        Ok(())
    }

    /// Destination: finalize a received migration.
    pub async fn finalize_receive(&self, vm_id: String) -> Result<VmObservedState, AgentError> {
        let mut client = self.client().await?;
        let resp = client
            .finalize_receive(FinalizeReceiveRequest { vm_id })
            .await?
            .into_inner();
        Ok(resp.state.unwrap_or_default())
    }

    /// Source: discard a VM whose state has migrated away.
    pub async fn discard_vm(&self, vm_id: String) -> Result<(), AgentError> {
        let mut client = self.client().await?;
        client.discard_vm(DiscardVmRequest { vm_id }).await?;
        Ok(())
    }
}
