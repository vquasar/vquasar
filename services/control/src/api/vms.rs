//! Virtual-machine endpoints (design document, sections 14, 15, 22).
//!
//! Writes persist desired state and return a task id; the reconcile loop does
//! the actual work asynchronously (section 15). The API never blocks on the
//! agent.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use ch_model::{DesiredPowerState, VirtualMachineSpec};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::store::{Store, Vm};

#[derive(Debug, Deserialize)]
pub struct CreateVm {
    pub name: String,
    pub spec: VirtualMachineSpec,
}

/// Async-operation acknowledgement (section 15).
#[derive(Debug, Serialize)]
pub struct Accepted {
    pub vm_id: Uuid,
    pub task_id: Uuid,
}

pub async fn create(
    State(store): State<Store>,
    Json(body): Json<CreateVm>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    body.spec
        .validate()
        .map_err(|e| ApiError::invalid(e.to_string()))?;

    // Persist desired state first (section 7), then let reconciliation act.
    let vm = store.insert_vm(&body.name, &body.spec).await?;
    let task = store.insert_task("vm.create", Some(vm.id)).await?;
    store
        .insert_event("vm", Some(vm.id), "vm.created", "info", &vm.name)
        .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            vm_id: vm.id,
            task_id: task.id,
        }),
    ))
}

pub async fn list(State(store): State<Store>) -> ApiResult<Json<Vec<Vm>>> {
    Ok(Json(store.list_vms().await?))
}

pub async fn get(State(store): State<Store>, Path(id): Path<Uuid>) -> ApiResult<Json<Vm>> {
    store
        .get_vm(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::vm_not_found(id))
}

pub async fn delete(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let vm = store
        .get_vm(id)
        .await?
        .ok_or_else(|| ApiError::vm_not_found(id))?;
    // Mark for deletion; the reconcile loop tears it down on the agent.
    store.set_vm_phase(id, "Deleting").await?;
    let task = store.insert_task("vm.delete", Some(id)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            vm_id: vm.id,
            task_id: task.id,
        }),
    ))
}

pub async fn start(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    set_power(&store, id, DesiredPowerState::Running, "vm.start").await
}

pub async fn stop(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    set_power(&store, id, DesiredPowerState::Stopped, "vm.stop").await
}

/// Update a VM's desired power state and queue a task; reconciliation applies it.
async fn set_power(
    store: &Store,
    id: Uuid,
    power: DesiredPowerState,
    task_type: &str,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let vm = store
        .get_vm(id)
        .await?
        .ok_or_else(|| ApiError::vm_not_found(id))?;
    let mut spec = vm.spec.0.clone();
    spec.desired_power_state = power;
    store.set_vm_spec(id, &spec).await?;
    let task = store.insert_task(task_type, Some(id)).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            vm_id: id,
            task_id: task.id,
        }),
    ))
}
