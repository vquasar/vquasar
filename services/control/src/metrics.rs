//! Prometheus metrics (design M17, observability). A `/metrics` endpoint
//! exposes operational telemetry beyond the events table: current inventory
//! (VMs by phase, hosts by state, tasks by state, in-flight migrations) plus
//! reconcile / migration / recovery / HTTP counters.
//!
//! Inventory gauges are refreshed from the store each reconcile tick (cheap,
//! low-frequency) rather than queried on every scrape. Counters are incremented
//! inline where the events happen (see `reconcile`).

use std::collections::HashMap;
use std::time::Instant;

use axum::extract::{MatchedPath, Request};
use axum::middleware::Next;
use axum::response::Response;
use metrics::{counter, describe_counter, describe_gauge, describe_histogram, gauge, histogram};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};

use crate::store::Store;

/// All phases a VM can report, so a gauge that drops to zero is published as 0
/// rather than going stale at its last value.
const VM_PHASES: &[&str] = &[
    "Pending",
    "Scheduling",
    "Creating",
    "Stopped",
    "Starting",
    "Running",
    "Stopping",
    "Migrating",
    "Failed",
    "Deleting",
];
const HOST_STATES: &[&str] = &["Ready", "NotReady", "Maintenance", "Disabled"];
const TASK_STATES: &[&str] = &["Pending", "Running", "Succeeded", "Failed", "Cancelled"];

/// Install the global Prometheus recorder and return a handle for rendering.
pub fn install() -> anyhow::Result<PrometheusHandle> {
    let handle = PrometheusBuilder::new()
        .install_recorder()
        .map_err(|e| anyhow::anyhow!("install prometheus recorder: {e}"))?;
    describe_gauge!("vquasar_vms", "Virtual machines by phase");
    describe_gauge!("vquasar_hosts", "Hosts by state");
    describe_gauge!("vquasar_hosts_schedulable", "Hosts currently schedulable");
    describe_gauge!("vquasar_tasks", "Tasks by state");
    describe_gauge!("vquasar_migrations_active", "In-flight live migrations");
    describe_counter!(
        "vquasar_reconcile_passes_total",
        "Reconcile loop iterations"
    );
    describe_counter!(
        "vquasar_reconcile_errors_total",
        "Reconcile pass errors by pass"
    );
    describe_counter!("vquasar_migrations_total", "Finished migrations by result");
    describe_counter!(
        "vquasar_vm_recoveries_total",
        "VMs re-launched after host recovery"
    );
    describe_counter!(
        "vquasar_http_requests_total",
        "HTTP requests by method/path/status"
    );
    describe_histogram!(
        "vquasar_http_request_duration_seconds",
        "HTTP request latency (s)"
    );
    Ok(handle)
}

/// Refresh the inventory gauges from the store (called each reconcile tick).
pub async fn update_from_store(store: &Store) -> anyhow::Result<()> {
    let mut vms: HashMap<&str, f64> = VM_PHASES.iter().map(|p| (*p, 0.0)).collect();
    for v in store.list_vms().await? {
        *vms.entry_or_other(&v.phase, VM_PHASES) += 1.0;
    }
    for (phase, n) in &vms {
        gauge!("vquasar_vms", "phase" => *phase).set(*n);
    }

    let mut hosts: HashMap<&str, f64> = HOST_STATES.iter().map(|s| (*s, 0.0)).collect();
    let mut schedulable = 0.0;
    for h in store.list_hosts().await? {
        *hosts.entry_or_other(&h.state, HOST_STATES) += 1.0;
        if h.schedulable {
            schedulable += 1.0;
        }
    }
    for (state, n) in &hosts {
        gauge!("vquasar_hosts", "state" => *state).set(*n);
    }
    gauge!("vquasar_hosts_schedulable").set(schedulable);

    let mut tasks: HashMap<&str, f64> = TASK_STATES.iter().map(|s| (*s, 0.0)).collect();
    for t in store.list_tasks().await? {
        *tasks.entry_or_other(&t.state, TASK_STATES) += 1.0;
    }
    for (state, n) in &tasks {
        gauge!("vquasar_tasks", "state" => *state).set(*n);
    }

    gauge!("vquasar_migrations_active").set(store.list_active_migrations().await?.len() as f64);
    Ok(())
}

/// Small helper: map an observed label to one of a known set, bucketing
/// anything unexpected under "Other" so a typo can't create unbounded series.
trait EntryOrOther {
    fn entry_or_other(&mut self, key: &str, known: &[&'static str]) -> &mut f64;
}
impl EntryOrOther for HashMap<&'static str, f64> {
    fn entry_or_other(&mut self, key: &str, known: &[&'static str]) -> &mut f64 {
        let label = known
            .iter()
            .find(|k| **k == key)
            .copied()
            .unwrap_or("Other");
        self.entry(label).or_insert(0.0)
    }
}

/// axum middleware: count requests and record latency, labelled by the matched
/// route template (not the concrete path) to keep cardinality bounded.
pub async fn track_http(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_string();
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_string())
        .unwrap_or_else(|| "<unmatched>".to_string());
    let start = Instant::now();
    let resp = next.run(req).await;
    let elapsed = start.elapsed().as_secs_f64();
    let status = resp.status().as_u16().to_string();
    counter!("vquasar_http_requests_total", "method" => method, "path" => path.clone(), "status" => status)
        .increment(1);
    histogram!("vquasar_http_request_duration_seconds", "path" => path).record(elapsed);
    resp
}
