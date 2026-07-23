//! Control-plane client for the host-agent gRPC API (design document, section
//! 12). One [`Agent`] targets one host's endpoint.
//!
//! A fresh channel is opened per call: agent traffic is low-frequency and this
//! keeps the control plane free of per-host connection state for now.

use ch_proto::agent::host_agent_client::HostAgentClient;
use ch_proto::agent::{
    DeleteVmRequest, EnsureVmRequest, GetHostInfoRequest, GetHostInfoResponse, VmObservedState,
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

    /// Reconcile a VM towards `spec_json` (the JSON-encoded orchestration spec).
    pub async fn ensure_vm(
        &self,
        vm_id: String,
        name: String,
        spec_json: Vec<u8>,
    ) -> Result<VmObservedState, AgentError> {
        let mut client = self.client().await?;
        let resp = client
            .ensure_vm(EnsureVmRequest {
                vm_id,
                name,
                spec_json,
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
}
