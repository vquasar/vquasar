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

use std::collections::HashSet;
use std::net::IpAddr;

use crate::api::error::{ApiError, ApiResult};
use crate::authz::{AuthUser, RequireVmCreate, RequireVmUpdate};
use crate::ipam::Subnet;
use crate::netalloc::allocate_mac;
use crate::store::{Image, Store, Template, Vm};

/// Allocate and persist static IPs for a new VM's NICs on managed networks
/// (design M13a). NICs on unmanaged (DHCP) networks are skipped. A NIC may
/// request specific addresses; otherwise the lowest free address per configured
/// family is assigned. Idempotent per (network, ip) via the DB unique index.
async fn allocate_nic_ips(store: &Store, vm_id: Uuid, spec: &VirtualMachineSpec) -> ApiResult<()> {
    for (index, nic) in spec.network_interfaces.iter().enumerate() {
        allocate_one_nic(store, vm_id, nic, index).await?;
    }
    Ok(())
}

/// Allocate the static IP(s) for a single NIC on a managed network (M13a/M13d).
async fn allocate_one_nic(
    store: &Store,
    vm_id: Uuid,
    nic: &NetworkInterfaceSpec,
    index: usize,
) -> ApiResult<()> {
    {
        let network = store
            .get_network(nic.network_id.as_uuid())
            .await?
            .ok_or_else(|| ApiError::invalid(format!("network not found: {}", nic.network_id)))?;
        if !network.is_managed() {
            return Ok(());
        }
        let mac = nic.mac.clone().unwrap_or_else(|| allocate_mac(vm_id, index));

        // Parse operator-requested addresses (reject garbage early).
        let mut requested = Vec::new();
        for a in &nic.addresses {
            let ip: IpAddr = a
                .parse()
                .map_err(|_| ApiError::invalid(format!("invalid requested IP: {a}")))?;
            requested.push(ip);
        }

        for (cidr, gw, ps, pe, family) in [
            (
                network.cidr_v4.as_deref(),
                network.gateway_v4.as_deref(),
                network.pool_v4_start.as_deref(),
                network.pool_v4_end.as_deref(),
                4i16,
            ),
            (
                network.cidr_v6.as_deref(),
                network.gateway_v6.as_deref(),
                network.pool_v6_start.as_deref(),
                network.pool_v6_end.as_deref(),
                6i16,
            ),
        ] {
            let Some(subnet) = Subnet::parse_opt(cidr, gw, ps, pe)
                .map_err(|e| ApiError::invalid(e.to_string()))?
            else {
                continue;
            };
            let want_v6 = family == 6;
            let chosen = if let Some(req) = requested.iter().find(|ip| ip.is_ipv6() == want_v6) {
                subnet
                    .validate(*req)
                    .map_err(|e| ApiError::invalid(e.to_string()))?;
                *req
            } else {
                let taken: HashSet<IpAddr> = store
                    .allocations_for_network(network.id)
                    .await?
                    .iter()
                    .filter_map(|a| a.ip.parse().ok())
                    .collect();
                subnet
                    .next_free(&taken)
                    .map_err(|e| ApiError::invalid(e.to_string()))?
            };
            store
                .insert_allocation(network.id, &chosen.to_string(), family, Some(vm_id), index as i32, &mac)
                .await
                .map_err(|e| match &e {
                    sqlx::Error::Database(db) if db.is_unique_violation() => {
                        ApiError::invalid(format!("address {chosen} is already allocated"))
                    }
                    _ => e.into(),
                })?;
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct ChangeNic {
    /// The network to move this NIC onto.
    pub network_id: Uuid,
    /// Optionally replace the NIC's security groups at the same time.
    #[serde(default)]
    pub security_groups: Option<Vec<Uuid>>,
}

/// Move an existing NIC to a different network on a running VM (design M13d).
/// The agent re-homes the NIC's TAP on the dataplane (no VM restart); the guest
/// keeps its MAC and IP, so on a different subnet it must renew DHCP or be
/// reconfigured to use the new network at L3.
pub async fn change_nic(
    State(store): State<Store>,
    _: RequireVmUpdate,
    Path((id, index)): Path<(Uuid, usize)>,
    Json(body): Json<ChangeNic>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let vm = store
        .get_vm(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("vm not found: {id}")))?;
    let mut spec = vm.spec.0.clone();
    if index >= spec.network_interfaces.len() {
        return Err(ApiError::invalid(format!("nic index {index} out of range")));
    }
    store
        .get_network(body.network_id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("network not found: {}", body.network_id)))?;

    // Retarget the NIC. Drop any operator-requested static IPs — they belonged
    // to the old network — and re-allocate from the new one.
    {
        let nic = &mut spec.network_interfaces[index];
        nic.network_id = ch_model::NetworkId::from(body.network_id);
        nic.addresses.clear();
        if let Some(sgs) = &body.security_groups {
            nic.security_groups = sgs.clone();
        }
    }
    spec.validate()
        .map_err(|e| ApiError::invalid(e.to_string()))?;

    // Swap allocations: free the old NIC addresses, then allocate on the new net.
    store.release_nic_allocations(id, index as i32).await?;
    allocate_one_nic(&store, id, &spec.network_interfaces[index], index).await?;

    // Persist (bumps generation) so the reconciler re-homes the TAP.
    let vm = store
        .set_vm_spec(id, &spec)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("vm not found: {id}")))?;
    let task = store.insert_task("vm.nic.change", Some(vm.id)).await?;
    store
        .insert_event("vm", Some(vm.id), "vm.nic.changed", "info", &vm.name)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            vm_id: vm.id,
            task_id: task.id,
        }),
    ))
}

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
    _: RequireVmCreate,
    Json(body): Json<CreateVm>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    body.spec
        .validate()
        .map_err(|e| ApiError::invalid(e.to_string()))?;

    // Persist desired state first (section 7), then let reconciliation act.
    let vm = store.insert_vm(&body.name, &body.spec).await?;
    // Allocate static IPs for NICs on managed networks (M13a); roll back the VM
    // row if allocation fails so we never leave a half-provisioned VM.
    if let Err(e) = allocate_nic_ips(&store, vm.id, &vm.spec).await {
        let _ = store.delete_vm_row(vm.id).await;
        return Err(e);
    }
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
    /// Optional hot-plug memory cap (MiB) so the VM can grow memory live (M10).
    #[serde(default)]
    pub memory_max_mib: Option<u64>,
    #[serde(default)]
    pub disk_size_bytes: Option<u64>,
    #[serde(default)]
    pub network_id: Option<Uuid>,
    /// Security groups applied to the NIC (design M13c).
    #[serde(default)]
    pub security_groups: Vec<Uuid>,
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
    _: RequireVmCreate,
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
    if let Err(e) = allocate_nic_ips(&store, vm.id, &vm.spec).await {
        let _ = store.delete_vm_row(vm.id).await;
        return Err(e);
    }
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
                addresses: Vec::new(),
                security_groups: ov.security_groups.clone(),
            }]
        })
        .unwrap_or_default();

    // Cloud-init: override wins over template default; only attach when the
    // image expects a seed. Default the hostname to the VM name.
    // Attach cloud-init when the image expects a seed, or whenever the caller
    // explicitly supplied one (raw user-data must be honoured either way, M10).
    let cloud_init = if image.cloud_init || ov.cloud_init.is_some() {
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
            max_size_mib: ov.memory_max_mib,
        },
        boot: image.boot.0.clone(),
        disks,
        network_interfaces,
        placement: PlacementSpec::default(),
        cloud_init,
    }
}

