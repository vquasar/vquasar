//! Generated gRPC types for the host-agent protocol (`proto/agent.proto`).
//!
//! The agent implements [`agent::host_agent_server::HostAgent`]; the control
//! plane (and tests) use [`agent::host_agent_client::HostAgentClient`]. This
//! API is private to the agent/control-plane channel and must never be exposed
//! publicly (design document, section 12).

pub mod agent {
    tonic::include_proto!("vquasar.agent.v1");
}
