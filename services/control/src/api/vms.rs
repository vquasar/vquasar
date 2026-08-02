//! Virtual-machine endpoints (design document, sections 14, 15, 22).
//!
//! Writes persist desired state and return a task id; the reconcile loop does
//! the actual work asynchronously (section 15). The API never blocks on the
//! agent.

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use ch_model::{
    CloudInitSpec, CpuSpec, DesiredPowerState, DiskImageType, DiskSpec, MemorySpec,
    NetworkInterfaceSpec, PlacementSpec, VirtualMachineSpec,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::error::{ApiError, ApiResult};
use crate::store::{Image, Store, Template, Vm};

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

/// Overrides applied on top of a template when instantiating a VM (design M9).
#[derive(Debug, Default, Deserialize)]
pub struct TemplateOverrides {
    #[serde(default)]
    pub boot_vcpus: Option<u32>,
    #[serde(default)]
    pub max_vcpus: Option<u32>,
    #[serde(default)]
    pub memory_mib: Option<u64>,
    #[serde(default)]
    pub disk_size_bytes: Option<u64>,
    #[serde(default)]
    pub network_id: Option<Uuid>,
    #[serde(default)]
    pub cloud_init: Option<CloudInitSpec>,
}

#[derive(Debug, Deserialize)]
pub struct CreateVmFromTemplate {
    pub name: String,
    pub template_id: Uuid,
    #[serde(default)]
    pub overrides: TemplateOverrides,
}

/// Create a VM from a template: the control plane assembles a full spec (boot
/// recipe from the image, a provisioned volume on shared storage, optional NIC
/// and cloud-init seed), then reconciliation provisions and launches it.
pub async fn create_from_template(
    State(store): State<Store>,
    Json(body): Json<CreateVmFromTemplate>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    if body.name.is_empty() {
        return Err(ApiError::invalid("name is required"));
    }
    let template = store
        .get_template(body.template_id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("template not found: {}", body.template_id)))?;
    let image = store
        .get_image(template.image_id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("image not found: {}", template.image_id)))?;

    // Pick the id up front so the provisioned volume path can reference it.
    let vm_id = Uuid::new_v4();
    let spec = build_spec_from_template(
        vm_id,
        &body.name,
        &template,
        &image,
        &body.overrides,
        store.shared_volumes_dir(),
    );
    spec.validate()
        .map_err(|e| ApiError::invalid(e.to_string()))?;

    let vm = store.insert_vm_with_id(vm_id, &body.name, &spec).await?;
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

/// Assemble a [`VirtualMachineSpec`] from a template + image + overrides.
fn build_spec_from_template(
    vm_id: Uuid,
    name: &str,
    template: &Template,
    image: &Image,
    ov: &TemplateOverrides,
    shared_volumes_dir: &str,
) -> VirtualMachineSpec {
    let image_type = if template.disk_format == "raw" {
        DiskImageType::Raw
    } else {
        DiskImageType::Qcow2
    };
    let ext = match image_type {
        DiskImageType::Raw => "raw",
        DiskImageType::Qcow2 => "qcow2",
    };
    let volume_path = PathBuf::from(shared_volumes_dir).join(format!("{vm_id}.{ext}"));
    let size_bytes = ov
        .disk_size_bytes
        .or_else(|| template.disk_size_bytes.map(|s| s as u64))
        .or_else(|| image.default_size_bytes.map(|s| s as u64));

    // One provisioned data disk; the cloud-init seed is generated and appended
    // by the agent from `cloud_init` (kept off the control-plane spec so paths
    // stay agent-owned).
    let disks = vec![DiskSpec::provisioned(
        volume_path,
        image_type,
        image.source_path.clone(),
        size_bytes,
    )];

    let network_interfaces = ov
        .network_id
        .or(template.network_id)
        .map(|network_id| {
            vec![NetworkInterfaceSpec {
                network_id: ch_model::NetworkId::from(network_id),
                mac: None,
            }]
        })
        .unwrap_or_default();

    // Cloud-init: override wins over template default; only attach when the
    // image expects a seed. Default the hostname to the VM name.
    let cloud_init = if image.cloud_init {
        let mut ci = ov
            .cloud_init
            .clone()
            .or_else(|| template.cloud_init.as_ref().map(|c| c.0.clone()))
            .unwrap_or(CloudInitSpec {
                hostname: None,
                ssh_authorized_keys: vec![],
                password: None,
                user_data: None,
            });
        if ci.hostname.is_none() {
            ci.hostname = Some(name.to_string());
        }
        Some(ci)
    } else {
        None
    };

    VirtualMachineSpec {
        desired_power_state: DesiredPowerState::Running,
        cpu: CpuSpec {
            boot_vcpus: ov.boot_vcpus.unwrap_or(template.boot_vcpus as u32),
            max_vcpus: ov.max_vcpus.unwrap_or(template.max_vcpus as u32),
        },
        memory: MemorySpec {
            size_mib: ov.memory_mib.unwrap_or(template.memory_mib as u64),
        },
        boot: image.boot.0.clone(),
        disks,
        network_interfaces,
        placement: PlacementSpec::default(),
        cloud_init,
    }
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

#[derive(Debug, Deserialize)]
pub struct MigrateRequest {
    pub target_host_id: Uuid,
}

/// Request a live migration of a running VM to another host (section 28). The
/// migration controller drives it asynchronously.
pub async fn migrate(
    State(store): State<Store>,
    Path(id): Path<Uuid>,
    Json(body): Json<MigrateRequest>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let vm = store
        .get_vm(id)
        .await?
        .ok_or_else(|| ApiError::vm_not_found(id))?;

    if vm.phase != "Running" {
        return Err(ApiError::invalid("only a running VM can be migrated"));
    }
    let source_host_id = vm
        .host_id
        .ok_or_else(|| ApiError::invalid("VM is not placed on a host"))?;
    if source_host_id == body.target_host_id {
        return Err(ApiError::invalid("target host is the VM's current host"));
    }
    let target = store
        .get_host(body.target_host_id)
        .await?
        .ok_or_else(|| ApiError::host_not_found(body.target_host_id))?;
    if target.state != "Ready" || !target.schedulable {
        return Err(ApiError::invalid("target host is not Ready/schedulable"));
    }
    if store.active_migration_for_vm(id).await?.is_some() {
        return Err(ApiError::invalid(
            "a migration is already in progress for this VM",
        ));
    }

    let task = store.insert_task("vm.migrate", Some(id)).await?;
    store
        .insert_migration(id, Some(source_host_id), body.target_host_id, task.id)
        .await?;
    store.set_vm_phase(id, "Migrating").await?;
    store
        .insert_event("vm", Some(id), "migration.requested", "info", &vm.name)
        .await?;

    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            vm_id: id,
            task_id: task.id,
        }),
    ))
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
