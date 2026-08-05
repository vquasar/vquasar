//! End-to-end integration test (design M17, Quality & delivery).
//!
//! This runs the *real* `vquasar-control` binary against a throwaway PostgreSQL
//! database, with the test itself standing in as the host agent: a small
//! in-process tonic `HostAgent` server with in-memory VM state. So it exercises
//! the real REST API → reconcile loop → gRPC-to-agent path end to end, with no
//! Cloud Hypervisor and no lab hardware — CI-friendly.
//!
//! Requires a reachable PostgreSQL. Set `E2E_PG_ADMIN_URL` (a URL to a database
//! the test can `CREATE DATABASE` from, e.g. `postgres://ch:ch@127.0.0.1:5432/postgres`);
//! defaults to that. The test creates a uniquely-named database per run and
//! drops it on teardown. Auth is disabled (dev superuser), so no OIDC is needed.

use std::collections::HashMap;
use std::pin::Pin;
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tokio_stream::Stream;
use tonic::transport::Server;
use tonic::{Request, Response, Status};

use vquasar_proto::agent::host_agent_server::{HostAgent, HostAgentServer};
use vquasar_proto::agent::vm_observed_state::Phase;
use vquasar_proto::agent::{
    ConsoleClientMessage, ConsoleServerMessage, DeleteVmRequest, DiscardVmRequest, EnsureVmRequest,
    EnsureVmResponse, FinalizeReceiveRequest, GetHostInfoRequest, GetHostInfoResponse,
    GetVmMetricsRequest, GetVmRequest, GetVmResponse, ListVmsRequest, ListVmsResponse,
    OperationResponse, PrepareReceiveRequest, PrepareReceiveResponse, SendMigrationRequest,
    StartVmRequest, StopVmRequest, VmMetricsResponse, VmObservedState,
};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn admin_url() -> String {
    std::env::var("E2E_PG_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://ch:ch@127.0.0.1:5432/postgres".to_string())
}

/// Grab a currently-free localhost port (small TOCTOU window, fine for tests).
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

// ---------------------------------------------------------------------------
// Fake host agent: in-memory VM lifecycle over the real gRPC contract.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct AgentState {
    /// vm_id -> observed phase.
    vms: HashMap<String, Phase>,
    /// vm_ids a migration receiver is expecting (prepare_receive → finalize).
    pending_receive: HashMap<String, ()>,
}

struct FakeAgent {
    host_id: String,
    state: Arc<Mutex<AgentState>>,
}

impl FakeAgent {
    fn observed(id: &str, phase: Phase) -> VmObservedState {
        VmObservedState {
            vm_id: id.to_string(),
            phase: phase as i32,
            message: String::new(),
            ip_address: String::new(),
        }
    }
}

#[tonic::async_trait]
impl HostAgent for FakeAgent {
    async fn get_host_info(
        &self,
        _r: Request<GetHostInfoRequest>,
    ) -> Result<Response<GetHostInfoResponse>, Status> {
        let vm_count = self.state.lock().unwrap().vms.len() as u32;
        Ok(Response::new(GetHostInfoResponse {
            host_id: self.host_id.clone(),
            hostname: self.host_id.clone(),
            architecture: "x86_64".into(),
            kernel_version: "test".into(),
            cloud_hypervisor_version: "fake".into(),
            logical_cpus: 8,
            cpu_model: "Fake CPU".into(),
            cpu_vendor: "GenuineIntel".into(),
            cpu_features: vec!["sse2".into(), "avx".into(), "avx2".into()],
            total_memory_bytes: 16 * 1024 * 1024 * 1024,
            available_memory_bytes: 16 * 1024 * 1024 * 1024,
            vm_count,
        }))
    }

    async fn get_vm(&self, r: Request<GetVmRequest>) -> Result<Response<GetVmResponse>, Status> {
        let id = r.into_inner().vm_id;
        let st = self.state.lock().unwrap();
        let phase = st.vms.get(&id).copied().unwrap_or(Phase::Stopped);
        Ok(Response::new(GetVmResponse {
            state: Some(Self::observed(&id, phase)),
        }))
    }

    async fn get_vm_metrics(
        &self,
        r: Request<GetVmMetricsRequest>,
    ) -> Result<Response<VmMetricsResponse>, Status> {
        let id = r.into_inner().vm_id;
        let running = matches!(
            self.state.lock().unwrap().vms.get(&id),
            Some(Phase::Running)
        );
        Ok(Response::new(VmMetricsResponse {
            running,
            cpu_pct: if running { 1.0 } else { 0.0 },
            mem_bytes: if running { 128 * 1024 * 1024 } else { 0 },
            ..Default::default()
        }))
    }