/// Edit an existing VM (design M10). Each field is optional; provided ones are
/// applied to the spec and reconciliation hot-plugs what Cloud Hypervisor
/// supports, leaving the rest for the next restart.
#[derive(Debug, Default, Deserialize)]
pub struct UpdateVm {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub boot_vcpus: Option<u32>,
    #[serde(default)]
    pub max_vcpus: Option<u32>,
    #[serde(default)]
    pub memory_mib: Option<u64>,
    #[serde(default)]
    pub memory_max_mib: Option<u64>,
    /// Grow an existing disk (by index) to a new size in bytes.
    #[serde(default)]
    pub grow_disk: Option<GrowDisk>,
    /// Attach a new blank data disk.
    #[serde(default)]
    pub add_disk: Option<AddDisk>,
    /// Attach a new NIC on the given network.
    #[serde(default)]
    pub add_nic: Option<AddNic>,
}

#[derive(Debug, Deserialize)]
pub struct GrowDisk {
    pub index: usize,
    pub size_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct AddDisk {
    pub size_bytes: u64,
    #[serde(default)]
    pub image_type: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddNic {
    pub network_id: Uuid,
}

pub async fn update(
    State(store): State<Store>,
    _: RequireVmUpdate,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateVm>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    let vm = store
        .get_vm(id)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("vm not found: {id}")))?;
    let mut spec = vm.spec.0.clone();

