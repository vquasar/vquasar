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

use std::collections::{HashMap, HashSet};
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
    ConsoleClientMessage, ConsoleServerMessage, DeleteVmRequest, DeleteVolumeRequest,
    DiscardVmRequest, EnsureVmRequest, EnsureVmResponse, FinalizeReceiveRequest,
    GetHostInfoRequest, GetHostInfoResponse, GetVmMetricsRequest, GetVmRequest, GetVmResponse,
    ListVmsRequest, ListVmsResponse, OperationResponse, PrepareReceiveRequest,
    PrepareReceiveResponse, ProvisionVolumeRequest, ProvisionVolumeResponse, SendMigrationRequest,
    StartVmRequest, StopVmRequest, StoragePoolReport, VmMetricsResponse, VmObservedState,
};

static SEQ: AtomicU32 = AtomicU32::new(0);

fn admin_url() -> String {
    std::env::var("E2E_PG_ADMIN_URL")
        .unwrap_or_else(|_| "postgres://ch:ch@127.0.0.1:5432/postgres".to_string())
}

/// Grab a currently-free localhost port (small TOCTOU window, fine for tests).
fn spawn_control(env: &[(String, String)]) -> Child {
    Command::new(env!("CARGO_BIN_EXE_vquasar-control"))
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .spawn()
        .expect("spawn vquasar-control")
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
    /// Hold `ensure_vm` open for this long before answering. The fault-
    /// injection hook: a real create takes seconds, a fake one takes
    /// microseconds, so without a way to widen that window a test cannot kill
    /// the leader *during* a create — which is the case that broke on the lab
    /// (#35).
    ensure_delay_ms: u64,
    /// Number of `ensure_vm` calls that have arrived, across leaders.
    ensure_calls: u32,
    /// When set, `ensure_vm` fails with this message — standing in for the
    /// residue of an interrupted create, which fails identically every time
    /// however often it is retried (#35).
    ensure_error: Option<String>,
    /// vm_id -> how many receivers `prepare_receive` has started for it.
    ///
    /// A count rather than a set because the number is the whole question:
    /// `Manager::prepare_receive` launches a VMM and inserts into `pending`
    /// unconditionally, so calling it twice really does leave two receivers for
    /// one guest. ADR-021 names that as the one non-idempotent step in the
    /// system; counting is how a test can say so (#42).
    pending_receive: HashMap<String, u32>,
    /// vm_ids whose live state has been sent away. Cloud Hypervisor cannot send
    /// a VM twice — the source is left with a husk — so the fake refuses too,
    /// rather than cheerfully accepting a retry the real thing would reject.
    sent: HashSet<String>,
    /// Per-step windows, in the same spirit as `ensure_delay_ms`: hold the RPC
    /// open long enough for a test to interrupt the leader *inside* the step
    /// rather than between ticks.
    prepare_delay_ms: u64,
    send_delay_ms: u64,
    finalize_delay_ms: u64,
    /// What the last `ensure_vm` said about the first NIC's egress policy:
    /// whether it is default-deny, and how many egress rules came with it
    /// (design §18). The control plane can accept an egress rule and still
    /// never send it; this is how a test tells those apart.
    egress_seen: Option<(bool, usize)>,
    /// Volume paths this host has been asked to build, in order (ADR-025).
    /// A control plane that "provisions" a local volume without ever asking
    /// the host that owns the disk is the failure this catches.
    volumes_built: Vec<String>,
    /// Volume paths this host has been asked to remove.
    volumes_removed: Vec<String>,
    /// Storage pools this host refuses to report as usable, keyed by pool name
    /// with the reason it gives (ADR-023). Anything else it is asked about, it
    /// can use. Refusing by *name* is deliberate: the control plane is what
    /// hands out ids, so a test that used them would be asserting on its own
    /// bookkeeping rather than on the host's answer.
    pools_refused: HashMap<String, String>,
    /// Every controller lease epoch seen on an `ensure_vm`, in arrival order.
    /// The agent-side check is unit-tested; what this proves is the *wiring* —
    /// that the control plane actually puts its epoch on the wire (ADR-022).
    epochs_seen: Vec<Option<i64>>,
    /// Call counts, across leaders. These are the assertions.
    prepare_calls: u32,
    send_calls: u32,
    finalize_calls: u32,
    discard_calls: u32,
    /// Calls that ran to completion. tonic drops a handler future when the
    /// client disconnects, so a control plane dying mid-RPC *cancels* the
    /// agent's work at whatever await it had reached. The gap between
    /// `_calls` and `_completed` is that cancellation, and both real handlers
    /// mutate agent state before their await — so the gap is not harmless.
    prepare_completed: u32,
    finalize_completed: u32,
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
        r: Request<GetHostInfoRequest>,
    ) -> Result<Response<GetHostInfoResponse>, Status> {
        let probes = r.into_inner().pools;
        let (vm_count, storage_pools) = {
            let st = self.state.lock().unwrap();
            let reports: Vec<_> = probes
                .iter()
                .map(|p| match st.pools_refused.get(&p.name) {
                    Some(why) => StoragePoolReport {
                        pool_id: p.pool_id.clone(),
                        usable: false,
                        message: why.clone(),
                        capacity_bytes: 0,
                        available_bytes: 0,
                    },
                    None => StoragePoolReport {
                        pool_id: p.pool_id.clone(),
                        usable: true,
                        message: String::new(),
                        capacity_bytes: FAKE_POOL_CAPACITY,
                        available_bytes: FAKE_POOL_CAPACITY / 2,
                    },
                })
                .collect();
            (st.vms.len() as u32, reports)
        };
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
            storage_pools,
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
        let epoch = r
            .metadata()
            .get("x-vquasar-controller-epoch")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<i64>().ok());
        let req = r.into_inner();
        let (delay, err) = {
            let mut st = self.state.lock().unwrap();
            st.ensure_calls += 1;
            st.epochs_seen.push(epoch);
            if let Some(n) = req.networks.first() {
                st.egress_seen = Some((n.egress_default_deny, n.egress_rules.len()));
            }
            (st.ensure_delay_ms, st.ensure_error.clone())
        };
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        if let Some(msg) = err {
            return Err(Status::internal(msg));
        }
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
        let delay = {
            let mut st = self.state.lock().unwrap();
            st.prepare_calls += 1;
            // At most one receiver per VM, like `Manager::prepare_receive`: a
            // repeat returns the receiver that exists rather than starting a
            // second one (#45). The bookkeeping happens before the delay,
            // mirroring an agent whose work survives its caller disconnecting.
            let slot = st.pending_receive.entry(id).or_insert(0);
            if *slot == 0 {
                *slot = 1;
            }
            st.prepare_delay_ms
        };
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        self.state.lock().unwrap().prepare_completed += 1;
        Ok(Response::new(PrepareReceiveResponse {
            migration_url: "tcp:127.0.0.1:0".into(),
        }))
    }

    async fn send_migration(
        &self,
        r: Request<SendMigrationRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = r.into_inner().vm_id;
        let delay = {
            let mut st = self.state.lock().unwrap();
            st.send_calls += 1;
            // `Manager::send_migration` looks the VM up in `vms` and fails if it
            // is not there; CH then refuses to send a VM whose state has already
            // gone. Both refusals matter to an interrupted `Sending`.
            if !st.vms.contains_key(&id) {
                return Err(Status::not_found(format!("vm {id} not managed here")));
            }
            if st.sent.contains(&id) {
                return Err(Status::failed_precondition(format!(
                    "vm {id} has already sent its state"
                )));
            }
            st.send_delay_ms
        };
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        // The source is left holding a husk: CH shuts the VM down once its state
        // has gone, and `discard_vm` collects it later. Leaving it `Running`
        // here would let a test claiming "the VM is still on its source host"
        // pass over a guest that is not running anywhere.
        let mut st = self.state.lock().unwrap();
        st.sent.insert(id.clone());
        st.vms.insert(id, Phase::Stopped);
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
        let delay = {
            let mut st = self.state.lock().unwrap();
            st.finalize_calls += 1;
            // Idempotent once the guest is adopted: finalise is past the point
            // of no return, so a repeat has to report success rather than send
            // the controller down its failure path (#45).
            if matches!(st.vms.get(&id), Some(Phase::Running)) {
                return Ok(Response::new(EnsureVmResponse {
                    state: Some(Self::observed(&id, Phase::Running)),
                }));
            }
            match st.pending_receive.get_mut(&id) {
                Some(n) if *n > 0 => *n -= 1,
                _ => return Err(Status::not_found(format!("no pending receive for {id}"))),
            }
            // Adopt before the delay, not after: `Manager::finalize_receive`
            // now runs in a spawned task, so a caller that disconnects mid-call
            // no longer takes the transferred guest with it.
            st.vms.insert(id.clone(), Phase::Running);
            st.finalize_delay_ms
        };
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
        self.state.lock().unwrap().finalize_completed += 1;
        Ok(Response::new(EnsureVmResponse {
            state: Some(Self::observed(&id, Phase::Running)),
        }))
    }

    async fn discard_vm(
        &self,
        r: Request<DiscardVmRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let id = r.into_inner().vm_id;
        let mut st = self.state.lock().unwrap();
        st.discard_calls += 1;
        // Clears a prepared receiver as well as a managed VM: that is how the
        // controller frees a VM for a later migration after one fails (#45).
        st.pending_receive.remove(&id);
        st.vms.remove(&id);
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "discarded".into(),
        }))
    }
    async fn provision_volume(
        &self,
        r: Request<ProvisionVolumeRequest>,
    ) -> Result<Response<ProvisionVolumeResponse>, Status> {
        let req = r.into_inner();
        self.state.lock().unwrap().volumes_built.push(req.path);
        // Echo the requested size: a real agent reports what qemu-img made,
        // and for a blank volume that is what was asked for.
        Ok(Response::new(ProvisionVolumeResponse {
            size_bytes: req.size_bytes,
        }))
    }

    async fn delete_volume(
        &self,
        r: Request<DeleteVolumeRequest>,
    ) -> Result<Response<OperationResponse>, Status> {
        let req = r.into_inner();
        self.state.lock().unwrap().volumes_removed.push(req.path);
        Ok(Response::new(OperationResponse {
            accepted: true,
            message: "removed".into(),
        }))
    }
}

/// What a fake host says a usable storage pool measures. A constant so a test
/// can tell "the number came from the host" apart from "the number came from
/// anywhere else" (ADR-023).
const FAKE_POOL_CAPACITY: u64 = 1 << 40;

/// Start a fake agent on `port`; returns its shared state (for assertions).
fn spawn_agent(host_id: &str, port: u16) -> Arc<Mutex<AgentState>> {
    spawn_agent_stoppable(host_id, port).0
}