    async fn list_vms(
        &self,
        _r: Request<ListVmsRequest>,
    ) -> Result<Response<ListVmsResponse>, Status> {
        let st = self.state.lock().unwrap();
        let vms = st
            .vms
            .iter()
            .map(|(id, p)| Self::observed(id, *p))
            .collect();
        Ok(Response::new(ListVmsResponse { vms }))
    }

    async fn ensure_vm(
        &self,
        r: Request<EnsureVmRequest>,
    ) -> Result<Response<EnsureVmResponse>, Status> {
        let req = r.into_inner();
        // Mirror the real agent: reconcile to the spec's desired power state.
        let stopped = serde_json::from_slice::<Value>(&req.spec_json)
            .ok()
            .and_then(|s| {
                s.get("desired_power_state")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .as_deref()
            == Some("Stopped");
        let phase = if stopped {
            Phase::Stopped
        } else {
            Phase::Running
        };
        self.state
            .lock()
            .unwrap()
            .vms
            .insert(req.vm_id.clone(), phase);
        Ok(Response::new(EnsureVmResponse {
            state: Some(Self::observed(&req.vm_id, phase)),
        }))
    }

    async fn start_vm(
        &self,
        r: Request<StartVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = r.into_inner().vm_id;
        self.state.lock().unwrap().vms.insert(id, Phase::Running);
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "started".into(),
        }))
    }

    async fn stop_vm(
        &self,
        r: Request<StopVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = r.into_inner().vm_id;
        self.state.lock().unwrap().vms.insert(id, Phase::Stopped);
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "stopped".into(),
        }))
    }

    async fn delete_vm(
        &self,
        r: Request<DeleteVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = r.into_inner().vm_id;
        self.state.lock().unwrap().vms.remove(&id);
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "deleted".into(),
        }))
    }

    type VmConsoleStream = Pin<Box<dyn Stream<Item = Result<ConsoleServerMessage, Status>> + Send>>;
    async fn vm_console(
        &self,
        _r: Request<tonic::Streaming<ConsoleClientMessage>>,
    ) -> Result<Response<Self::VmConsoleStream>, Status> {
        Err(Status::unimplemented("console not supported in e2e fake"))
    }

    async fn prepare_receive(
        &self,
        r: Request<PrepareReceiveRequest>,
    ) -> Result<Response<PrepareReceiveResponse>, Status> {
        let id = r.into_inner().vm_id;
        self.state.lock().unwrap().pending_receive.insert(id, ());
        Ok(Response::new(PrepareReceiveResponse {
            migration_url: "tcp:127.0.0.1:0".into(),
        }))
    }

    async fn send_migration(
        &self,
        _r: Request<SendMigrationRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "sent".into(),
        }))
    }

    async fn finalize_receive(
        &self,
        r: Request<FinalizeReceiveRequest>,
    ) -> Result<Response<EnsureVmResponse>, Status> {
        let id = r.into_inner().vm_id;
        let mut st = self.state.lock().unwrap();
        st.pending_receive.remove(&id);
        st.vms.insert(id.clone(), Phase::Running);
        Ok(Response::new(EnsureVmResponse {
            state: Some(Self::observed(&id, Phase::Running)),
        }))
    }

    async fn discard_vm(
        &self,
        r: Request<DiscardVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = r.into_inner().vm_id;
        self.state.lock().unwrap().vms.remove(&id);
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "discarded".into(),
        }))
    }
}

/// Start a fake agent on `port`; returns its shared state (for assertions).
fn spawn_agent(host_id: &str, port: u16) -> Arc<Mutex<AgentState>> {
    let state = Arc::new(Mutex::new(AgentState::default()));
    let agent = FakeAgent {
        host_id: host_id.to_string(),
        state: state.clone(),
    };
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    tokio::spawn(async move {
        Server::builder()
            .add_service(HostAgentServer::new(agent))
            .serve(addr)
            .await
            .unwrap();
    });
    state
}

// ---------------------------------------------------------------------------
// Harness: throwaway DB + spawned control binary.
// ---------------------------------------------------------------------------

struct Harness {
    control: Child,
    base: String,
    client: reqwest::Client,
    db_name: String,
}

