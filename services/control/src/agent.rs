//! Control-plane client for the host-agent gRPC API (design document, section
//! 12). One [`Agent`] targets one host's endpoint.
//!
//! A fresh channel is opened per call: agent traffic is low-frequency and this
//! keeps the control plane free of per-host connection state for now.

use ch_proto::agent::host_agent_client::HostAgentClient;
use ch_proto::agent::{
    DeleteVmRequest, DiscardVmRequest, EnsureVmRequest, FinalizeReceiveRequest, GetHostInfoRequest,
    GetHostInfoResponse, ListVmsRequest, NetworkBinding, PrepareReceiveRequest,
    SendMigrationRequest, VmObservedState,
};
use tonic::transport::Channel;

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

    async fn client(&self) -> Result<HostAgentClient<Channel>, AgentError> {
        HostAgentClient::connect(self.endpoint.clone())
            .await
            .map_err(|source| AgentError::Connect {
                endpoint: self.endpoint.clone(),
                source,
            })
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

    /// Reconcile a VM towards `spec_json` (the JSON-encoded orchestration spec)
    /// with resolved per-NIC dataplane bindings.
    pub async fn ensure_vm(
        &self,
        vm_id: String,
        name: String,
        spec_json: Vec<u8>,
        networks: Vec<NetworkBinding>,
    ) -> Result<VmObservedState, AgentError> {
        let mut client = self.client().await?;
        let resp = client
            .ensure_vm(EnsureVmRequest {
                vm_id,
                name,
                spec_json,
                networks,
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
