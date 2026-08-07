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
            overlay_vnis: vec![],
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
    db_url: String,
}

impl Harness {
    async fn start() -> Self {
        Self::start_with(&[]).await
    }

    async fn start_with(extra_env: &[(&str, &str)]) -> Self {
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
            // The harness database is a throwaway with no TLS. Encryption is
            // the default now, so opting out has to be explicit — which is the
            // point: it is visible here rather than assumed.
            .env("VQUASAR_CONTROL_DATABASE__SSL_MODE", "disable")
            // The harness's specs use synthetic paths, so declare the root they
            // live under; a real install keeps the default (/var/lib/vquasar).
            .env("VQUASAR_CONTROL_STORAGE__ALLOWED_PATHS", "[\"/x\"]")
            .env("VQUASAR_CONTROL_RECONCILE__INTERVAL_SECS", "1")
            .env("RUST_LOG", "warn")
            .envs(extra_env.iter().copied())
            .spawn()
            .expect("spawn vquasar-control");

        let h = Harness {
            control,
            base,
            client: reqwest::Client::new(),
            db_name,
            db_url: db_url.clone(),
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

    /// Status code from a WebSocket upgrade attempt (the handshake is enough:
    /// we are testing what happens *before* the upgrade).
    async fn ws_status(&self, path: &str) -> u16 {
        reqwest::Client::new()
            .get(format!("{}/api/v1{path}", self.base))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .send()
            .await
            .expect("console request")
            .status()
            .as_u16()
    }

    /// Run a statement against the harness database, for setting up states the
    /// API deliberately will not create (here: a pre-kind-model network).
    async fn sql(&self, stmt: &str) {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.db_url)
            .await
            .expect("connect to the harness DB");
        sqlx::query(stmt).execute(&pool).await.expect("statement");
        pool.close().await;
    }

    /// A single text value from the harness database.
    async fn query_one(&self, sql: &str) -> String {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect(&self.db_url)
            .await
            .expect("connect to the harness DB");
        let v: Option<String> = sqlx::query_scalar(sql)
            .fetch_one(&pool)
            .await
            .expect("query");
        pool.close().await;
        v.unwrap_or_default()
    }

    /// DELETE, returning the status and body — some deletions are refusals
    /// whose message is the point.
    async fn delete_status(&self, path: &str) -> (u16, Value) {
        let r = self
            .client
            .delete(format!("{}/api/v1{path}", self.base))
            .send()
            .await
            .expect("delete");
        let st = r.status().as_u16();
        let body = r.json::<Value>().await.unwrap_or(Value::Null);
        (st, body)
    }

    /// GET within a project's scope.
    async fn get_in(&self, project: &str, path: &str) -> Value {
        self.client
            .get(format!("{}/api/v1{path}", self.base))
            .header("X-Vquasar-Project", project)
            .send()
            .await
            .expect("get")
            .json()
            .await
            .unwrap_or(Value::Null)
    }

    async fn get_status_in(&self, project: &str, path: &str) -> (u16, Value) {
        let r = self
            .client
            .get(format!("{}/api/v1{path}", self.base))
            .header("X-Vquasar-Project", project)
            .send()
            .await
            .expect("get");
        let st = r.status().as_u16();
        (st, r.json().await.unwrap_or(Value::Null))
    }

    /// POST within a project's scope.
    async fn post_in(
        &self,
        project: &str,
        path: &str,
        body: Value,
    ) -> (reqwest::StatusCode, Value) {
        let r = self
            .client
            .post(format!("{}/api/v1{path}", self.base))
            .header("X-Vquasar-Project", project)
            .json(&body)
            .send()
            .await
            .expect("post");
        let st = r.status();
        (st, r.json().await.unwrap_or(Value::Null))
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
    /// DELETE within a project's scope.
    async fn delete_in(&self, project: &str, path: &str) -> u16 {
        self.client
            .delete(format!("{}/api/v1{path}", self.base))
            .header("X-Vquasar-Project", project)
            .send()
            .await
            .expect("delete")
            .status()
            .as_u16()
    }

    /// PATCH within a project's scope.
    async fn patch_in(&self, project: &str, path: &str, body: Value) -> u16 {
        self.client
            .patch(format!("{}/api/v1{path}", self.base))
            .header("X-Vquasar-Project", project)
            .json(&body)
            .send()
            .await
            .expect("patch")
            .status()
            .as_u16()
    }

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

    // /metrics reflects one Running VM. The gauge is refreshed on a reconcile
    // tick (separate from the VM's phase transition), so poll for it.
    let mut metrics_ok = false;
    for _ in 0..40 {
        let m = reqwest::get(format!("{}/metrics", h.base))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        if m.contains("vquasar_vms{phase=\"Running\"} 1") {
            metrics_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(metrics_ok, "metrics should show 1 running VM");

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

/// Two API-level security invariants, end to end against the real binary.
///
/// Both were live defects: `vm:read` returned decrypted cloud-init secrets, and
/// a VM spec could name any host path — including the agent's own key — which
/// the agent would then open with privilege (design §30).
#[tokio::test]
async fn secrets_are_not_returned_and_host_paths_are_confined() {
    let h = Harness::start().await;

    // --- cloud-init secrets never reach a caller -------------------------
    let mut spec = vm_spec();
    spec["cloud_init"] = json!({
        "password": "hunter2",
        "user_data": "#cloud-config\nruncmd:\n - echo TOP-SECRET",
        "ssh_authorized_keys": ["ssh-ed25519 AAAASECRETKEY"],
    });
    let (st, created) = h
        .post("/vms", json!({"name": "secrets", "spec": spec}))
        .await;
    assert!(st.is_success(), "create vm: {st} {created}");
    let vm = created["vm_id"].as_str().unwrap().to_string();

    for path in [format!("/vms/{vm}"), "/vms".to_string()] {
        let body = h.get(&path).await.to_string();
        assert!(
            !body.contains("hunter2"),
            "password leaked via {path}: {body}"
        );
        assert!(
            !body.contains("TOP-SECRET"),
            "user-data leaked via {path}: {body}"
        );
        assert!(
            !body.contains("AAAASECRETKEY"),
            "ssh key leaked via {path}: {body}"
        );
    }

    // --- host paths stay inside the permitted roots ----------------------
    // The harness permits "/x"; the agent's key material is not under it.
    let mut escaping = vm_spec();
    escaping["boot"] = json!({"type": "direct_kernel", "kernel": "/x/vmlinuz"});
    escaping["disks"] = json!([{"path": "/etc/vquasar/tls/agent.key"}]);
    let (st, body) = h
        .post("/vms", json!({"name": "escape", "spec": escaping}))
        .await;
    assert_eq!(
        st.as_u16(),
        400,
        "reading the agent key must be refused: {body}"
    );

    let mut traversal = vm_spec();
    traversal["boot"] = json!({"type": "direct_kernel", "kernel": "/x/../etc/shadow"});
    let (st, body) = h
        .post("/vms", json!({"name": "traversal", "spec": traversal}))
        .await;
    assert_eq!(st.as_u16(), 400, "traversal must be refused: {body}");
}

/// The network type model (ADR-016): a network declares what it isolates, its
/// segment is platform-allocated, and one network is one L2 domain.
#[tokio::test]
async fn network_kinds_and_platform_allocated_segments() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_NETWORK__PROVIDER_VLANS", "100-200")]).await;

    // A caller cannot pick the segment they land on — that is how you join
    // somebody else's overlay, or reach a provider VLAN you were not given.
    let (st, body) = h
        .post(
            "/networks",
            json!({"name": "pick-vni", "kind": "tenant", "vni": 4096}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
    let (st, body) = h
        .post(
            "/networks",
            json!({"name": "pick-vlan", "kind": "vlan", "vlan": 999}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "vlan outside the allowlist: {body}");

    // Tenant networks get distinct VNIs from the platform.
    let (st, a) = h
        .post("/networks", json!({"name": "t-a", "kind": "tenant"}))
        .await;
    assert!(st.is_success(), "{a}");
    let (_, b) = h
        .post("/networks", json!({"name": "t-b", "kind": "tenant"}))
        .await;
    assert_ne!(a["vni"], b["vni"], "two tenant networks share a VNI");
    assert_eq!(
        a["segment_key"],
        json!(format!("vxlan:{}", a["vni"].as_i64().unwrap()))
    );

    // One network = one L2 domain: the same uplink+tag cannot be claimed twice.
    let (st, _) = h
        .post(
            "/networks",
            json!({"name": "v1", "kind": "vlan", "vlan": 150}),
        )
        .await;
    assert!(st.is_success());
    let (st, body) = h
        .post(
            "/networks",
            json!({"name": "v2", "kind": "vlan", "vlan": 150}),
        )
        .await;
    assert_eq!(
        st.as_u16(),
        400,
        "duplicate segment must be refused: {body}"
    );

    // Every network carries a policy object from creation (ADR-017).
    assert!(
        a["default_security_group_id"].is_string(),
        "tenant network has no default policy group: {a}"
    );

    // A network's segment is its identity and cannot be swapped underneath the
    // VMs attached to it.
    let id = a["id"].as_str().unwrap();
    let (st, body) = h
        .post(
            &format!("/networks/{id}"),
            json!({"name": "t-a", "vlan": 150}),
        )
        .await;
    assert!(st.as_u16() == 400 || st.as_u16() == 405, "{body}");
}

/// An unknown path under `/api/v1` must answer with the error envelope, not the
/// single-page shell. The control plane serves the console from the same origin
/// and falls back to `index.html` so deep links work; without a router fallback
/// on the API that fallback would swallow a typo'd endpoint and hand a client
/// an HTML page with status 200 — the worst possible answer.
#[tokio::test]
async fn unknown_api_route_returns_the_error_envelope() {
    let h = Harness::start().await;

    let r = h
        .client
        .get(format!("{}/api/v1/no-such-endpoint", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::NOT_FOUND);
    let ct = r
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(ct.starts_with("application/json"), "content-type was {ct}");

    let body: Value = r.json().await.unwrap();
    assert!(
        body["error"]["code"].is_string() && body["error"]["request_id"].is_string(),
        "not the standard envelope: {body}"
    );
}

/// Serving the console is part of the product, not a convenience: the control
/// plane is the only origin an operator's browser talks to. Three things have
/// to hold at once — a real file wins over the SPA shell (favicons and brand
/// marks live at the root, not under /assets), an unmatched path falls back to
/// the shell so deep links like /hosts/:id work, and every response carries the
/// baseline security headers.
#[tokio::test]
async fn serves_the_console_with_deep_links_and_security_headers() {
    let ui = tempfile::tempdir().unwrap();
    std::fs::create_dir(ui.path().join("assets")).unwrap();
    std::fs::write(
        ui.path().join("index.html"),
        "<!doctype html><title>vQuasar</title>",
    )
    .unwrap();
    std::fs::write(ui.path().join("assets/app.js"), "export default 1;").unwrap();
    std::fs::write(ui.path().join("favicon.svg"), "<svg/>").unwrap();

    let h = Harness::start_with(&[(
        "VQUASAR_CONTROL_SERVER__UI_DIR",
        ui.path().to_str().unwrap(),
    )])
    .await;

    // A real file at the root is served as itself. Before the SPA fallback
    // covered the whole directory this returned index.html, so every favicon
    // and brand asset silently came back as an HTML page.
    let r = h
        .client
        .get(format!("{}/favicon.svg", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    assert!(
        r.headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("image/svg"),
        "favicon.svg was not served as an image"
    );
    // Security headers ride on everything this server hands out.
    let csp = r
        .headers()
        .get("content-security-policy")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(csp.contains("default-src 'self'"), "CSP was {csp:?}");
    assert!(csp.contains("frame-ancestors 'none'"), "CSP was {csp:?}");
    assert_eq!(
        r.headers()
            .get("x-content-type-options")
            .and_then(|v| v.to_str().ok()),
        Some("nosniff")
    );
    assert!(r.text().await.unwrap().contains("<svg"));

    // A hashed bundle under /assets still resolves.
    let r = h
        .client
        .get(format!("{}/assets/app.js", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK);

    // A client-side route matches no file and gets the shell, so a bookmarked
    // deep link loads the app instead of 404ing.
    let r = h
        .client
        .get(format!("{}/hosts/some-uuid", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::OK);
    assert!(r.text().await.unwrap().contains("vQuasar"));

    // The API keeps its own 404 — the SPA fallback must not swallow it.
    let r = h
        .client
        .get(format!("{}/api/v1/no-such-endpoint", h.base))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), reqwest::StatusCode::NOT_FOUND);
    assert!(r.json::<Value>().await.unwrap()["error"]["code"].is_string());
}

/// The console WebSocket authorized the *permission* and then opened a session
/// against whatever id was in the path, without ever resolving it. An unknown
/// (or, once projects exist, someone else's) VM must not reach the upgrade.
#[tokio::test]
async fn console_rejects_an_unknown_vm_before_upgrading() {
    let h = Harness::start().await;
    let unknown = uuid::Uuid::new_v4();
    let code = h.ws_status(&format!("/vms/{unknown}/console")).await;
    assert_eq!(code, 404, "an unknown VM must not open a console session");

    // A real VM still resolves (auth is disabled in the harness, so this
    // isolates the ownership/resolution check from the permission check).
    let (st, created) = h
        .post("/vms", json!({"name": "conso", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{created}");
    let vm = created["vm_id"].as_str().unwrap();
    let code = h.ws_status(&format!("/vms/{vm}/console")).await;
    assert_ne!(code, 404, "a real VM must get past the resolution check");
}

/// A grandfathered network can be given a real segment identity, but only once
/// and only if nothing else already occupies it (ADR-016).
#[tokio::test]
async fn a_legacy_network_can_adopt_a_segment_exactly_once() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_NETWORK__PROVIDER_VLANS", "100-200")]).await;

    // Two networks standing in for the grandfathered pair: no segment key, so
    // nothing says they are not the same broadcast domain.
    // One at a time: the second could not even be created while the first holds
    // the untagged segment — which is the constraint working. Clearing each
    // segment right after reproduces the state migration 0016 leaves behind.
    let mut ids = Vec::new();
    for name in ["legacy-a", "legacy-b"] {
        let (st, n) = h
            .post("/networks", json!({"name": name, "kind": "provider"}))
            .await;
        assert!(st.is_success(), "{n}");
        let id = n["id"].as_str().unwrap().to_string();
        h.sql(&format!(
            "UPDATE networks SET segment_key=NULL, legacy_segment=true WHERE id='{id}'"
        ))
        .await;
        ids.push(id);
    }

    // The first adopts the untagged segment it already occupies.
    let (st, a) = h
        .post(&format!("/networks/{}/adopt-segment", ids[0]), json!({}))
        .await;
    assert!(st.is_success(), "first adoption should succeed: {a}");
    assert_eq!(a["segment_key"], json!("default:untagged"));
    assert_eq!(a["legacy_segment"], json!(false));

    // The second cannot: it is the *same* broadcast domain.
    let (st, b) = h
        .post(&format!("/networks/{}/adopt-segment", ids[1]), json!({}))
        .await;
    assert_eq!(st.as_u16(), 400, "a duplicate segment must be refused: {b}");
    assert!(
        b["error"]["message"].as_str().unwrap().contains("same"),
        "the error should explain why: {b}"
    );

    // ...but it can take a distinct tag, once the caller confirms the re-tag.
    let (st, b) = h
        .post(
            &format!("/networks/{}/adopt-segment", ids[1]),
            json!({"vlan": 150}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "re-tagging needs confirmation: {b}");
    let (st, b) = h
        .post(
            &format!("/networks/{}/adopt-segment", ids[1]),
            json!({"vlan": 150, "retag_ok": true}),
        )
        .await;
    assert!(st.is_success(), "{b}");
    assert_eq!(b["segment_key"], json!("default:150"));

    // Adoption is one-way: a network with a segment may never change it.
    let (st, again) = h
        .post(&format!("/networks/{}/adopt-segment", ids[0]), json!({}))
        .await;
    assert_eq!(st.as_u16(), 400, "{again}");
    assert!(again["error"]["message"]
        .as_str()
        .unwrap()
        .contains("cannot be changed"));
}

/// phone_home was unauthenticated: anyone who could reach the API and knew a VM
/// id could set that VM's recorded address. VM ids are not secret — the task and
/// event streams hand them out (design M13e).
#[tokio::test]
async fn phone_home_requires_the_guests_own_token() {
    let h = Harness::start().await;
    let (st, created) = h
        .post("/vms", json!({"name": "ph", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{created}");
    let vm = created["vm_id"].as_str().unwrap().to_string();

    // The endpoint always answers 204: the response must not tell a caller
    // whether the VM exists or whether the token was right. The observable
    // difference is whether the address was actually recorded.
    let (st, _) = h.post(&format!("/phone-home/{vm}"), json!({})).await;
    assert_eq!(st.as_u16(), 204);
    let (st, _) = h
        .post(&format!("/phone-home/{vm}?token=guessed"), json!({}))
        .await;
    assert_eq!(st.as_u16(), 204);
    assert!(
        h.get(&format!("/vms/{vm}")).await["ip_address"].is_null(),
        "an unauthenticated caller must not be able to set the address"
    );

    // With the VM's own token, the address is recorded.
    let token: String = h
        .query_one(&format!(
            "SELECT phone_home_token FROM virtual_machines WHERE id='{vm}'"
        ))
        .await;
    assert!(!token.is_empty(), "a VM should be issued a token");
    let (st, _) = h
        .post(&format!("/phone-home/{vm}?token={token}"), json!({}))
        .await;
    assert_eq!(st.as_u16(), 204);
    h.wait_for(
        &format!("/vms/{vm}"),
        |v| !v["ip_address"].is_null(),
        "the address is recorded once the guest proves who it is",
    )
    .await;
}

/// Validation enforced only minimums, so a single request could ask for 4096
/// vCPUs or a 64 TiB volume. The VM case is admitted-then-retried-forever; the
/// volume case does the work on shared storage immediately.
#[tokio::test]
async fn absurd_resource_requests_are_refused() {
    let h = Harness::start().await;

    let mut spec = vm_spec();
    spec["cpu"] = json!({"boot_vcpus": 4096, "max_vcpus": 4096});
    let (st, body) = h
        .post("/vms", json!({"name": "huge-cpu", "spec": spec}))
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");

    let mut spec = vm_spec();
    spec["memory"] = json!({"size_mib": 64u64 * 1024 * 1024});
    let (st, body) = h
        .post("/vms", json!({"name": "huge-mem", "spec": spec}))
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");

    // The volume path is the one that consumes shared storage before anything
    // is persisted.
    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "huge", "format": "raw", "size_bytes": 1u64 << 60}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");

    // And an ordinary request is untouched.
    let (st, body) = h
        .post("/vms", json!({"name": "normal", "spec": vm_spec()}))
        .await;
    assert!(
        st.is_success(),
        "a reasonable spec must still be accepted: {body}"
    );
}

/// Projects exist as objects before anything is scoped to them (ADR-018). The
/// invasive part — the schema — lands once; scoping, per-project RBAC and
/// quotas follow separately.
#[tokio::test]
async fn projects_are_created_and_refuse_to_vanish_with_contents() {
    let h = Harness::start().await;

    // Migration 0021 leaves exactly one project, and everything belongs to it.
    let list = h.get("/projects").await;
    let projects = list.as_array().unwrap();
    assert_eq!(projects.len(), 1, "{list}");
    assert_eq!(projects[0]["name"], json!("default"));
    assert_eq!(projects[0]["is_default"], json!(true));

    // Names are identifiers, not free text.
    for bad in ["", "Team Blue", "-leading", "under_score"] {
        let (st, body) = h.post("/projects", json!({"name": bad})).await;
        assert_eq!(st.as_u16(), 400, "{bad:?} should be refused: {body}");
    }

    let (st, p) = h
        .post(
            "/projects",
            json!({"name": "team-blue", "description": "a tenant"}),
        )
        .await;
    assert!(st.is_success(), "{p}");
    let id = p["id"].as_str().unwrap().to_string();
    assert_eq!(p["is_default"], json!(false));

    // Names are unique: a collision is the caller's to see, not a 500.
    let (st, body) = h.post("/projects", json!({"name": "team-blue"})).await;
    assert_eq!(st.as_u16(), 400, "{body}");

    // The default project is the fallback for a caller with no context, so it
    // cannot be removed.
    let default_id = projects[0]["id"].as_str().unwrap();
    let (st, body) = h.delete_status(&format!("/projects/{default_id}")).await;
    assert_eq!(st, 400, "the default project must not be deletable: {body}");

    // An empty project deletes cleanly.
    let (st, _) = h.delete_status(&format!("/projects/{id}")).await;
    assert_eq!(st, 204);
    assert_eq!(h.get("/projects").await.as_array().unwrap().len(), 1);
}

/// With tenancy on, a caller sees its own project and the shared catalogues —
/// and cannot reach another project's resources by naming their ids (ADR-018).
#[tokio::test]
async fn tenancy_scopes_reads_and_refuses_foreign_references() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_TENANCY__ENABLED", "true")]).await;

    let (st, blue) = h.post("/projects", json!({"name": "blue"})).await;
    assert!(st.is_success(), "{blue}");
    let blue = blue["id"].as_str().unwrap().to_string();

    // A VM in the default project.
    let (st, created) = h
        .post("/vms", json!({"name": "in-default", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{created}");
    let vm = created["vm_id"].as_str().unwrap().to_string();

    // The default project sees it...
    assert_eq!(h.get("/vms").await.as_array().unwrap().len(), 1);
    // ...and blue does not, by list or by id. Naming the id gets the same
    // answer an unknown one would: not found, never "forbidden".
    assert_eq!(
        h.get_in(&blue, "/vms").await.as_array().unwrap().len(),
        0,
        "another project's VM must not be listed"
    );
    let (st, _) = h.get_status_in(&blue, &format!("/vms/{vm}")).await;
    assert_eq!(st, 404, "another project's VM must not be readable by id");

    // Reference smuggling: blue creating a VM on the default project's network.
    // Networks created before tenancy are shared (project_id NULL), so make one
    // owned by default to have something genuinely foreign.
    let (st, net) = h
        .post("/networks", json!({"name": "owned", "kind": "tenant"}))
        .await;
    assert!(st.is_success(), "{net}");
    let net = net["id"].as_str().unwrap().to_string();
    assert_eq!(
        h.query_one(&format!(
            "SELECT project_id::text FROM networks WHERE id='{net}'"
        ))
        .await,
        "00000000-0000-0000-0000-000000000001",
        "a tenant network must be stamped with the project that created it"
    );

    let mut spec = vm_spec();
    spec["network_interfaces"] = json!([{"network_id": net}]);
    let (st, body) = h
        .post_in(&blue, "/vms", json!({"name": "smuggle", "spec": spec}))
        .await;
    assert_eq!(
        st.as_u16(),
        404,
        "a create body must not be able to name another project's network: {body}"
    );
}

/// Writes are scoped, and what a write creates is stamped with the caller's
/// project. Reads being scoped is only half of tenancy: if a project can still
/// delete or re-point another project's resources, the isolation is decorative
/// (design §47, ADR-018).
#[tokio::test]
async fn tenancy_scopes_writes_and_stamps_what_it_creates() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_TENANCY__ENABLED", "true")]).await;

    let (st, blue) = h.post("/projects", json!({"name": "blue"})).await;
    assert!(st.is_success(), "{blue}");
    let blue = blue["id"].as_str().unwrap().to_string();
    let default = "00000000-0000-0000-0000-000000000001";

    // ---- what a project creates belongs to that project ------------------
    let (st, sg) = h
        .post_in(&blue, "/security-groups", json!({"name": "web"}))
        .await;
    assert!(st.is_success(), "{sg}");
    let sg = sg["id"].as_str().unwrap().to_string();
    assert_eq!(
        h.query_one(&format!(
            "SELECT project_id::text FROM security_groups WHERE id='{sg}'"
        ))
        .await,
        blue,
        "a security group must be stamped with the project that created it"
    );

    let (st, vm) = h
        .post_in(&blue, "/vms", json!({"name": "blue-vm", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{vm}");
    let vm = vm["vm_id"].as_str().unwrap().to_string();
    // A task inherits the project of the VM it acts on, so a scoped task feed
    // needs no separate bookkeeping.
    assert_eq!(
        h.query_one(&format!(
            "SELECT project_id::text FROM tasks WHERE vm_id='{vm}' LIMIT 1"
        ))
        .await,
        blue,
        "a task must inherit its VM's project"
    );

    // ---- one project cannot write another's resources --------------------
    let (st, other) = h.post("/security-groups", json!({"name": "theirs"})).await;
    assert!(st.is_success(), "{other}");
    let other = other["id"].as_str().unwrap().to_string();

    assert_eq!(
        h.delete_in(&blue, &format!("/security-groups/{other}"))
            .await,
        404,
        "deleting another project's security group must look like a missing id"
    );
    assert_eq!(
        h.patch_in(
            &blue,
            &format!("/security-groups/{other}"),
            json!({"name": "mine-now"})
        )
        .await,
        404,
        "renaming another project's security group must be refused"
    );
    assert_eq!(
        h.post_in(
            &blue,
            &format!("/security-groups/{other}/rules"),
            json!({"direction": "ingress", "ethertype": "IPv4", "protocol": "tcp",
                   "port_min": 22, "port_max": 22}),
        )
        .await
        .0
        .as_u16(),
        404,
        "adding a rule to another project's group would be a policy change on their VMs"
    );
    // The refusal is real, not just a status code.
    assert_eq!(
        h.query_one(&format!(
            "SELECT name FROM security_groups WHERE id='{other}'"
        ))
        .await,
        "theirs"
    );

    assert_eq!(
        h.delete_in(&blue, &format!("/vms/{vm}")).await,
        202,
        "a project deletes its own VM (accepted; the reconcile loop does the work)"
    );

    // ---- power operations are scoped too ---------------------------------
    let (st, theirs) = h
        .post("/vms", json!({"name": "their-vm", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{theirs}");
    let theirs = theirs["vm_id"].as_str().unwrap().to_string();
    assert_eq!(
        h.post_in(&blue, &format!("/vms/{theirs}/stop"), json!({}))
            .await
            .0
            .as_u16(),
        404,
        "stopping another project's VM must be refused"
    );

    // ---- a shared catalogue is readable by all and writable by none ------
    // NULL project_id is what the migration left on rows that predate tenancy.
    let (st, img) = h
        .post(
            "/images",
            json!({"name": "shared", "source_path": "/x/img.qcow2", "format": "qcow2",
                   "boot": {"type": "firmware", "firmware": "/x/CLOUDHV.fd"}}),
        )
        .await;
    assert!(st.is_success(), "{img}");
    let img = img["id"].as_str().unwrap().to_string();
    h.sql(&format!(
        "UPDATE images SET project_id = NULL WHERE id='{img}'"
    ))
    .await;

    let (st, _) = h.get_status_in(&blue, &format!("/images/{img}")).await;
    assert_eq!(
        st, 200,
        "a platform-shared image is readable from any project"
    );
    assert_eq!(
        h.delete_in(&blue, &format!("/images/{img}")).await,
        404,
        "sharing an image must not hand every project the power to delete it"
    );
    assert_eq!(
        h.query_one(&format!("SELECT name FROM images WHERE id='{img}'"))
            .await,
        "shared"
    );

    // With tenancy on, the default project is a project like any other.
    assert_eq!(
        h.query_one(&format!(
            "SELECT count(*)::text FROM virtual_machines WHERE project_id='{default}'"
        ))
        .await,
        "1"
    );
}