impl Harness {
    async fn start() -> Self {
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        let db_name = format!("vquasar_e2e_{}_{}", std::process::id(), seq);

        // Create a throwaway database.
        let admin = admin_url();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&admin)
            .await
            .expect("connect to admin DB (set E2E_PG_ADMIN_URL)");
        sqlx::query(&format!("CREATE DATABASE {db_name}"))
            .execute(&pool)
            .await
            .expect("create test database");
        pool.close().await;

        // Point control at the new DB. Derive its URL from the admin URL.
        let db_url = format!("{}/{}", admin.rsplit_once('/').unwrap().0, db_name);
        let port = free_port();
        let base = format!("http://127.0.0.1:{port}");

        let control_bin = env!("CARGO_BIN_EXE_vquasar-control");
        let control = Command::new(control_bin)
            .env("VQUASAR_CONTROL_DATABASE__URL", &db_url)
            .env(
                "VQUASAR_CONTROL_SERVER__LISTEN",
                format!("127.0.0.1:{port}"),
            )
            .env("VQUASAR_CONTROL_AUTH__DISABLED", "true")
            .env("VQUASAR_CONTROL_RECONCILE__INTERVAL_SECS", "1")
            .env("RUST_LOG", "warn")
            .spawn()
            .expect("spawn vquasar-control");