/// The same, with a handle that stops the server — so a test can make a host
/// genuinely unreachable rather than merely unregistered.
fn spawn_agent_stoppable(
    host_id: &str,
    port: u16,
) -> (Arc<Mutex<AgentState>>, tokio::sync::oneshot::Sender<()>) {
    let state = Arc::new(Mutex::new(AgentState::default()));
    let agent = FakeAgent {
        host_id: host_id.to_string(),
        state: state.clone(),
    };
    let addr = format!("127.0.0.1:{port}").parse().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        Server::builder()
            .add_service(HostAgentServer::new(agent))
            .serve_with_shutdown(addr, async {
                // Only an explicit send stops the server. Most tests never want
                // to stop theirs and drop the handle immediately, and a dropped
                // oneshot resolves — so without this the agent would shut down
                // the moment `spawn_agent` returned.
                if stopped.await.is_err() {
                    std::future::pending::<()>().await;
                }
            })
            .await
            .unwrap();
    });
    (state, stop)
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
    /// Kept so the process can be restarted on the same address with the same
    /// settings — startup behaviour (migrations, orphan recovery) is only
    /// testable across a restart. The listen address is in here too, so the
    /// restart lands on the same port.
    env: Vec<(String, String)>,
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

        let mut env: Vec<(String, String)> = vec![
            ("VQUASAR_CONTROL_DATABASE__URL".into(), db_url.clone()),
            (
                "VQUASAR_CONTROL_SERVER__LISTEN".into(),
                format!("127.0.0.1:{port}"),
            ),
            ("VQUASAR_CONTROL_AUTH__DISABLED".into(), "true".into()),
            // The harness database is a throwaway with no TLS. Encryption is
            // the default now, so opting out has to be explicit — which is the
            // point: it is visible here rather than assumed.
            (
                "VQUASAR_CONTROL_DATABASE__SSL_MODE".into(),
                "disable".into(),
            ),
            // The harness's specs use synthetic paths, so declare the root they
            // live under; a real install keeps the default (/var/lib/vquasar).
            (
                "VQUASAR_CONTROL_STORAGE__ALLOWED_PATHS".into(),
                "[\"/x\"]".into(),
            ),
            (
                "VQUASAR_CONTROL_RECONCILE__INTERVAL_SECS".into(),
                "1".into(),
            ),
            ("RUST_LOG".into(), "warn".into()),
        ];
        env.extend(
            extra_env
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );

        let control = spawn_control(&env);
        let h = Harness {
            control,
            base,
            client: reqwest::Client::new(),
            db_name,
            db_url: db_url.clone(),
            env,
        };
        h.wait_healthy().await;
        h
    }

    async fn metrics(&self) -> String {
        self.client
            .get(format!("{}/metrics", self.base))
            .send()
            .await
            .expect("metrics")
            .text()
            .await
            .unwrap_or_default()
    }

    /// Start a second control plane against the same database, on its own port
    /// and with its own instance id. Returns a handle that kills it on drop.
    ///
    /// This is the whole point of the HA work: two processes, one database.
    async fn start_peer(&self, instance_id: &str) -> Peer {
        let port = free_port();
        let mut env: Vec<(String, String)> = self
            .env
            .iter()
            .map(|(k, v)| {
                if k == "VQUASAR_CONTROL_SERVER__LISTEN" {
                    (k.clone(), format!("127.0.0.1:{port}"))
                } else {
                    (k.clone(), v.clone())
                }
            })
            .collect();
        env.push((
            "VQUASAR_CONTROL_SERVER__INSTANCE_ID".into(),
            instance_id.to_string(),
        ));
        let child = spawn_control(&env);
        let peer = Peer {
            child,
            base: format!("http://127.0.0.1:{port}"),
            client: reqwest::Client::new(),
        };
        for _ in 0..100 {
            if let Ok(r) = peer
                .client
                .get(format!("{}/healthz", peer.base))
                .send()
                .await
            {
                if r.status().is_success() {
                    return peer;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        panic!("peer control plane did not become healthy");
    }

    /// Ask the control plane to stop the way systemd does — SIGTERM — and wait
    /// for it to exit. `Child::kill` sends SIGKILL, which is a crash, not a
    /// shutdown, and would exercise nothing.
    async fn stop_gracefully(&mut self) {
        // Via kill(1) rather than a libc binding: the harness needs one signal
        // in one place, and a dependency for that is not worth it.
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.control.id().to_string())
            .status();
        let _ = self.control.wait();
    }

    /// Kill the control plane and start it again on the same address and
    /// database — the only way to exercise what startup does.
    async fn restart(&mut self) {
        let _ = self.control.kill();
        let _ = self.control.wait();
        self.control = spawn_control(&self.env);
        self.wait_healthy().await;
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

    /// Poll a single-value query until it satisfies `pred`. Migration state is
    /// not on the API — a migration is reported through the VM's phase and its
    /// task — so an interruption test that wants to know which *step* was
    /// interrupted has to ask the database.
    async fn wait_sql(&self, sql: &str, pred: impl Fn(&str) -> bool, what: &str) -> String {
        let mut last = String::new();
        for _ in 0..600 {
            last = self.query_one(sql).await;
            if pred(&last) {
                return last;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("timed out waiting for {what}; last value was {last:?}");
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

    /// GET returning status and body.
    async fn get_status(&self, path: &str) -> (u16, Value) {
        let r = self
            .client
            .get(format!("{}/api/v1{path}", self.base))
            .send()
            .await
            .expect("get");
        let st = r.status().as_u16();
        (st, r.json().await.unwrap_or(Value::Null))
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

    /// PUT returning status and body.
    async fn put(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let r = self
            .client
            .put(format!("{}/api/v1{path}", self.base))
            .json(&body)
            .send()
            .await
            .expect("put");
        let st = r.status();
        (st, r.json().await.unwrap_or(Value::Null))
    }

    /// PATCH returning status and body — some of these are refusals whose
    /// message is the point.
    async fn patch_body(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .client
            .patch(format!("{}/api/v1{path}", self.base))
            .json(&body)
            .send()
            .await
            .expect("patch");
        let st = r.status().as_u16();
        (st, r.json().await.unwrap_or(Value::Null))
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
        // 60s. Long enough for the reconcile retry budget (#35) to run out,
        // which is the slowest thing any test legitimately waits for. This
        // bounds failures only — a passing test returns as soon as it can.
        for _ in 0..120 {
            let v = self.get(path).await;
            if pred(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("timed out waiting for: {what} (at {path})");
    }
}

/// Whether this instance's own metrics say it is running the controllers.
fn leading(metrics: &str) -> bool {
    metrics
        .lines()
        .find(|l| l.starts_with("vquasar_controller_is_leader "))
        .and_then(|l| l.split_whitespace().nth(1))
        .map(|v| v.starts_with('1'))
        .unwrap_or(false)
}

/// A second control plane sharing the harness's database.
struct Peer {
    child: Child,
    base: String,
    client: reqwest::Client,
}

impl Peer {
    /// Raw `/metrics` text, for asserting on this instance's own gauges rather
    /// than on shared database state.
    async fn metrics(&self) -> String {
        self.client
            .get(format!("{}/metrics", self.base))
            .send()
            .await
            .expect("peer metrics")
            .text()
            .await
            .unwrap_or_default()
    }

    async fn get(&self, path: &str) -> Value {
        self.client
            .get(format!("{}/api/v1{path}", self.base))
            .send()
            .await
            .expect("peer get")
            .json()
            .await
            .unwrap_or(Value::Null)
    }

    /// Write through the *peer*, for asserting on what the new leader does
    /// rather than on what the database ended up holding.
    async fn post(&self, path: &str, body: Value) -> (reqwest::StatusCode, Value) {
        let r = self
            .client
            .post(format!("{}/api/v1{path}", self.base))
            .json(&body)
            .send()
            .await
            .expect("peer post");
        let st = r.status();
        (st, r.json().await.unwrap_or(Value::Null))
    }

    /// Block until this instance is the one running the controllers.
    async fn wait_until_leader(&self) {
        for _ in 0..120 {
            if self
                .metrics()
                .await
                .contains("vquasar_controller_is_leader 1")
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("peer never became the controller");
    }

    /// Poll this instance until `pred` holds — asserting against the *peer*,
    /// so the test proves the peer finished the work rather than that the row
    /// changed somehow.
    async fn wait_for(&self, path: &str, pred: impl Fn(&Value) -> bool, what: &str) -> Value {
        for _ in 0..120 {
            let v = self.get(path).await;
            if pred(&v) {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        panic!("timed out waiting for: {what} (peer, at {path})");
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
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

/// Storage pools (ADR-023). The point of the resource is that a pool says where
/// bytes go and *nothing else* — whether it works is observed from the agents,
/// so a pool nobody reports says `pending` instead of looking correct.
#[tokio::test]
async fn storage_pools_are_seeded_confined_and_usable_only_when_reported() {
    let h = Harness::start_with(&[(
        "VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR",
        "/x/shared/volumes",
    )])
    .await;

    // A cluster that predates pools gets one seeded from the directory it was
    // already using, so nothing moves and no volume path changes.
    let pools = h.get("/storage-pools").await;
    let pools = pools.as_array().expect("pool list");
    assert_eq!(pools.len(), 1, "{pools:?}");
    let d = &pools[0];
    assert_eq!(d["name"], "default");
    assert_eq!(d["kind"], "shared_dir");
    assert_eq!(d["params"]["path"], "/x/shared/volumes");
    // Configured, and reported by nobody. Saying `ready` here would be the
    // exact lie this resource exists to remove.
    assert_eq!(d["state"], "pending", "{d}");
    assert_eq!(d["reachable_hosts"], 0);
    assert!(d["capacity_bytes"].is_null(), "{d}");

    // A pool names a directory the agent opens with privilege, so it is
    // confined like any other caller-supplied host path (design §30).
    for (what, path) in [
        ("outside the roots", "/etc/vquasar/tls"),
        ("traversal", "/x/../etc/vquasar/tls"),
        ("relative", "x/rel"),
    ] {
        let (st, body) = h
            .post(
                "/storage-pools",
                json!({"name": "escape", "kind": "shared_dir", "path": path}),
            )
            .await;
        assert_eq!(st.as_u16(), 400, "{what} was accepted: {body}");
    }

    // Names end up in operator-facing places and eventually in paths.
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "Fast Pool", "kind": "shared_dir", "path": "/x/fast"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");

    // An unknown kind is refused rather than stored as an opaque blob.
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "ceph", "kind": "rbd", "pool": "vms"}),
        )
        .await;
    assert!(
        st.as_u16() == 400 || st.as_u16() == 422,
        "unknown kind accepted: {st} {body}"
    );

    let (st, fast) = h
        .post(
            "/storage-pools",
            json!({"name": "fast", "kind": "shared_dir",
                   "path": "/x/fast", "description": "NVMe"}),
        )
        .await;
    assert!(st.is_success(), "{fast}");
    let id = fast["id"].as_str().expect("pool id").to_string();
    assert_eq!(fast["state"], "pending", "brand new pool: {fast}");

    // One directory is one pool. Two would double-count the same disk's
    // capacity and split its volumes across two namespaces.
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "fast-2", "kind": "shared_dir", "path": "/x/fast"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "same directory twice: {body}");
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "fast", "kind": "shared_dir", "path": "/x/other"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "duplicate name: {body}");

    // A pool's identity is where its bytes are. Renaming is fine; repointing
    // it would strand every volume in it while the row still looked correct,
    // so the field is not editable — sending it changes nothing.
    let (st, patched) = h
        .patch_body(
            &format!("/storage-pools/{id}"),
            json!({"name": "fast-nvme", "kind": "shared_dir", "path": "/x/elsewhere"}),
        )
        .await;
    assert_eq!(st, 200, "{patched}");
    assert_eq!(patched["name"], "fast-nvme");
    assert_eq!(
        patched["params"]["path"], "/x/fast",
        "a pool was repointed underneath its volumes: {patched}"
    );

    // Reachability is observed, never declared: there is no API to say "this
    // host can see that pool". The row below is what an agent's report will
    // write (M23 step 2); what is under test here is that the pool's state
    // follows it.
    let host = "11111111-1111-4111-8111-111111111111";
    h.sql(&format!(
        "INSERT INTO hosts (id, name, endpoint, created_at, updated_at)
         VALUES ('{host}', 'reporter', 'http://127.0.0.1:1', now(), now())"
    ))
    .await;
    h.sql(&format!(
        "INSERT INTO storage_pool_reachability
             (pool_id, host_id, capacity_bytes, available_bytes, reported_at)
         VALUES ('{id}', '{host}', 1000, 400, now())"
    ))
    .await;
    let after = h.get(&format!("/storage-pools/{id}")).await;
    assert_eq!(after["state"], "ready", "{after}");
    assert_eq!(after["reachable_hosts"], 1);
    assert_eq!(after["capacity_bytes"], 1000);
    assert_eq!(after["available_bytes"], 400);

    // Deleting the pool takes its observations with it: a stale reachability
    // row would otherwise outlive the thing it described.
    assert_eq!(
        h.delete(&format!("/storage-pools/{id}")).await.as_u16(),
        204
    );
    assert_eq!(h.get_status(&format!("/storage-pools/{id}")).await.0, 404);
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM storage_pool_reachability")
            .await,
        "0"
    );
    assert_eq!(
        h.delete(&format!("/storage-pools/{id}")).await.as_u16(),
        404
    );
}

/// A pool is usable exactly while some host says it is (ADR-023). Nothing an
/// operator types makes it so, and nothing keeps it so once the host stops
/// saying it.
#[tokio::test]
async fn a_pool_is_usable_only_while_a_host_reports_it() {
    let h = Harness::start_with(&[(
        "VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR",
        "/x/shared/volumes",
    )])
    .await;

    // Two pools: one the host will report, one it will refuse. Both look
    // equally correct from the control plane's side, which is the point.
    let (st, unmounted) = h
        .post(
            "/storage-pools",
            json!({"name": "unmounted", "kind": "shared_dir", "path": "/x/unmounted"}),
        )
        .await;
    assert!(st.is_success(), "{unmounted}");
    let unmounted_id = unmounted["id"].as_str().unwrap().to_string();
    let pools = h.get("/storage-pools").await;
    let default_id = pools
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "default")
        .and_then(|p| p["id"].as_str())
        .expect("the seeded default pool")
        .to_string();

    let port = free_port();
    let (agent, stop_agent) = spawn_agent_stoppable("hostA", port);
    agent.lock().unwrap().pools_refused.insert(
        "unmounted".into(),
        "/x/unmounted does not exist here — the pool is probably not mounted on this host".into(),
    );
    let host = h.register_host("hostA", port).await;

    // The pool the host can use becomes ready, carrying the host's numbers.
    // Nothing typed them: they arrived over the wire from the agent.
    let d = h
        .wait_for(
            &format!("/storage-pools/{default_id}"),
            |p| p["state"] == "ready",
            "the reported pool becoming ready",
        )
        .await;
    assert_eq!(d["reachable_hosts"], 1, "{d}");
    assert_eq!(d["capacity_bytes"], json!(FAKE_POOL_CAPACITY));
    assert_eq!(d["available_bytes"], json!(FAKE_POOL_CAPACITY / 2));

    // The refused one stays pending — and says who refused it and why, so the
    // answer to "why was my placement refused" is not an ssh session.
    let u = h
        .wait_for(
            &format!("/storage-pools/{unmounted_id}"),
            |p| !p["hosts"].as_array().map(Vec::is_empty).unwrap_or(true),
            "a report about the unmounted pool",
        )
        .await;
    assert_eq!(u["state"], "pending", "{u}");
    assert_eq!(u["reachable_hosts"], 0);
    assert!(
        u["capacity_bytes"].is_null(),
        "unusable pool got a size: {u}"
    );
    let report = &u["hosts"][0];
    assert_eq!(report["host_id"], json!(host));
    assert_eq!(report["host_name"], "hostA");
    assert_eq!(report["usable"], json!(false));
    // No measurement, rather than a measurement of zero: a host that cannot use
    // a pool did not find it empty, it did not find it at all.
    assert!(report["capacity_bytes"].is_null(), "{report}");
    assert!(report["available_bytes"].is_null(), "{report}");
    assert!(
        report["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not mounted"),
        "{report}"
    );

    // A host that stops answering stops vouching for anything. Leaving its last
    // word in place would keep a pool `ready` on the strength of a machine that
    // is gone — the stale-declaration failure, one level up.
    let _ = stop_agent.send(());
    h.wait_for(
        &format!("/storage-pools/{default_id}"),
        |p| p["state"] == "pending",
        "the pool going pending once its only host is unreachable",
    )
    .await;
}

/// A volume belongs to a pool, its file lives there, and a VM whose disks are
/// in a pool is only placed on a host that reports it (ADR-023). This is the
/// failure that used to be a path error at launch, moved to placement.
#[tokio::test]
async fn a_vm_is_placed_only_where_its_pool_is_reported() {
    let h = Harness::start_with(&[(
        "VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR",
        "/x/shared/volumes",
    )])
    .await;

    let (st, fast) = h
        .post(
            "/storage-pools",
            json!({"name": "fast", "kind": "shared_dir", "path": "/x/fast"}),
        )
        .await;
    assert!(st.is_success(), "{fast}");
    let fast_id = fast["id"].as_str().unwrap().to_string();

    // Volume rows are inserted directly here: building the file runs qemu-img,
    // which this harness deliberately does not require. What is under test is
    // where the control plane says a volume's bytes are, and that is derived
    // from its pool rather than from the old config value.
    let in_fast = "22222222-2222-4222-8222-222222222222";
    let grandfathered = "33333333-3333-4333-8333-333333333333";
    h.sql(&format!(
        "INSERT INTO volumes (id, name, size_bytes, format, status, pool_id, created_at, updated_at)
         VALUES ('{in_fast}', 'v1', 1048576, 'qcow2', 'ready', '{fast_id}', now(), now()),
                ('{grandfathered}', 'v0', 1048576, 'qcow2', 'ready', NULL, now(), now())"
    ))
    .await;

    let v = h.get(&format!("/volumes/{in_fast}")).await;
    assert_eq!(v["pool_id"], json!(fast_id));
    assert_eq!(v["path"], json!(format!("/x/fast/vol-{in_fast}.qcow2")));

    // A volume that predates pools keeps exactly the path it had. Nothing an
    // upgrade does may move a file that a running VM has open.
    let v0 = h.get(&format!("/volumes/{grandfathered}")).await;
    assert_eq!(
        v0["path"],
        json!(format!("/x/shared/volumes/vol-{grandfathered}.qcow2"))
    );

    // Naming a pool that does not exist is refused before any work happens.
    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "v3", "size_bytes": 1024, "pool": "nowhere"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "unknown pool accepted: {body}");

    // A pool holding volumes does not vanish: losing the record of where bytes
    // are is worse than a refusal.
    let (st, body) = h.delete_status(&format!("/storage-pools/{fast_id}")).await;
    assert_eq!(st, 400, "{body}");
    assert!(
        format!("{body}").contains("still live"),
        "the refusal must say why: {body}"
    );

    // A second kind. Its mount point is the pool's host path in exactly the
    // way a shared directory's path is, so everything downstream is unchanged.
    let (st, nfs) = h
        .post(
            "/storage-pools",
            json!({"name": "shelf", "kind": "nfs", "server": "10.0.0.5",
                   "export": "/exports/vms", "mount_point": "/x/nfs/shelf"}),
        )
        .await;
    assert!(st.is_success(), "{nfs}");
    assert_eq!(nfs["params"]["server"], "10.0.0.5");
    assert_eq!(nfs["state"], "pending", "nothing has mounted it yet: {nfs}");

    // The server field is an address, not mount syntax: `10.0.0.5:/exports`
    // typed there would build `10.0.0.5:/exports:/exports`.
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "wrong", "kind": "nfs", "server": "10.0.0.5:/exports",
                   "export": "/exports/vms", "mount_point": "/x/nfs/wrong"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");

    // One host path is one pool *across kinds*: a shared directory and an NFS
    // mount point at the same place would double-count one filesystem.
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "clash", "kind": "shared_dir", "path": "/x/nfs/shelf"}),
        )
        .await;
    assert_eq!(
        st.as_u16(),
        400,
        "same directory under another kind: {body}"
    );

    // And one export is one pool, however many paths it is offered at.
    let (st, body) = h
        .post(
            "/storage-pools",
            json!({"name": "twice", "kind": "nfs", "server": "10.0.0.5",
                   "export": "/exports/vms", "mount_point": "/x/nfs/again"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "same export twice: {body}");

    // Now placement. The host can use `default` but not `fast`.
    let port = free_port();
    let agent = spawn_agent("hostA", port);
    agent
        .lock()
        .unwrap()
        .pools_refused
        .insert("fast".into(), "not mounted on this host".into());
    h.register_host("hostA", port).await;

    let mut spec = vm_spec();
    spec["disks"] = json!([{
        "path": "/x/fast/vol-a.qcow2",
        "image_type": "qcow2",
        "pool": fast_id,
    }]);
    let (st, v) = h
        .post("/vms", json!({"name": "pooled-vm", "spec": spec}))
        .await;
    assert!(st.is_success(), "{v}");
    let vm_id = v["vm_id"].as_str().unwrap().to_string();

    // It must not be placed, and the open task must say *why* — an operator
    // waiting for capacity that will never arrive is the failure here.
    let task = h
        .wait_for(
            &format!("/tasks/{}", v["task_id"].as_str().unwrap()),
            |t| {
                t["message"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("storage pool")
            },
            "a refusal naming the storage",
        )
        .await;
    assert!(
        !format!("{task}").contains("waiting for a schedulable host"),
        "a storage refusal must not read as a capacity problem: {task}"
    );
    let vm = h.get(&format!("/vms/{vm_id}")).await;
    assert!(vm["host_id"].is_null(), "placed anyway: {vm}");

    // Once the host reports the pool, the same VM places without any change to
    // the VM itself. Nothing about the desired state moved; the observation did.
    agent.lock().unwrap().pools_refused.clear();
    h.wait_for(
        &format!("/vms/{vm_id}"),
        |v| !v["host_id"].is_null(),
        "the VM placing once its pool is reported",
    )
    .await;

    // Live migration has always assumed the destination has the same storage
    // mounted at the same path, and nothing checked it — the guest arrived and
    // failed to launch on the far side. Now the target is refused up front.
    h.wait_for(
        &format!("/vms/{vm_id}"),
        |v| v["phase"] == "Running",
        "the VM running before it can be migrated",
    )
    .await;
    let b_port = free_port();
    let b_agent = spawn_agent("hostB", b_port);
    b_agent
        .lock()
        .unwrap()
        .pools_refused
        .insert("fast".into(), "not mounted on this host".into());
    let host_b = h.register_host("hostB", b_port).await;
    let (st, body) = h
        .post(
            &format!("/vms/{vm_id}/migrate"),
            json!({"target_host_id": host_b}),
        )
        .await;
    assert_eq!(
        st.as_u16(),
        400,
        "migrated onto storage it cannot see: {body}"
    );
    assert!(
        format!("{body}").contains("storage pool"),
        "the refusal must name what is missing: {body}"
    );

    // A disk the control plane places itself records the pool it chose. Without
    // that, a VM's own system disk would constrain nothing and the refusal
    // above would only ever fire for attached volumes.
    let default_id = h
        .get("/storage-pools")
        .await
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "default")
        .and_then(|p| p["id"].as_str())
        .unwrap()
        .to_string();
    let mut blank = vm_spec();
    blank["disks"] = json!([{"path": "", "image_type": "qcow2", "size_bytes": 1_048_576}]);
    let (st, v) = h
        .post("/vms", json!({"name": "auto-placed", "spec": blank}))
        .await;
    assert!(st.is_success(), "{v}");
    let auto = h
        .get(&format!("/vms/{}", v["vm_id"].as_str().unwrap()))
        .await;
    let disk = &auto["spec"]["disks"][0];
    assert_eq!(disk["pool"], json!(default_id), "{auto}");
    assert!(
        disk["path"]
            .as_str()
            .unwrap_or_default()
            .starts_with("/x/shared/volumes/"),
        "{auto}"
    );

    // A disk naming a pool that does not exist is a VM that could never be
    // placed anywhere, so it is refused at the door.
    let mut bogus = vm_spec();
    bogus["disks"] = json!([{
        "path": "/x/fast/ghost.qcow2",
        "image_type": "qcow2",
        "pool": "00000000-0000-4000-8000-000000000000",
    }]);
    let (st, body) = h
        .post("/vms", json!({"name": "ghost", "spec": bogus}))
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
}

/// A pool with files in it, some of which no longer have an owner (#41).
///
/// Returns the pool root and the env a harness needs to sweep it, having
/// written: an orphaned VM disk, an orphaned seed, an orphaned volume file, and
/// two files that must survive whatever the policy — one belonging to a live
/// VM, one the platform never made.
fn orphan_fixture(tag: &str) -> (std::path::PathBuf, Vec<(String, String)>) {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!(
        "vquasar-e2e-orphans-{}-{tag}-{seq}",
        std::process::id()
    ));
    std::fs::create_dir_all(root.join("seeds")).expect("pool root");
    let env = vec![
        (
            "VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR".to_string(),
            root.to_string_lossy().into_owned(),
        ),
        (
            "VQUASAR_CONTROL_STORAGE__ALLOWED_PATHS".to_string(),
            format!("[\"/x\",\"{}\"]", root.display()),
        ),
        // Nothing here is being written concurrently, so the settling guard has
        // no work to do; it is exercised as a unit test instead.
        (
            "VQUASAR_CONTROL_STORAGE__ORPHAN_MIN_AGE_SECS".to_string(),
            "0".to_string(),
        ),
        (
            "VQUASAR_CONTROL_STORAGE__ORPHAN_SWEEP_SECS".to_string(),
            "1".to_string(),
        ),
    ];
    (root, env)
}

/// Files whose owning row is gone are reported, and reclaimed only when asked
/// (#41). The sweep lives here because a seed on shared storage may belong to a
/// VM on any host — an agent doing this would be deleting another host's work.
#[tokio::test]
async fn orphaned_files_are_reclaimed_only_when_asked() {
    let (root, mut env) = orphan_fixture("delete");
    env.push((
        "VQUASAR_CONTROL_STORAGE__ORPHAN_RECLAIM".to_string(),
        "delete".to_string(),
    ));
    let refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let h = Harness::start_with(&refs).await;

    // A VM that still exists. Its disk must survive the sweep.
    let (st, v) = h
        .post("/vms", json!({"name": "keeper", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let live = v["vm_id"].as_str().unwrap().to_string();

    let dead_vm = "44444444-4444-4444-8444-444444444444";
    let dead_vol = "55555555-5555-4555-8555-555555555555";
    let keep_disk = root.join(format!("{live}.qcow2"));
    let orphan_disk = root.join(format!("{dead_vm}.qcow2"));
    let orphan_extra = root.join(format!("{dead_vm}-disk1.raw"));
    let orphan_vol = root.join(format!("vol-{dead_vol}.qcow2"));
    let orphan_seed = root.join("seeds").join(format!("{dead_vm}.iso"));
    let not_ours = root.join("ubuntu-24.04.qcow2");
    for f in [
        &keep_disk,
        &orphan_disk,
        &orphan_extra,
        &orphan_vol,
        &orphan_seed,
        &not_ours,
    ] {
        std::fs::write(f, b"x").expect("fixture file");
    }

    h.wait_for(
        "/events",
        |evs| {
            evs.as_array().is_some_and(|e| {
                e.iter()
                    .any(|ev| ev["event_type"] == "storage.orphans" && ev["message"] != json!(null))
            })
        },
        "a sweep to report what it found",
    )
    .await;

    // Everything with no owner goes.
    for gone in [&orphan_disk, &orphan_extra, &orphan_vol, &orphan_seed] {
        assert!(!gone.exists(), "not reclaimed: {}", gone.display());
    }
    // A live VM's disk stays, and so does a file the platform never made —
    // that second one is what keeps this safe to switch on.
    assert!(keep_disk.exists(), "reclaimed a live VM's disk");
    assert!(not_ours.exists(), "reclaimed an operator's own file");

    let _ = std::fs::remove_dir_all(&root);
}

/// The default policy looks and tells, and touches nothing.
#[tokio::test]
async fn orphaned_files_are_reported_without_being_touched_by_default() {
    let (root, env) = orphan_fixture("report");
    let refs: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let h = Harness::start_with(&refs).await;

    let dead_vm = "66666666-6666-4666-8666-666666666666";
    let orphan_seed = root.join("seeds").join(format!("{dead_vm}.iso"));
    std::fs::write(&orphan_seed, b"x").expect("fixture file");

    let evs = h
        .wait_for(
            "/events",
            |evs| {
                evs.as_array()
                    .is_some_and(|e| e.iter().any(|ev| ev["event_type"] == "storage.orphans"))
            },
            "a sweep to report what it found",
        )
        .await;
    let msg = evs
        .as_array()
        .unwrap()
        .iter()
        .find(|ev| ev["event_type"] == "storage.orphans")
        .and_then(|ev| ev["message"].as_str())
        .unwrap_or_default()
        .to_string();
    // The report has to say how to act on it, or it is just a number.
    assert!(msg.contains("orphan_reclaim"), "{msg}");
    assert!(
        orphan_seed.exists(),
        "the default policy deleted a file: {}",
        orphan_seed.display()
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Egress rules are enforced, or refused — never accepted and ignored
/// (design §18). An accepted rule that changes nothing is a control an operator
/// believes in and does not have.
#[tokio::test]
async fn an_egress_rule_is_refused_unless_the_cluster_enforces_egress() {
    let h = Harness::start().await;
    let (st, sg) = h.post("/security-groups", json!({"name": "web"})).await;
    assert!(st.is_success(), "{sg}");
    let sg_id = sg["id"].as_str().unwrap().to_string();

    let (st, body) = h
        .post(
            &format!("/security-groups/{sg_id}/rules"),
            json!({"direction": "egress", "protocol": "tcp",
                   "port_min": 443, "port_max": 443}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
    assert!(
        format!("{body}").contains("egress_mode"),
        "the refusal has to say how to make it enforceable: {body}"
    );

    // An ingress rule on the same group is unaffected.
    let (st, body) = h
        .post(
            &format!("/security-groups/{sg_id}/rules"),
            json!({"direction": "ingress", "protocol": "tcp",
                   "port_min": 22, "port_max": 22}),
        )
        .await;
    assert!(st.is_success(), "{body}");
}

/// With egress enforced, the rule is accepted *and reaches the agent* with the
/// flag that makes it mean something.
#[tokio::test]
async fn an_enforced_egress_rule_reaches_the_agent() {
    let h = Harness::start_with(&[
        ("VQUASAR_CONTROL_NETWORK__EGRESS_MODE", "enforced"),
        ("VQUASAR_CONTROL_NETWORK__POLICY_MODE", "enforced"),
    ])
    .await;
    let port = free_port();
    let agent = spawn_agent("hostA", port);
    h.register_host("hostA", port).await;

    let (st, net) = h
        .post("/networks", json!({"name": "tenant-a", "kind": "tenant"}))
        .await;
    assert!(st.is_success(), "{net}");
    let net_id = net["id"].as_str().unwrap().to_string();
    // Every network carries a default group from creation (ADR-017); putting
    // the rule there is what makes it apply to every NIC on the network.
    let sg_id = net["default_security_group_id"]
        .as_str()
        .unwrap()
        .to_string();

    let (st, body) = h
        .post(
            &format!("/security-groups/{sg_id}/rules"),
            json!({"direction": "egress", "protocol": "tcp",
                   "port_min": 443, "port_max": 443, "remote_cidr": "10.9.0.0/16"}),
        )
        .await;
    assert!(st.is_success(), "{body}");

    let mut spec = vm_spec();
    spec["network_interfaces"] = json!([{"network_id": net_id}]);
    let (st, v) = h
        .post("/vms", json!({"name": "egress-vm", "spec": spec}))
        .await;
    assert!(st.is_success(), "{v}");

    // The flag and the rule both have to arrive: the flag alone denies
    // everything, the rule alone changes nothing.
    for _ in 0..120 {
        if let Some((deny, rules)) = agent.lock().unwrap().egress_seen {
            if deny && rules == 1 {
                return;
            }
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!(
        "the agent never saw an enforced egress policy; last was {:?}",
        agent.lock().unwrap().egress_seen
    );
}

/// Per-disk storage policy (design §20): a ceiling that means something, a
/// zero that is refused, and a spec that says nothing being left exactly alone.
#[tokio::test]
async fn storage_policy_is_carried_or_refused_but_never_guessed() {
    let h = Harness::start().await;

    // Zero is not "unlimited", it is "this disk may never do any I/O" — almost
    // certainly a field left at its numeric default.
    let mut zero = vm_spec();
    zero["disks"] = json!([{
        "path": "/x/a.raw", "image_type": "raw",
        "policy": {"iops": 0},
    }]);
    let (st, body) = h.post("/vms", json!({"name": "zero", "spec": zero})).await;
    assert_eq!(st.as_u16(), 400, "{body}");
    assert!(format!("{body}").contains("disks[0].policy"), "{body}");

    // A real policy survives the round trip through the database.
    let mut throttled = vm_spec();
    throttled["disks"] = json!([{
        "path": "/x/a.raw", "image_type": "raw",
        "policy": {"cache": "direct", "allocation": "thick", "iops": 2000},
    }]);
    let (st, v) = h
        .post("/vms", json!({"name": "throttled", "spec": throttled}))
        .await;
    assert!(st.is_success(), "{v}");
    let vm = h
        .get(&format!("/vms/{}", v["vm_id"].as_str().unwrap()))
        .await;
    let policy = &vm["spec"]["disks"][0]["policy"];
    assert_eq!(policy["cache"], "direct", "{vm}");
    assert_eq!(policy["allocation"], "thick", "{vm}");
    assert_eq!(policy["iops"], 2000, "{vm}");
    // Untouched dimensions stay unsaid rather than becoming a stored zero.
    assert!(policy.get("bandwidth_bytes_per_sec").is_none(), "{policy}");

    // And a disk that says nothing about policy still says nothing after a
    // round trip: an existing fleet's specs do not grow keys on upgrade.
    let mut plain = vm_spec();
    plain["disks"] = json!([{"path": "/x/b.raw", "image_type": "raw"}]);
    let (st, v) = h
        .post("/vms", json!({"name": "plain", "spec": plain}))
        .await;
    assert!(st.is_success(), "{v}");
    let vm = h
        .get(&format!("/vms/{}", v["vm_id"].as_str().unwrap()))
        .await;
    assert!(
        vm["spec"]["disks"][0].get("policy").is_none(),
        "a policy appeared on a disk that never asked for one: {vm}"
    );
}

/// A pool declares whether its bytes are shared, and every rule that assumed
/// "reported ⇒ reachable" has to change with it (ADR-025).
#[tokio::test]
async fn local_storage_pins_a_vm_and_says_so() {
    let h = Harness::start_with(&[(
        "VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR",
        "/x/shared/volumes",
    )])
    .await;

    let (st, local) = h
        .post(
            "/storage-pools",
            json!({"name": "nvme", "kind": "local_dir", "path": "/x/nvme"}),
        )
        .await;
    assert!(st.is_success(), "{local}");
    let local_id = local["id"].as_str().unwrap().to_string();
    assert_eq!(local["sharing"], "local");
    assert!(
        local["sharing_note"]
            .as_str()
            .unwrap_or_default()
            .contains("cannot be live-migrated"),
        "{local}"
    );
    // The seeded default is shared, and says so — the two are told apart by
    // the pool, never by the path.
    let pools = h.get("/storage-pools").await;
    let default = pools
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "default")
        .unwrap()
        .clone();
    assert_eq!(default["sharing"], "shared");

    // A volume is built by the control plane, which cannot reach another
    // host's disk. Refused, naming what to do instead.
    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "v", "size_bytes": 1024, "pool": "nvme"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
    assert!(format!("{body}").contains("local to each host"), "{body}");

    // Two hosts, both reporting both pools.
    let (pa, pb) = (free_port(), free_port());
    let a = spawn_agent("hostA", pa);
    let _b = spawn_agent("hostB", pb);
    let host_a = h.register_host("hostA", pa).await;
    let host_b = h.register_host("hostB", pb).await;

    // Capacity: a shared pool is one filesystem seen twice, a local one is two
    // filesystems. Reporting either the same way is wrong by a factor of N.
    let ready = h
        .wait_for(
            &format!("/storage-pools/{local_id}"),
            |p| p["reachable_hosts"] == json!(2),
            "both hosts reporting the local pool",
        )
        .await;
    assert_eq!(ready["capacity_bytes"], json!(FAKE_POOL_CAPACITY * 2));
    let shared = h
        .get(&format!(
            "/storage-pools/{}",
            default["id"].as_str().unwrap()
        ))
        .await;
    assert_eq!(shared["reachable_hosts"], json!(2));
    assert_eq!(shared["capacity_bytes"], json!(FAKE_POOL_CAPACITY));

    // A VM with a disk in the local pool schedules normally — the file does not
    // exist yet, so any reporting host will do.
    let mut spec = vm_spec();
    spec["disks"] = json!([{
        "path": "/x/nvme/vm.qcow2", "image_type": "qcow2", "pool": local_id,
    }]);
    let (st, v) = h
        .post("/vms", json!({"name": "pinned", "spec": spec}))
        .await;
    assert!(st.is_success(), "{v}");
    let vm_id = v["vm_id"].as_str().unwrap().to_string();
    let vm = h
        .wait_for(
            &format!("/vms/{vm_id}"),
            |v| v["phase"] == "Running",
            "the pinned VM running",
        )
        .await;
    let placed_on = vm["host_id"].as_str().unwrap().to_string();
    let elsewhere = if placed_on == host_a {
        &host_b
    } else {
        &host_a
    };

    // …and then it cannot leave. The other host reports the pool, so the
    // ADR-023 reachability check passes — this refusal is a different one, and
    // without it the guest would start on an empty disk.
    let (st, body) = h
        .post(
            &format!("/vms/{vm_id}/migrate"),
            json!({"target_host_id": elsewhere}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
    assert!(format!("{body}").contains("local to its host"), "{body}");
    assert!(
        format!("{body}").contains("nvme"),
        "the pool is named: {body}"
    );

    // A drain says the same thing rather than reporting it as no capacity.
    let (st, drain) = h
        .post(&format!("/hosts/{placed_on}/drain"), json!({}))
        .await;
    assert!(st.is_success(), "{drain}");
    let skipped = drain["skipped"]
        .as_array()
        .map(|s| {
            s.iter().any(|x| {
                x["vm_id"] == json!(vm_id)
                    && x["reason"]
                        .as_str()
                        .unwrap_or_default()
                        .contains("local storage")
            })
        })
        .unwrap_or(false);
    assert!(skipped, "drain did not name the pin: {drain}");
    let _ = a;
}

/// A volume in a local pool is built by the host that owns the disk, and pins
/// every VM that attaches it to that host (ADR-025).
#[tokio::test]
async fn a_local_volume_is_built_on_its_host_and_pins_what_attaches_it() {
    let h = Harness::start_with(&[(
        "VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR",
        "/x/shared/volumes",
    )])
    .await;
    let (st, pool) = h
        .post(
            "/storage-pools",
            json!({"name": "nvme", "kind": "local_dir", "path": "/x/nvme"}),
        )
        .await;
    assert!(st.is_success(), "{pool}");

    let (pa, pb) = (free_port(), free_port());
    let a = spawn_agent("hostA", pa);
    let b = spawn_agent("hostB", pb);
    let host_a = h.register_host("hostA", pa).await;
    let host_b = h.register_host("hostB", pb).await;
    h.wait_for(
        &format!("/storage-pools/{}", pool["id"].as_str().unwrap()),
        |p| p["reachable_hosts"] == json!(2),
        "both hosts reporting the local pool",
    )
    .await;

    // A local volume has to name its host: nothing else has chosen one, and the
    // choice pins every VM that later attaches it.
    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "scratch", "size_bytes": 1048576, "pool": "nvme"}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
    assert!(format!("{body}").contains("has to name the host"), "{body}");

    // And on shared storage, naming one would record a fact that is not true.
    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "shared", "size_bytes": 1048576, "host": host_a}),
        )
        .await;
    assert_eq!(st.as_u16(), 400, "{body}");
    assert!(format!("{body}").contains("is shared"), "{body}");

    // The host must actually report the pool — asked of the agents, not of the
    // operator's belief.
    let ghost = "77777777-7777-4777-8777-777777777777";
    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "v", "size_bytes": 1048576, "pool": "nvme", "host": ghost}),
        )
        .await;
    assert_eq!(st.as_u16(), 404, "{body}");

    // Built where the bytes are: hostA is asked, hostB is not.
    let (st, vol) = h
        .post(
            "/volumes",
            json!({"name": "scratch", "size_bytes": 1048576, "pool": "nvme", "host": host_a}),
        )
        .await;
    assert!(st.is_success(), "{vol}");
    let vol_id = vol["id"].as_str().unwrap().to_string();
    assert_eq!(vol["host_id"], json!(host_a), "{vol}");
    let built = a.lock().unwrap().volumes_built.clone();
    assert_eq!(built.len(), 1, "hostA was not asked to build it: {built:?}");
    assert!(
        built[0].contains(&vol_id) && built[0].starts_with("/x/nvme/"),
        "{built:?}"
    );
    assert!(
        b.lock().unwrap().volumes_built.is_empty(),
        "the wrong host was asked to build it"
    );

    // Attaching it pins the VM: the other host reports the pool, but its disk
    // does not have this volume on it.
    let (st, v) = h
        .post("/vms", json!({"name": "attached", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let vm_id = v["vm_id"].as_str().unwrap().to_string();
    h.wait_for(
        &format!("/vms/{vm_id}"),
        |v| v["phase"] == "Running",
        "the VM running before the volume is attached",
    )
    .await;
    let (st, att) = h
        .post(
            &format!("/volumes/{vol_id}/attach"),
            json!({"vm_id": vm_id}),
        )
        .await;
    assert!(st.is_success(), "{att}");
    let vm = h.get(&format!("/vms/{vm_id}")).await;
    let disk = vm["spec"]["disks"]
        .as_array()
        .and_then(|d| d.last())
        .cloned()
        .unwrap();
    assert_eq!(disk["pinned_host"], json!(host_a), "{vm}");

    // Deleting it asks the host that has the bytes — sending this to the
    // control plane's own filesystem would leave every local volume behind.
    // (Detach needs the VM off: Cloud Hypervisor has no disk hot-unplug.)
    assert!(h
        .post(&format!("/vms/{vm_id}/stop"), json!({}))
        .await
        .0
        .is_success());
    h.wait_for(
        &format!("/vms/{vm_id}"),
        |v| v["phase"] == "Stopped",
        "the VM stopped so its volume can be detached",
    )
    .await;
    let (st, det) = h
        .post(&format!("/volumes/{vol_id}/detach"), json!({}))
        .await;
    assert!(st.is_success(), "{det}");
    assert_eq!(h.delete(&format!("/volumes/{vol_id}")).await.as_u16(), 204);
    let removed = a.lock().unwrap().volumes_removed.clone();
    assert_eq!(
        removed.len(),
        1,
        "hostA was not asked to remove it: {removed:?}"
    );
    assert!(removed[0].contains(&vol_id), "{removed:?}");
    let _ = host_b;
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

/// Quotas are admission control on committed intent (ADR-019): a resource
/// counts from the moment its row exists, the check happens in the transaction
/// that persists the intent, and the reconcile loop never sees a quota.
#[tokio::test]
async fn quotas_refuse_at_admission_and_count_committed_intent() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_TENANCY__ENABLED", "true")]).await;
    let default = "00000000-0000-0000-0000-000000000001";

    // No quota row is not a quota of zero — every project that predates quotas
    // has none, and this migration must not make a cluster start refusing work.
    let (st, v) = h
        .post("/vms", json!({"name": "unlimited", "spec": vm_spec()}))
        .await;
    assert!(
        st.is_success(),
        "a project without a quota is unlimited: {v}"
    );
    let first = v["vm_id"].as_str().unwrap().to_string();

    // vm_spec() asks for 1 vCPU and 512 MiB. Cap at two VMs' worth.
    let (st, q) = h
        .put(
            &format!("/projects/{default}/quota"),
            json!({"max_vms": 2, "max_vcpus": 2, "max_memory_mib": 1024}),
        )
        .await;
    assert!(st.is_success(), "{q}");
    assert_eq!(q["usage"]["vms"], 1, "the VM created above already counts");
    assert_eq!(q["usage"]["memory_mib"], 512);
    assert_eq!(q["over_quota"], false);

    let (st, v) = h
        .post("/vms", json!({"name": "second", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "the second VM fits exactly: {v}");

    let (st, body) = h
        .post("/vms", json!({"name": "third", "spec": vm_spec()}))
        .await;
    assert_eq!(st.as_u16(), 409, "the third must be refused: {body}");
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED");
    // The arithmetic is the point: "over quota" alone sends an operator to the
    // database to work out which limit and by how much.
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("vms"), "{msg}");
    assert!(msg.contains("limit 2"), "{msg}");

    // Nothing was persisted by the refusal.
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM virtual_machines")
            .await,
        "2"
    );

    // An in-place edit is admission too, on the difference it makes.
    let (st, body) = h
        .patch_body(&format!("/vms/{first}"), json!({"max_vcpus": 8}))
        .await;
    assert_eq!(st, 409, "growing a VM past the cap is refused: {body}");
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED");

    // Shrinking is always admissible, even from over quota — otherwise
    // lowering a limit would trap a project with no way down.
    let (st, body) = h
        .patch_body(&format!("/vms/{first}"), json!({"memory_mib": 256}))
        .await;
    assert_eq!(st, 202, "shrinking is always admissible: {body}");

    // Lowering a limit below current usage is permitted and non-destructive:
    // it blocks new commitments and reports as over quota.
    let (st, q) = h
        .put(&format!("/projects/{default}/quota"), json!({"max_vms": 1}))
        .await;
    assert!(st.is_success(), "{q}");
    assert_eq!(q["over_quota"], true);
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM virtual_machines")
            .await,
        "2",
        "lowering a quota must not delete anything"
    );

    // A VM still counts while it is being deleted — the row is what commits.
    let (st, _) = h.post(&format!("/vms/{first}/stop"), json!({})).await;
    assert!(st.is_success());

    // Clearing the quota returns the project to unlimited.
    assert_eq!(h.delete(&format!("/projects/{default}/quota")).await, 204);
    let (st, v) = h
        .post("/vms", json!({"name": "after-clear", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
}

/// A volume is reserved before its file is built, so the expensive work never
/// happens for a request that will not fit (ADR-019).
#[tokio::test]
async fn a_volume_is_admitted_before_it_is_provisioned() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_TENANCY__ENABLED", "true")]).await;
    let default = "00000000-0000-0000-0000-000000000001";

    let (st, _) = h
        .put(
            &format!("/projects/{default}/quota"),
            json!({"max_storage_bytes": 1_000_000}),
        )
        .await;
    assert!(st.is_success());

    let (st, body) = h
        .post(
            "/volumes",
            json!({"name": "too-big", "size_bytes": 2_000_000, "format": "qcow2"}),
        )
        .await;
    assert_eq!(st.as_u16(), 409, "refused before qemu-img runs: {body}");
    assert_eq!(body["error"]["code"], "QUOTA_EXCEEDED");
    // No row, and therefore no reservation left behind to leak the quota.
    assert_eq!(h.query_one("SELECT count(*)::text FROM volumes").await, "0");
}

/// Task and event feeds are scoped, and platform work belongs to no project
/// (design §47). A tenant watching "what is happening" must not be watching the
/// fleet.
#[tokio::test]
async fn task_and_event_feeds_are_scoped_and_platform_work_is_not_a_tenants() {
    let h = Harness::start_with(&[("VQUASAR_CONTROL_TENANCY__ENABLED", "true")]).await;
    let (st, blue) = h.post("/projects", json!({"name": "blue"})).await;
    assert!(st.is_success(), "{blue}");
    let blue = blue["id"].as_str().unwrap().to_string();

    let port = free_port();
    let _agent = spawn_agent("hostA", port);
    h.register_host("hostA", port).await;

    let (st, vm) = h
        .post_in(&blue, "/vms", json!({"name": "blue-vm", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{vm}");
    let task = vm["task_id"].as_str().unwrap().to_string();

    // The VM's task carries the VM's project.
    assert_eq!(
        h.query_one(&format!(
            "SELECT project_id::text FROM tasks WHERE id='{task}'"
        ))
        .await,
        blue
    );

    // Every task today names a VM, so the platform case is exercised through
    // the derivation itself: with no VM to inherit from it resolves to NULL,
    // rather than falling back to the default project and putting platform work
    // in a tenant's feed.
    h.sql(
        "INSERT INTO tasks (id, task_type, vm_id, project_id, created_at, updated_at)
         VALUES (gen_random_uuid(), 'host.drain', NULL,
                 (SELECT project_id FROM virtual_machines WHERE id IS NULL),
                 now(), now())",
    )
    .await;
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM tasks WHERE project_id IS NULL")
            .await,
        "1",
        "platform work belongs to no project"
    );

    // blue sees its own task and nothing else.
    let tasks = h.get_in(&blue, "/tasks").await;
    let tasks = tasks.as_array().unwrap();
    assert_eq!(
        tasks.len(),
        1,
        "a project sees only its own tasks: {tasks:?}"
    );
    assert_eq!(tasks[0]["id"], task);

    // The platform view sees both.
    assert_eq!(
        h.get_in("*", "/tasks").await.as_array().unwrap().len(),
        2,
        "platform scope sees platform work too"
    );

    // Naming another project's task by id answers as an unknown id would.
    let (st, _) = h.get_status_in("*", &format!("/tasks/{task}")).await;
    assert_eq!(st, 200);
    let default = "00000000-0000-0000-0000-000000000001";
    let (st, _) = h.get_status_in(default, &format!("/tasks/{task}")).await;
    assert_eq!(st, 404, "another project's task must not be readable by id");

    // Events follow the resource they describe. Host events belong to nobody.
    assert!(
        h.query_one(
            "SELECT count(*)::text FROM events
              WHERE resource_type='host' AND project_id IS NOT NULL"
        )
        .await
            == "0",
        "a host event must not be stamped with a project"
    );
    let blue_events = h.get_in(&blue, "/events").await;
    let blue_events = blue_events.as_array().unwrap();
    assert!(
        blue_events
            .iter()
            .all(|e| e["resource_type"] == "vm" || e["resource_type"] == "volume"),
        "a tenant's feed must not carry fleet events: {blue_events:?}"
    );
    assert!(
        h.get_in("*", "/events")
            .await
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["resource_type"] == "host"),
        "the platform view does carry them"
    );
}

/// Work that was in flight when the process died is reclaimed at startup.
///
/// Nothing else ever clears a transitional row: the detached task that owns it
/// dies with the process. A stuck `provisioning` volume is the worse of the two
/// — it holds quota for a file that will never exist (design §7, ADR-019).
#[tokio::test]
async fn in_flight_work_is_reclaimed_after_a_restart() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_TENANCY__ENABLED", "true")]).await;
    let default = "00000000-0000-0000-0000-000000000001";

    // Stand in for the states a killed process leaves behind. Creating them
    // through the API would mean racing a real download to kill it mid-flight;
    // the row is what the sweep acts on, and this is exactly the row.
    h.sql(
        "INSERT INTO images
            (id, name, source_path, format, boot, default_size_bytes, cloud_init, os,
             status, managed, created_at, updated_at)
         VALUES (gen_random_uuid(), 'half-downloaded', '/x/img.qcow2', 'qcow2',
                 '{\"type\":\"firmware\",\"firmware\":\"/x/CLOUDHV.fd\"}'::jsonb,
                 NULL, false, NULL, 'importing', TRUE, now(), now())",
    )
    .await;
    h.sql(
        "INSERT INTO volumes
            (id, name, size_bytes, format, project_id, status, created_at, updated_at)
         VALUES (gen_random_uuid(), 'half-provisioned', 500, 'qcow2',
                 '00000000-0000-0000-0000-000000000001', 'provisioning', now(), now())",
    )
    .await;

    // The reservation counts against quota while it exists — that is the point
    // of reserving, and the reason a stuck one has to be reclaimed.
    let (st, q) = h.get_status(&format!("/projects/{default}/quota")).await;
    assert_eq!(st, 200, "{q}");
    assert_eq!(q["usage"]["volumes"], 1);
    assert_eq!(q["usage"]["storage_bytes"], 500);

    h.restart().await;

    // The image is failed, and says why rather than sitting in `importing`.
    let images = h.get("/images").await;
    let orphan = images
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "half-downloaded")
        .expect("the image row survives; only its status changes");
    assert_eq!(orphan["status"], "failed");
    assert!(
        orphan["error"]
            .as_str()
            .unwrap_or_default()
            .contains("restart"),
        "the failure should name its cause: {orphan}"
    );

    // The reservation is gone, and with it the quota it was holding.
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM volumes WHERE status='provisioning'")
            .await,
        "0"
    );
    let (_, q) = h.get_status(&format!("/projects/{default}/quota")).await;
    assert_eq!(
        q["usage"]["volumes"], 0,
        "the reclaimed reservation frees quota"
    );
    assert_eq!(q["usage"]["storage_bytes"], 0);

    // A ready volume is untouched: the sweep must reclaim orphans, not
    // everything that happens to be a volume.
    h.sql(
        "INSERT INTO volumes
            (id, name, size_bytes, format, project_id, status, created_at, updated_at)
         VALUES (gen_random_uuid(), 'finished', 100, 'qcow2',
                 '00000000-0000-0000-0000-000000000001', 'ready', now(), now())",
    )
    .await;
    h.restart().await;
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM volumes WHERE name='finished'")
            .await,
        "1",
        "a ready volume must survive a restart"
    );
}

/// Two control planes over one database (design §48, ADR-021).
///
/// The claim is not "two processes start" — it is that exactly one of them runs
/// the controllers, both serve the API, and leadership moves when the holder
/// goes away. Each of those is checked separately, because each fails
/// differently.
#[tokio::test]
async fn two_control_planes_share_one_database_and_one_leader() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    let peer = h.start_peer("beta").await;

    // Exactly one leader, and both instances name the same one — the answer
    // comes from the database, not from whichever instance was asked.
    let a = h.get("/leader").await;
    let b = peer.get("/leader").await;
    assert_eq!(
        a["leader"]["holder"], b["leader"]["holder"],
        "the two instances must agree on who leads: {a} vs {b}"
    );
    assert_eq!(a["leader"]["valid"], true);
    assert_eq!(
        h.query_one("SELECT count(*)::text FROM controller_lease")
            .await,
        "1",
        "there is one lease row, ever"
    );

    // Each instance knows whether it is the one.
    let leader_is_alpha = a["is_self"].as_bool().unwrap();
    assert_eq!(
        leader_is_alpha,
        !b["is_self"].as_bool().unwrap(),
        "exactly one instance may believe it is the leader"
    );

    // The property that actually matters, and the one the lease exists for:
    // only the holder *runs the controllers*. Read from each instance's own
    // gauge rather than from the shared row, because a broken lease that let
    // both instances act would still leave a single, agreed-upon row — the two
    // would simply take it from each other. Sampled repeatedly: a lease that
    // ping-pongs shows up as both instances reporting 1 at some point.
    let mut leaders_seen = 0;
    for _ in 0..8 {
        let a_leads = leading(&h.metrics().await);
        let b_leads = leading(&peer.metrics().await);
        assert!(
            !(a_leads && b_leads),
            "two instances must never both be running the controllers"
        );
        if a_leads || b_leads {
            leaders_seen += 1;
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
    assert!(
        leaders_seen > 0,
        "somebody has to be running the controllers"
    );

    // Both serve the API regardless of who leads: the API is active/active, and
    // a standby answering 503 would make half the fleet useless.
    let (st, v) = h
        .post("/vms", json!({"name": "ha-vm", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "the API works on this instance: {v}");
    assert_eq!(
        peer.get("/vms").await.as_array().unwrap().len(),
        1,
        "and the other instance sees the same state"
    );

    // Leadership moves. Killing the holder frees the lease — on a clean stop it
    // is handed back rather than waiting out the TTL.
    let holder = a["leader"]["holder"].as_str().unwrap().to_string();
    let epoch_before = a["leader"]["epoch"].as_i64().unwrap();
    drop(peer);
    h.restart().await;

    let after = h
        .wait_for(
            "/leader",
            |v| v["leader"]["valid"] == true,
            "a live leader after both were bounced",
        )
        .await;
    assert_eq!(after["leader"]["holder"], "alpha");
    // Alpha reclaiming its own lease is a renewal, not a new term; beta taking
    // over from alpha would have been. Either way the epoch never goes
    // backwards, which is what makes it usable as a fencing token later.
    assert!(
        after["leader"]["epoch"].as_i64().unwrap() >= epoch_before,
        "epoch must be monotonic (was {epoch_before}, now {})",
        after["leader"]["epoch"]
    );
    assert!(!holder.is_empty());
}

/// A restart reclaims its *own* orphaned work and leaves another instance's
/// alone (ADR-021). Without the owner column, bringing one instance back would
/// kill a download another instance was still running.
#[tokio::test]
async fn a_restart_reclaims_only_its_own_in_flight_work() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;

    // One row each: alpha's, beta's, and one from before the column existed.
    for (name, owner) in [
        ("alphas", "'alpha'"),
        ("betas", "'beta'"),
        ("legacy", "NULL"),
    ] {
        h.sql(&format!(
            "INSERT INTO images
                (id, name, source_path, format, boot, default_size_bytes, cloud_init, os,
                 status, managed, owner, created_at, updated_at)
             VALUES (gen_random_uuid(), '{name}', '/x/{name}.qcow2', 'qcow2',
                     '{{\"type\":\"firmware\",\"firmware\":\"/x/CLOUDHV.fd\"}}'::jsonb,
                     NULL, false, NULL, 'importing', TRUE, {owner}, now(), now())"
        ))
        .await;
    }

    h.restart().await;

    let by_name = |v: &Value, name: &str| -> String {
        v.as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == name)
            .unwrap()["status"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let images = h.get("/images").await;
    assert_eq!(by_name(&images, "alphas"), "failed", "its own orphan");
    assert_eq!(
        by_name(&images, "betas"),
        "importing",
        "another instance's live import must survive"
    );
    // A row with no owner predates the column, so it can only have been written
    // by a binary that is no longer running.
    assert_eq!(by_name(&images, "legacy"), "failed");
}

/// A clean stop hands the lease back, so a peer takes over in a renewal
/// interval instead of waiting out the TTL (ADR-021).
///
/// This is here because it was not true: `shutdown_signal` waited only on
/// Ctrl-C, and systemd sends SIGTERM — so under the unit that actually ships,
/// the graceful path never ran at all. A failover on the lab took the full 15s
/// rather than the ~1s a clean handover should cost, which is how it was found.
#[tokio::test]
async fn a_clean_stop_hands_the_lease_back() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    h.wait_for("/leader", |v| v["leader"]["valid"] == true, "a leader")
        .await;
    assert_eq!(h.get("/leader").await["leader"]["holder"], "alpha");

    h.stop_gracefully().await;

    // The lease is expired *immediately*, not merely expiring: a peer starting
    // now would take it on its first tick rather than waiting out the TTL.
    let expired = h
        .query_one("SELECT (expires_at <= now())::text FROM controller_lease")
        .await;
    assert_eq!(
        expired, "true",
        "a graceful stop must release the lease rather than leave it to time out"
    );
}

/// A reconcile that cannot succeed stops being invisible (#35).
///
/// Found on a two-node lab: a leader killed mid-create left the VM in
/// `Scheduling` for as long as anyone watched — ~130 attempts over 400s — with
/// `message` NULL and nothing in the API to say anything was wrong. Retrying
/// forever is right for a transient hiccup and wrong for residue that makes
/// every attempt fail identically.
#[tokio::test]
async fn a_reconcile_that_cannot_succeed_ends_in_failed_and_says_why() {
    let h = Harness::start().await;
    let port = free_port();
    let agent = spawn_agent("hostA", port);
    h.register_host("hostA", port).await;

    // The agent now fails ensure the way an interrupted create does: the same
    // error, every time.
    agent.lock().unwrap().ensure_error =
        Some("cloud-hypervisor API returned 500: VM is already created".into());

    let (st, v) = h
        .post("/vms", json!({"name": "wedged", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let id = v["vm_id"].as_str().unwrap().to_string();

    // It gives up rather than retrying for ever, and lands somewhere terminal.
    let vm = h
        .wait_for(&format!("/vms/{id}"), |v| v["phase"] == "Failed", "Failed")
        .await;

    // And it says what happened — the agent's own error, not a generic one.
    let msg = vm["message"].as_str().unwrap_or_default();
    assert!(msg.contains("VM is already created"), "{msg}");
    assert!(msg.contains("reconcile failed"), "{msg}");

    // An operator watching events sees it too, at error severity.
    let events = h.get("/events").await;
    assert!(
        events
            .as_array()
            .unwrap()
            .iter()
            .any(|e| { e["event_type"] == "vm.reconcile_failed" && e["severity"] == "error" }),
        "the give-up should raise an event: {events}"
    );
}

/// Recovery clears the count, so a VM that fails a few times and then succeeds
/// has not spent any of its budget.
#[tokio::test]
async fn a_recovered_reconcile_forgets_its_failures() {
    let h = Harness::start().await;
    let port = free_port();
    let agent = spawn_agent("hostA", port);
    h.register_host("hostA", port).await;

    agent.lock().unwrap().ensure_error = Some("agent restarting".into());
    let (st, v) = h
        .post("/vms", json!({"name": "flaky", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let id = v["vm_id"].as_str().unwrap().to_string();

    // Let it fail at least once, then let the agent recover.
    h.wait_for(
        &format!("/vms/{id}"),
        |v| {
            v["message"]
                .as_str()
                .unwrap_or_default()
                .contains("restarting")
        },
        "a recorded failure",
    )
    .await;
    agent.lock().unwrap().ensure_error = None;

    h.wait_for(
        &format!("/vms/{id}"),
        |v| v["phase"] == "Running",
        "Running",
    )
    .await;
    assert_eq!(
        h.query_one(&format!(
            "SELECT reconcile_failures::text FROM virtual_machines WHERE id='{id}'"
        ))
        .await,
        "0",
        "a success must reset the consecutive-failure count"
    );
}

/// The case that broke on the lab, in CI: a leader killed *during* a create.
///
/// The window is the point. A real create takes seconds and a fake one takes
/// microseconds, so the agent holds `ensure_vm` open long enough for the leader
/// to be stopped inside it — otherwise this test would pass by never
/// interrupting anything (#35).
#[tokio::test]
async fn a_create_interrupted_by_a_failover_is_finished_by_the_peer() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    let port = free_port();
    let agent = spawn_agent("hostA", port);
    h.register_host("hostA", port).await;

    let peer = h.start_peer("beta").await;
    h.wait_for(
        "/leader",
        |v| v["leader"]["holder"] == "alpha",
        "alpha leading",
    )
    .await;

    // Widen the create window so the kill lands inside it.
    agent.lock().unwrap().ensure_delay_ms = 3_000;

    let (st, v) = h
        .post("/vms", json!({"name": "interrupted", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let id = v["vm_id"].as_str().unwrap().to_string();

    // Wait until an ensure is actually in flight, then kill the leader. Killing
    // before the first call would test a failover, not an *interrupted* one.
    for _ in 0..100 {
        if agent.lock().unwrap().ensure_calls > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        agent.lock().unwrap().ensure_calls > 0,
        "the leader should have started a create before it is interrupted"
    );
    // The create must still be *unfinished* when the leader dies, or this test
    // is watching an ordinary failover rather than an interrupted create. This
    // is what makes the injected window load-bearing instead of decorative.
    let phase = h.get(&format!("/vms/{id}")).await["phase"].clone();
    assert_ne!(
        phase, "Running",
        "the create completed before the interruption; the injected window is not working"
    );
    h.stop_gracefully().await;

    // The peer takes over and finishes the job, with nobody intervening.
    peer.wait_until_leader().await;
    agent.lock().unwrap().ensure_delay_ms = 0;
    peer.wait_for(
        &format!("/vms/{id}"),
        |v| v["phase"] == "Running",
        "Running",
    )
    .await;
}

// ---------------------------------------------------------------------------
// Live migration under interruption (#42).
//
// ADR-021 singles migration out: every other pass converges, so a duplicated
// action is wasted work, but `prepare_receive` does not — two calls mean two
// receivers for one guest. That is the stated justification for epoch fencing,
// and until now nobody had interrupted a migration to see what actually
// happens. Each step of the state machine gets a case, and the question is the
// same every time: after the peer takes over, is the control plane's account of
// where the VM lives still true?
// ---------------------------------------------------------------------------

/// Two hosts, a VM running on A, and a standby ready to take over. Returns the
/// host ids, the agent states and the VM id.
async fn migration_fixture(
    h: &Harness,
) -> (
    String,
    String,
    Arc<Mutex<AgentState>>,
    Arc<Mutex<AgentState>>,
    String,
) {
    let (pa, pb) = (free_port(), free_port());
    let sa = spawn_agent("hostA", pa);
    let sb = spawn_agent("hostB", pb);
    let a = h.register_host("hostA", pa).await;
    let b = h.register_host("hostB", pb).await;

    // Cordon B so the VM lands on A, then uncordon it as a migration target.
    assert!(h
        .patch(&format!("/hosts/{b}"), json!({"schedulable": false}))
        .await
        .is_success());
    let (st, v) = h
        .post("/vms", json!({"name": "mig-interrupt", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "create vm: {st} {v}");
    let vm = v["vm_id"].as_str().unwrap().to_string();
    h.wait_for(
        &format!("/vms/{vm}"),
        |v| v["phase"] == "Running",
        "VM Running on A",
    )
    .await;
    assert!(h
        .patch(&format!("/hosts/{b}"), json!({"schedulable": true}))
        .await
        .is_success());
    (a, b, sa, sb, vm)
}

/// Does this agent report the guest as actually running? Holding a record is
/// not the same thing — a sent-away source still has one.
fn running_here(state: &Arc<Mutex<AgentState>>, vm: &str) -> bool {
    matches!(state.lock().unwrap().vms.get(vm), Some(Phase::Running))
}

/// Wait until an RPC has arrived at the fake agent, so the leader is stopped
/// *inside* a step rather than before it.
async fn wait_for_call(state: &Arc<Mutex<AgentState>>, count: impl Fn(&AgentState) -> u32) {
    for _ in 0..150 {
        if count(&state.lock().unwrap()) > 0 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the leader never reached the step this test means to interrupt");
}

/// `Pending`: the leader dies between `prepare_receive` succeeding on the target
/// and the `Sending` write that records it.
///
/// This is the case ADR-021 predicts, and it used to leave two receivers for one
/// guest. Note what fixed it: *not* epoch fencing. ADR-022 refuses a superseded
/// controller, and here the old leader is dead — the retry comes from the
/// legitimate current leader carrying a *higher* epoch, which the agent accepts
/// by design. At-most-once had to come from `prepare_receive` itself (#45).
#[tokio::test]
async fn a_migration_interrupted_inside_prepare_receive_leaves_one_receiver() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    let (_a, b, _sa, sb, vm) = migration_fixture(&h).await;
    let peer = h.start_peer("beta").await;
    h.wait_for(
        "/leader",
        |v| v["leader"]["holder"] == "alpha",
        "alpha leading",
    )
    .await;

    // Widen the step so the stop lands inside it.
    sb.lock().unwrap().prepare_delay_ms = 5_000;
    let (st, v) = h
        .post(&format!("/vms/{vm}/migrate"), json!({"target_host_id": b}))
        .await;
    assert!(st.is_success(), "migrate: {st} {v}");

    wait_for_call(&sb, |s| s.prepare_calls).await;
    // The step must still be unfinished, or this test is watching an ordinary
    // failover rather than an interrupted migration.
    assert_eq!(
        h.query_one(&format!("SELECT state FROM migrations WHERE vm_id='{vm}'"))
            .await,
        "Pending",
        "prepare_receive finished before the interruption; the injected window is not working"
    );
    h.stop_gracefully().await;

    peer.wait_until_leader().await;
    sb.lock().unwrap().prepare_delay_ms = 0;
    h.wait_sql(
        &format!("SELECT state FROM migrations WHERE vm_id='{vm}'"),
        |s| s == "Completed" || s == "Failed",
        "the migration to reach a terminal state",
    )
    .await;

    // Either outcome is defensible — finish the migration, or fail it and leave
    // the VM on its source. What is not defensible is two receivers.
    //
    // The assertion is on receivers, not calls. The peer retrying is correct:
    // it reads `Pending` and has no way to know the step already ran, and epoch
    // fencing admits it because its lease epoch is *higher*. At-most-once has to
    // come from the agent, which is why this counts what the agent built.
    let st = sb.lock().unwrap();
    assert!(
        st.pending_receive.get(&vm).copied().unwrap_or(0) <= 1,
        "two receivers for one guest ({} on the target after {} calls)",
        st.pending_receive.get(&vm).copied().unwrap_or(0),
        st.prepare_calls
    );
}

/// `Sending`: the leader dies after the source has sent its state but before the
/// `Finalizing` write.
///
/// The guest is gone from the source at this point. Whatever the peer does, the
/// control plane must not end up claiming the VM runs somewhere it does not.
#[tokio::test]
async fn a_migration_interrupted_inside_send_does_not_strand_the_guest() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    let (a, b, sa, sb, vm) = migration_fixture(&h).await;
    let peer = h.start_peer("beta").await;
    h.wait_for(
        "/leader",
        |v| v["leader"]["holder"] == "alpha",
        "alpha leading",
    )
    .await;

    sa.lock().unwrap().send_delay_ms = 5_000;
    let (st, v) = h
        .post(&format!("/vms/{vm}/migrate"), json!({"target_host_id": b}))
        .await;
    assert!(st.is_success(), "migrate: {st} {v}");

    wait_for_call(&sa, |s| s.send_calls).await;
    assert_eq!(
        h.query_one(&format!("SELECT state FROM migrations WHERE vm_id='{vm}'"))
            .await,
        "Sending",
        "send_migration finished before the interruption; the injected window is not working"
    );
    h.stop_gracefully().await;

    peer.wait_until_leader().await;
    sa.lock().unwrap().send_delay_ms = 0;
    h.wait_sql(
        &format!("SELECT state FROM migrations WHERE vm_id='{vm}'"),
        |s| s == "Completed" || s == "Failed",
        "the migration to reach a terminal state",
    )
    .await;

    // The invariant: wherever the control plane says the VM is, that host has
    // to actually be running it.
    let vm_row = peer.get(&format!("/vms/{vm}")).await;
    let host = vm_row["host_id"].as_str().unwrap_or_default().to_string();
    let phase = vm_row["phase"].as_str().unwrap_or_default().to_string();
    let (on_a, on_b) = (running_here(&sa, &vm), running_here(&sb, &vm));
    assert!(
        !(on_a && on_b),
        "the guest is running on both hosts after an interrupted send"
    );
    if phase == "Running" {
        let claimed_here = if host == a { on_a } else { on_b };
        assert!(
            claimed_here,
            "the control plane reports the VM Running on {host} (A={a}, B={b}) but no host \
             is running it (A: {on_a}, B: {on_b})"
        );
    }
}

/// `Finalizing`: the leader dies after the target has adopted the guest but
/// before the write that moves the VM's host.
///
/// This used to end with the guest running on neither host. tonic drops a
/// handler future when its client disconnects, so the dying leader *cancelled*
/// the agent's finalise — and because `Manager::finalize_receive` took the entry
/// out of `pending` before its await, the cancelled call destroyed the receiver
/// on its way out and the peer's retry got `NotFound`. The finalise now runs in
/// a spawned task and is idempotent once the guest is adopted (#45).
#[tokio::test]
async fn a_migration_interrupted_inside_finalize_does_not_lose_the_vm() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    let (a, b, sa, sb, vm) = migration_fixture(&h).await;
    let peer = h.start_peer("beta").await;
    h.wait_for(
        "/leader",
        |v| v["leader"]["holder"] == "alpha",
        "alpha leading",
    )
    .await;

    sb.lock().unwrap().finalize_delay_ms = 5_000;
    let (st, v) = h
        .post(&format!("/vms/{vm}/migrate"), json!({"target_host_id": b}))
        .await;
    assert!(st.is_success(), "migrate: {st} {v}");

    wait_for_call(&sb, |s| s.finalize_calls).await;
    assert_eq!(
        h.query_one(&format!("SELECT state FROM migrations WHERE vm_id='{vm}'"))
            .await,
        "Finalizing",
        "finalize_receive finished before the interruption; the injected window is not working"
    );
    h.stop_gracefully().await;

    peer.wait_until_leader().await;
    sb.lock().unwrap().finalize_delay_ms = 0;
    h.wait_sql(
        &format!("SELECT state FROM migrations WHERE vm_id='{vm}'"),
        |s| s == "Completed" || s == "Failed",
        "the migration to reach a terminal state",
    )
    .await;

    let vm_row = peer.get(&format!("/vms/{vm}")).await;
    let host = vm_row["host_id"].as_str().unwrap_or_default().to_string();
    let (on_a, on_b) = (running_here(&sa, &vm), running_here(&sb, &vm));
    let (calls, completed) = {
        let s = sb.lock().unwrap();
        (s.finalize_calls, s.finalize_completed)
    };
    assert!(
        on_a || on_b,
        "the VM is running on neither host after an interrupted finalize \
         ({calls} finalize calls, {completed} of them completed)"
    );
    let claimed_here = if host == a { on_a } else { on_b };
    assert!(
        claimed_here,
        "the control plane reports the VM on {host} (A={a}, B={b}) but it is running \
         elsewhere (A: {on_a}, B: {on_b})"
    );
}

/// The wiring for ADR-022: does the control plane actually put its lease epoch
/// on the wire, and does it advance when leadership moves?
///
/// The comparison itself is unit-tested in the agent. This is the half that
/// cannot be — the half where the defect would live, because a stamp that is
/// never applied looks exactly like a fleet with nothing to fence.
#[tokio::test]
async fn every_agent_rpc_carries_the_controller_lease_epoch() {
    let mut h = Harness::start_with(&[("VQUASAR_CONTROL_SERVER__INSTANCE_ID", "alpha")]).await;
    let port = free_port();
    let agent = spawn_agent("hostA", port);
    h.register_host("hostA", port).await;
    let peer = h.start_peer("beta").await;
    h.wait_for(
        "/leader",
        |v| v["leader"]["holder"] == "alpha",
        "alpha leading",
    )
    .await;
    let alpha_epoch = h.get("/leader").await["leader"]["epoch"].as_i64().unwrap();

    let (st, v) = h
        .post("/vms", json!({"name": "stamped", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let id = v["vm_id"].as_str().unwrap().to_string();
    h.wait_for(
        &format!("/vms/{id}"),
        |v| v["phase"] == "Running",
        "Running",
    )
    .await;

    let seen = agent.lock().unwrap().epochs_seen.clone();
    assert!(!seen.is_empty(), "no ensure_vm reached the agent");
    assert!(
        seen.iter().all(|e| *e == Some(alpha_epoch)),
        "every call must carry the leader's epoch {alpha_epoch}, saw {seen:?}"
    );

    // Failover: the successor's epoch is strictly higher, which is what lets an
    // agent tell a new leader from a superseded one.
    h.stop_gracefully().await;
    peer.wait_until_leader().await;
    let beta_epoch = peer.get("/leader").await["leader"]["epoch"]
        .as_i64()
        .unwrap();
    assert!(
        beta_epoch > alpha_epoch,
        "the epoch must advance on takeover ({alpha_epoch} -> {beta_epoch})"
    );

    let (st, v) = peer
        .post("/vms", json!({"name": "stamped-two", "spec": vm_spec()}))
        .await;
    assert!(st.is_success(), "{v}");
    let id = v["vm_id"].as_str().unwrap().to_string();
    peer.wait_for(
        &format!("/vms/{id}"),
        |v| v["phase"] == "Running",
        "Running",
    )
    .await;

    let after = agent.lock().unwrap().epochs_seen.clone();
    let fresh = &after[seen.len()..];
    assert!(
        !fresh.is_empty() && fresh.iter().all(|e| *e == Some(beta_epoch)),
        "the peer must stamp its own epoch {beta_epoch}, saw {fresh:?}"
    );
}