    if let Some(v) = body.boot_vcpus {
        spec.cpu.boot_vcpus = v;
    }
    if let Some(v) = body.max_vcpus {
        spec.cpu.max_vcpus = v;
    }
    if let Some(m) = body.memory_mib {
        spec.memory.size_mib = m;
    }
    if body.memory_max_mib.is_some() {
        spec.memory.max_size_mib = body.memory_max_mib;
    }
    if let Some(g) = &body.grow_disk {
        let disk = spec
            .disks
            .get_mut(g.index)
            .ok_or_else(|| ApiError::invalid("disk index out of range"))?;
        if g.size_bytes < disk.size_bytes.unwrap_or(0) {
            return Err(ApiError::invalid("disks can only grow"));
        }
        disk.size_bytes = Some(g.size_bytes);
    }
    if let Some(a) = &body.add_disk {
        let image_type = if a.image_type.as_deref() == Some("raw") {
            DiskImageType::Raw
        } else {
            DiskImageType::Qcow2
        };
        let ext = match image_type {
            DiskImageType::Raw => "raw",
            DiskImageType::Qcow2 => "qcow2",
        };
        let path = PathBuf::from(store.shared_volumes_dir())
            .join(format!("{id}-d{}.{ext}", spec.disks.len()));
        spec.disks
            .push(DiskSpec::blank(path, image_type, a.size_bytes));
    }
    if let Some(n) = &body.add_nic {
        spec.network_interfaces.push(NetworkInterfaceSpec {
            network_id: ch_model::NetworkId::from(n.network_id),
            mac: None,
            addresses: Vec::new(),
            security_groups: Vec::new(),
        });
    }

    spec.validate()
        .map_err(|e| ApiError::invalid(e.to_string()))?;
    let vm = store
        .update_vm(id, body.name.as_deref(), &spec)
        .await?
        .ok_or_else(|| ApiError::invalid(format!("vm not found: {id}")))?;
    let task = store.insert_task("vm.update", Some(vm.id)).await?;
    store
        .insert_event("vm", Some(vm.id), "vm.updated", "info", &vm.name)
        .await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(Accepted {
            vm_id: vm.id,
            task_id: task.id,
        }),
    ))
}

pub async fn list(State(store): State<Store>, user: AuthUser) -> ApiResult<Json<Vec<Vm>>> {
    user.require("vm:read")?;
    Ok(Json(store.list_vms().await?))
}

pub async fn get(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Vm>> {
    user.require("vm:read")?;
    store
        .get_vm(id)
        .await?
        .map(Json)
        .ok_or_else(|| ApiError::vm_not_found(id))
}

pub async fn delete(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    user.require("vm:delete")?;
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
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    user.require("vm:power")?;
    set_power(&store, id, DesiredPowerState::Running, "vm.start").await
}

pub async fn stop(
    State(store): State<Store>,
    user: AuthUser,
    Path(id): Path<Uuid>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    user.require("vm:power")?;
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
    user: AuthUser,
    Path(id): Path<Uuid>,
    Json(body): Json<MigrateRequest>,
) -> ApiResult<(StatusCode, Json<Accepted>)> {
    user.require("vm:migrate")?;
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