        let h = Harness {
            control,
            base,
            client: reqwest::Client::new(),
            db_name,
        };
        h.wait_healthy().await;
        h
    }

    async fn wait_healthy(&self) {
        for _ in 0..100 {
            if let Ok(r) = self
                .client
                .get(format!("{}/healthz", self.base))
                .send()
                .await
            {
                if r.status().is_success() {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("control did not become healthy");
    }

    async fn get(&self, path: &str) -> Value {
        let r = self
            .client
            .get(format!("{}/api/v1{path}", self.base))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "GET {path} -> {}", r.status());
        r.json().await.unwrap()
    }

    async fn post(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let r = self
            .client
            .post(format!("{}/api/v1{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status();
        let v = r.json::<Value>().await.unwrap_or(Value::Null);
        (status, v)
    }

    #[allow(dead_code)]
    async fn patch(&self, path: &str, body: Value) -> reqwest::StatusCode {
        self.client
            .patch(format!("{}/api/v1{path}", self.base))
            .json(&body)
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn delete(&self, path: &str) -> reqwest::StatusCode {
        self.client
            .delete(format!("{}/api/v1{path}", self.base))
            .send()
            .await
            .unwrap()
            .status()
    }

    /// Register a fake agent as a host and wait until control marks it Ready.
    async fn register_host(&self, name: &str, port: u16) -> String {
        let (st, v) = self
            .post(
                "/hosts",
                json!({"name": name, "endpoint": format!("http://127.0.0.1:{port}")}),
            )
            .await;
        assert!(st.is_success(), "register host: {st} {v}");
        let id = v["id"].as_str().unwrap().to_string();
        self.wait_for(
            &format!("/hosts/{id}"),
            |h| h["state"] == "Ready",
            "host Ready",
        )
        .await;
        id
    }

    /// Poll a GET endpoint until `pred` holds, or panic after a timeout.
    async fn wait_for(&self, path: &str, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
        for _ in 0..60 {
            let v = self.get(path).await;
            if pred(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("timed out waiting for: {what} (at {path})");
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = self.control.kill();
        let _ = self.control.wait();
        // Drop the throwaway database (best-effort, blocking).
        let admin = admin_url();
        let db = self.db_name.clone();
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                if let Ok(pool) = sqlx::postgres::PgPoolOptions::new()
                    .max_connections(1)
                    .connect(&admin)
                    .await
                {
                    let _ = sqlx::query(&format!(
                        "SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname='{db}'"
                    ))
                    .execute(&pool)
                    .await;
                    let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {db}"))
                        .execute(&pool)
                        .await;
                }
            });
        })
        .join();
    }
}

/// A minimal valid VM spec (direct-kernel, no disks/nics — paths are never
/// touched because the agent is fake).
fn vm_spec() -> Value {
    json!({
        "desired_power_state": "Running",
        "cpu": {"boot_vcpus": 1, "max_vcpus": 1},
        "memory": {"size_mib": 512},
        "boot": {"type": "direct_kernel", "kernel": "/x/vmlinuz"},
        "disks": [],
        "network_interfaces": [],
        "placement": {}
    })
}

#[tokio::test]
async fn vm_lifecycle_end_to_end() {
    let h = Harness::start().await;
    let a_port = free_port();
    let a_state = spawn_agent("hostA", a_port);
    let host = h.register_host("hostA", a_port).await;

    // Create a VM → scheduled onto the host → observed Running.
    let (st, v) = h
        .post("/vms", json!({"name": "e2e-vm", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "create vm: {st} {v}");
    let vm_id = v["vm_id"].as_str().unwrap().to_string();

    let vm = h
        .wait_for(
            &format!("/vms/{vm_id}"),
            |v| v["phase"] == "Running",
            "VM Running",
        )
        .await;
    assert_eq!(
        vm["host_id"].as_str().unwrap(),
        host,
        "scheduled onto hostA"
    );
    assert!(
        a_state.lock().unwrap().vms.contains_key(&vm_id),
        "agent has the VM"
    );

    // /metrics reflects one Running VM.
    let metrics = reqwest::get(format!("{}/metrics", h.base))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(
        metrics.contains("vquasar_vms{phase=\"Running\"} 1"),
        "metrics should show 1 running VM"
    );

    // Stop → Stopped, Start → Running.
    let (st, _) = h.post(&format!("/vms/{vm_id}/stop"), json!({})).await;
    assert!(st.is_success());
    h.wait_for(
        &format!("/vms/{vm_id}"),
        |v| v["phase"] == "Stopped",
        "VM Stopped",
    )
    .await;
    let (st, _) = h.post(&format!("/vms/{vm_id}/start"), json!({})).await;
    assert!(st.is_success());
    h.wait_for(
        &format!("/vms/{vm_id}"),
        |v| v["phase"] == "Running",
        "VM Running again",
    )
    .await;

    // Delete → gone from the API and from the agent.
    assert!(h.delete(&format!("/vms/{vm_id}")).await.is_success());
    for _ in 0..60 {
        let list = h.get("/vms").await;
        if list.as_array().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        h.get("/vms").await.as_array().unwrap().is_empty(),
        "VM deleted"
    );
}

#[tokio::test]
async fn scheduling_migration_and_drain() {
    let h = Harness::start().await;
    let (pa, pb) = (free_port(), free_port());
    let sa = spawn_agent("hostA", pa);
    let sb = spawn_agent("hostB", pb);
    let a = h.register_host("hostA", pa).await;
    let b = h.register_host("hostB", pb).await;

    // Cordon B so the new VM is forced onto A (deterministic placement).
    assert!(h
        .patch(&format!("/hosts/{b}"), json!({"schedulable": false}))
        .await
        .is_success());
    let (st, v) = h
        .post("/vms", json!({"name": "mig-vm", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "create vm: {st} {v}");
    let vm = v["vm_id"].as_str().unwrap().to_string();
    let cur = h
        .wait_for(
            &format!("/vms/{vm}"),
            |v| v["phase"] == "Running",
            "VM Running",
        )
        .await;
    assert_eq!(
        cur["host_id"].as_str().unwrap(),
        a,
        "cordon forced placement onto A"
    );
    assert!(sa.lock().unwrap().vms.contains_key(&vm) && !sb.lock().unwrap().vms.contains_key(&vm));

    // Uncordon B, then live-migrate A → B (identical CPUs ⇒ compatible).
    assert!(h
        .patch(&format!("/hosts/{b}"), json!({"schedulable": true}))
        .await
        .is_success());
    let (st, v) = h
        .post(&format!("/vms/{vm}/migrate"), json!({"target_host_id": b}))
        .await;
    assert!(st.is_success(), "migrate: {st} {v}");
    h.wait_for(
        &format!("/vms/{vm}"),
        |v| v["phase"] == "Running" && v["host_id"] == json!(b),
        "VM migrated to B",
    )
    .await;
    assert!(
        sb.lock().unwrap().vms.contains_key(&vm) && !sa.lock().unwrap().vms.contains_key(&vm),
        "agent state moved A → B",
    );

    // Drain B (where the VM now runs) → it evacuates back to A.
    let (st, drain) = h.post(&format!("/hosts/{b}/drain"), json!({})).await;
    assert!(st.is_success(), "drain: {st} {drain}");
    assert_eq!(
        drain["migrating"].as_array().unwrap().len(),
        1,
        "drain migrates the VM"
    );
    assert!(
        drain["cordoned"].as_bool().unwrap(),
        "drain cordons the host"
    );
    h.wait_for(
        &format!("/vms/{vm}"),
        |v| v["phase"] == "Running" && v["host_id"] == json!(a),
        "VM drained back to A",
    )
    .await;
    // B is left cordoned by the drain.
    assert_eq!(
        h.get(&format!("/hosts/{b}")).await["schedulable"],
        json!(false)
    );
}
