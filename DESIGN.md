# Cloud Hypervisor Orchestrator — Initial Design

**Status:** Early design / bootstrap specification
**Working name:** `vquasar`
**Primary language:** Rust
**VMM:** Cloud Hypervisor
**Initial networking:** Open vSwitch
**Future networking:** OVN + Open vSwitch
**UI:** React / TypeScript
**Control-plane database:** PostgreSQL

---

## 1. Purpose

Build a modern open-source virtualization management platform specifically
around Cloud Hypervisor. The system should provide functionality conceptually
comparable to the core infrastructure-management capabilities of vCenter,
oVirt, or Proxmox, while avoiding the legacy assumptions of those systems.

The platform should manage fleets of Linux hypervisor hosts running Cloud
Hypervisor and expose virtual machines as first-class managed resources. The
project must not depend on Kubernetes.

The long-term system should manage: hypervisor hosts; virtual machines; VM
lifecycle; CPU and memory placement; virtual networks; storage; images;
snapshots; live migration; hardware passthrough; GPUs; HA and fencing; host
maintenance; metrics and events; authentication and authorization.

The initial implementation should intentionally implement only a small vertical
slice of this architecture.

## 2. Product philosophy

The project is not intended to reproduce every feature of VMware, QEMU,
libvirt, or oVirt. Cloud Hypervisor has intentionally narrower assumptions than
traditional virtualization platforms. The orchestrator should preserve that
philosophy.

Primary principles:

1. Cloud Hypervisor is the VMM.
2. The orchestrator talks directly to Cloud Hypervisor.
3. libvirt is not part of the architecture.
4. Kubernetes is not part of the architecture.
5. Everything except the web frontend should preferably be Rust.
6. Desired state and observed state are separate.
7. Components should reconcile state rather than depend on long imperative workflows.
8. Hardware capabilities must be explicit and schedulable.
9. Networking and storage must eventually be pluggable.
10. Host compromise should not automatically imply control-plane compromise.
11. Cloud Hypervisor processes should run with as few privileges as possible.
12. The public API must be usable independently of the UI.
13. Every asynchronous operation must be represented as an observable task.

## 3. Initial system architecture

```
                         ┌───────────────────────────┐
                         │          Web UI           │
                         │    React / TypeScript     │
                         └─────────────┬─────────────┘
                                       │
                              REST / WebSocket
                                       │
                ┌──────────────────────▼─────────────────────┐
                │              Control Plane                 │
                │  REST API / VM Controller / Host Controller │
                │  Scheduler / Task Engine / Network Controller│
                │  Storage Controller / Migration Controller  │
                └───────────┬────────────────┬───────────────┘
                            │                │
                      PostgreSQL          gRPC/mTLS
                                             │
                    ┌────────────────────────┼─────────────────────┐
             ┌──────▼───────┐        ┌──────▼───────┐     ┌──────▼───────┐
             │  Host Agent  │        │  Host Agent  │     │  Host Agent  │
             └──────┬───────┘        └──────────────┘     └──────────────┘
                    │
          ┌─────────┼──────────┐
          ▼         ▼          ▼
        Cloud      OVS      Storage
      Hypervisor
```

Two Rust executables initially: `vquasar-control` and `vquasar-agent`. Avoid unnecessary
microservices.

## 4. Repository layout

Start with a Rust Cargo workspace.

```
vquasar/
├── Cargo.toml / Cargo.lock / README.md / DESIGN.md / LICENSE
├── rust-toolchain.toml / deny.toml
├── .github/workflows/
├── crates/
│   ├── common/  model/  client/  host/  network/
│   ├── storage/ scheduler/ api/ proto/
├── services/control/  services/agent/
├── ui/
├── proto/agent.proto
├── migrations/  config/  scripts/
├── test/integration/  test/fixtures/
└── docs/
```

Do not create dozens of tiny crates initially. Keep boundaries meaningful.

## 5. Rust technology choices

Runtime `tokio`; REST `axum`; middleware `tower`/`tower-http`; serialization
`serde`/`serde_json`; database `sqlx` + PostgreSQL; gRPC `tonic`/`prost`;
tracing `tracing`; metrics prometheus-compatible; UUIDs `uuid`; errors
`thiserror` + `anyhow`; CLI `clap`; HTTP client `reqwest`/`hyper`; Unix sockets
tokio `UnixStream`; configuration `figment`/`config`.

Business-domain logic should not depend directly on Axum, SQLx, or Tonic where
practical.

## 6. Core domain model

Initial resources: `Host`, `VirtualMachine`, `Network`, `NetworkInterface`,
`Volume`, `Disk`, `Image`, `Task`, `Event`.

Later: `Snapshot`, `Migration`, `Device`, `Gpu`, `PlacementPolicy`, `Tenant`,
`User`, `Role`, `Cluster`, `StoragePool`.

Every persistent resource has: `id`, `name`, `created_at`, `updated_at`,
`generation`. Reconciled resources additionally separate `spec` and `status`.

```rust
pub struct VirtualMachine {
    pub id: Uuid,
    pub name: String,
    pub spec: VirtualMachineSpec,
    pub status: VirtualMachineStatus,
    pub generation: i64,
}
```

VM phases: `Pending`, `Scheduling`, `Creating`, `Stopped`, `Starting`,
`Running`, `Stopping`, `Migrating`, `Failed`, `Deleting`.

Do not store Cloud Hypervisor's raw configuration as the primary API model.

## 7. Desired state versus observed state

A VM has desired state (e.g. `desired_power_state = Running`) and observed state
(e.g. `phase = Stopped`). The system detects the difference and reconciles it.
Persist desired state first, then reconcile — do not model the control plane as
"API request → execute RPC → return success", which is fragile after crashes.

## 8. Control-plane responsibilities

`vquasar-control` owns global intent: persistent API objects, public REST API,
scheduling, desired VM state, network/storage definitions, host registration
and availability, task lifecycle, events, orchestration workflows (auth later).

It must **not** launch Cloud Hypervisor directly, configure TAP devices,
modify OVS on remote machines, manipulate VFIO, or mount host filesystems
remotely. Those are host-agent responsibilities.

## 9. Host agent

`vquasar-agent` runs once on every virtualization host and is the local authority.

**Host discovery** reports: hostname, machine UUID, architecture, kernel
version, Cloud Hypervisor version, logical CPUs, CPU topology/model/features,
NUMA nodes, total/available memory, hugepages, network interfaces, OVS bridges,
storage availability, PCI devices, IOMMU groups, VFIO capability, GPU inventory,
SEV-SNP / TDX capability. Not every field must exist in v1, but the model should
allow them.

**VM lifecycle**: the agent manages the CH process, API socket, serial socket,
PID, runtime directory, logs, cgroup, TAP devices, OVS ports and attached
devices.

Example runtime layout:

```
/run/vquasar/vms/<vm-uuid>/
  ├── api.sock  serial.sock  pid  config.json  metadata.json
```

Persistent host state should not depend solely on `/run`.

## 10. Cloud Hypervisor integration

Dedicated crate `crates/client` exposes a `Hypervisor` trait for testing,
mocking and isolating CH version changes (not for supporting QEMU). The real
implementation talks directly to the CH API socket; CH processes are launched by
the agent. Do not shell out to `ch-remote` for normal operation.

## 11. Process supervision

The agent must recover its VM inventory after restart: inspect runtime state and
running CH processes, connect to known API sockets, compare with control-plane
assignments, reconstruct and report observed state. **VM processes must survive
agent restarts** — restarting `vquasar-agent` must not kill VMs.

## 12. Agent/control-plane protocol

Use gRPC (`proto/agent.proto`). Initial RPCs: `GetHostInfo`, `GetVm`, `ListVms`,
`EnsureVm`, `StartVm`, `StopVm`, `DeleteVm`. Eventually add streams for host
heartbeat, events, console, metrics, migration progress, task progress. Do not
expose this API publicly.

## 13. Host registration

For development, configure the control-plane address manually. Eventually
implement secure enrollment (agent identity → registration token → CSR →
control-plane approval → certificate → mTLS). Do not implement PKI enrollment in
phase one, but design the protocol so mTLS can be inserted without redesign.

## 14. Public API

Versioned HTTP REST under `/api/v1`:

```
GET/POST      /api/v1/vms         GET/DELETE /api/v1/vms/{id}
POST          /api/v1/vms/{id}/start|stop|reboot
GET           /api/v1/hosts       GET /api/v1/hosts/{id}
GET           /api/v1/tasks       GET /api/v1/tasks/{id}
GET           /api/v1/events
GET/POST      /api/v1/networks    GET/DELETE /api/v1/networks/{id}
```

## 15. Asynchronous operations

Long operations (create, delete, migrate, snapshot) return `{ vm_id, task_id }`
rather than holding the connection. Task states: `Pending`, `Running`,
`Succeeded`, `Failed`, `Cancelled`. Tasks produce events.

## 16. Event model

Events are first-class objects (`host.registered`, `vm.created`, `vm.started`,
`migration.completed`, …) with `id`, `timestamp`, `resource_type`,
`resource_id`, `event_type`, `severity`, `message`, `metadata`. The event stream
later drives auditing and UI updates.

## 17. Scheduler

Initial scheduler is deliberately simple: filter unavailable hosts → filter on
memory → filter on CPU → score remaining → prefer host with largest available
memory percentage → assign. Represent scheduling as two concepts, `filter` and
`score`, structured so a plugin framework can be added later (do not build the
framework now). Future filters: CPU arch/model/features, NUMA, hugepages, GPU,
VFIO, SR-IOV, SEV-SNP, TDX, network/storage reachability, (anti-)affinity.

## 18. Networking

Split into network model, network controller, and host dataplane. MVP uses Open
vSwitch with an integration bridge `br-int`; initial types are provider bridge
and VLAN (VLAN may be postponed). Agent responsibilities: create TAP, attach to
OVS, set VLAN if requested, start CH with the TAP, and clean up on deletion.
Define a `NetworkBackend` trait; the initial implementation is
`OvsNetworkBackend`. Prefer OVSDB over CLI parsing where practical.

## 19. OVN

OVN is post-MVP: logical switches/routers, distributed routing, DHCP, ACLs,
tenant isolation, Geneve overlays, provider connectivity. Our control plane acts
as the cloud-management system above OVN.

## 20. Storage

Storage must eventually be pluggable via a `StorageBackend` trait
(`create_volume`, `delete_volume`, `prepare_volume`, `release_volume`). Do not
implement distributed storage. Initial types: local file
(`/var/lib/vquasar/volumes/<uuid>.raw`) and shared path
(`/mnt/shared/...`, assumed identically mounted on participating hosts — enough
for early live-migration experiments). Future: LVM thin, NFS, Ceph RBD/CephFS,
iSCSI, NVMe-oF, SPDK, vhost-user-blk.

## 21. Images

Separate images from volumes. Image metadata eventually: name, architecture,
format, size, checksum, source, created_at. Initial implementation may assume
images already exist in a configured path; do not build a download service
before basic VM lifecycle works.

## 22. VM creation flow

```
POST /vms → persist spec → VM Controller → Scheduler (select host) →
persist assignment → Host Agent (prepare disk, TAP, OVS, runtime dir, launch CH,
wait for API socket, create VM, boot) → report observed state → status = Running
```

Every step should be idempotent where practical. Running reconciliation twice
must not create two VMs.

## 23. VM identity

The VM UUID is authoritative. Never identify VMs by PID, name, TAP name, or
host-local sequence. Derive host-local names from the UUID (e.g. `tap40340f77`),
respecting Linux interface-name length limits.

## 24. VM configuration

Initial VM definition supports CPU (`boot_vcpus`/`max_vcpus`), memory
(`size_mib`), boot (`kernel`/`initramfs`/`cmdline`), and network interfaces.
Direct-kernel boot is the initial developer workflow; disk boot follows shortly.

## 25. Serial console

A usable console is required early: browser terminal → WebSocket → vquasar-control →
gRPC → vquasar-agent → VM serial socket. No graphical console initially; frontend
uses `xterm.js`. Console auth may initially reuse the UI/API identity.

## 26. Heartbeats

Agents periodically report identity, timestamp, resource usage, VM inventory and
health (dev heartbeat ~5s). Host status: `Ready`, `NotReady`, `Maintenance`,
`Disabled`. A host becomes `NotReady` after missed heartbeats — but its VMs are
**not** restarted elsewhere (that needs fencing; see §27).

## 27. Fencing and HA

HA is out of scope for MVP. Never restart VMs elsewhere on lost heartbeat
without fencing. Future HA: detect failure → fence → verify fence → recover
storage → schedule elsewhere → start. Fencing providers (Redfish, IPMI, PDU,
cloud) become a provider interface when HA begins.

## 28. Live migration

Not required for the first lifecycle milestone but an early major milestone.
Assume shared storage initially. Model migration as a persisted task/state
machine (validate destination → reserve capacity → prepare networking → prepare
receiver → initiate → track → verify → update assignment → clean source), not a
single giant RPC handler.

## 29. GPU and VFIO architecture

Not MVP, but the data model must not preclude it. Host discovery eventually
identifies PCI address, vendor/device IDs, IOMMU group, NUMA node, driver, VFIO
availability, GPU model/UUID, SR-IOV capability. VMs may request a specific PCI
device or a device class; the scheduler resolves abstract requests to physical
devices. The agent owns privileged VFIO operations; CH runs unprivileged where
possible.

## 30. Security boundary

```
control plane ──mTLS──▶ privileged host agent ──▶ OVS / storage / VFIO
                                     │
                                     ▼ unprivileged CH process ──▶ VM
```

The API service must never accept arbitrary host shell commands. The agent
protocol exposes typed operations only — no `POST /agent/exec`.

## 31. Database

PostgreSQL. Initial tables: `hosts`, `virtual_machines`, `networks`, `volumes`,
`images`, `tasks`, `events`. SQLx migrations in `/migrations`. Avoid an ORM that
hides SQL; use explicit queries and transactions around state transitions.
Optimistic concurrency via generation/version columns eventually prevents
conflicting updates.

## 32. Controllers

Explicit controllers in `vquasar-control`: `HostController`, `VmController` (later
`NetworkController`, `VolumeController`, `MigrationController`, `HaController`).
A controller loop finds resources needing reconciliation and reconciles each.
Correctness must not depend exclusively on receiving an event — a periodic
reconciliation pass repairs missed events.

## 33. API/UI updates

Use WebSocket or SSE for live state (`GET /api/v1/events/stream`). The UI
subscribes rather than polling aggressively (initial implementation may poll).

## 34. Web UI

React + TypeScript + Vite (no Rust WASM initially). Pages: Dashboard; Hosts
(list/detail); Virtual Machines (list/create/detail/console); Networks; Tasks;
Events. VM list columns: Name, State, Host, vCPU, Memory, IP, Created. The
frontend must be API-only and never authoritative.

## 35. Observability

Structured tracing everywhere, including `request_id`, `task_id`, `vm_id`,
`host_id`. Prometheus metrics eventually: `agent_heartbeat_timestamp`,
`host_cpu_total`, `host_memory_total_bytes`, `host_memory_available_bytes`,
`host_vm_count`, `vm_state`, `task_total`, `task_failures_total`,
`api_request_duration_seconds`.

## 36. Configuration

TOML with environment-variable overrides. See `config/control.toml` and
`config/agent.toml`.

## 37. Error handling

Typed domain errors: `InsufficientResources`, `HostUnavailable`, `VmNotFound`,
`VmAlreadyRunning`, `NetworkUnavailable`, `StorageUnavailable`, `HypervisorError`,
`AgentUnavailable`, `InvalidConfiguration`. Do not expose raw internal errors
through the public API; return `{ error: { code, message, request_id } }`.

## 38. Testing strategy

First-class requirement. Unit tests for scheduler, domain validation, state
transitions, config translation, controller decisions. A `FakeHypervisor`
implements the same `Hypervisor` trait for controller/agent testing without KVM.
Integration tests use real Cloud Hypervisor when `/dev/kvm` exists (create, boot,
inspect, shutdown, delete, agent restart). Networking tests where privileged CI
is available.

## 39. Development environment

Primary target: Linux x86_64, KVM, Cloud Hypervisor v53+, Open vSwitch,
PostgreSQL. Developer mode runs `vquasar-control`, `vquasar-agent` and PostgreSQL on one
machine; a two-host lab is the next environment.

## 40. Explicit MVP scope

**Hosts:** start agent, register, report CPU/memory/CH version, heartbeat, show
through API. **VMs:** create, schedule, launch CH, direct-kernel boot, stop,
delete, query state, recover after agent restart. **Networking:** create TAP,
attach to OVS bridge, assign MAC, clean up on deletion. **UI:** host list, VM
list, create-VM form, VM detail, start/stop/delete, serial console if practical.

That is enough for MVP. Do not add more until it works reliably.

## 41. Explicit non-goals for MVP

Kubernetes, libvirt, QEMU, multi-tenancy, RBAC, OIDC, HA, automatic failover,
Redfish fencing, OVN, overlay networks, distributed storage, Ceph, backup,
snapshots, GPU passthrough, SR-IOV, SEV-SNP, TDX, DRS, automatic balancing,
Windows graphical console, Terraform provider, Ansible modules, multi-cluster
federation. Architect for these where appropriate; do not implement them.

## 42. Initial development milestones

* **M0 — Rust workspace:** workspace, `common`/`model`/`client` crates, agent
  and control services, CI (fmt, clippy, tests). Acceptance: `cargo build`,
  `cargo test`, `cargo clippy` all succeed on the workspace.
* **M1 — Cloud Hypervisor adapter:** launch VMM, connect socket, create/boot/
  info/shutdown/delete. Acceptance: a Linux VM boots and produces serial output.
* **M2 — Host agent:** host inventory, CH process manager, runtime dirs, VM
  inventory, gRPC API. Acceptance: a gRPC client drives VM lifecycle; restarting
  the agent does not terminate a running VM.
* **M3 — Control plane:** PostgreSQL, host/VM/task tables, REST API, agent
  comms, simple scheduler, basic reconciliation. Acceptance: `POST /api/v1/vms`
  creates a VM on a registered host.
* **M4 — OVS networking:** network model, TAP creation, OVS attachment, MAC
  allocation, cleanup. Acceptance: two VMs on the same provider network
  communicate.
* **M5 — Web interface:** host/VM lists, VM creation, lifecycle actions, task
  status. Acceptance: full lifecycle via UI without CLI/API.
* **M6 — Serial console:** WebSocket/gRPC console proxy.
* **M7 — Two-host scheduling.**
* **M8 — Shared-storage live migration** (only after the above is stable).

## 43. First implementation task

Start with Milestone 0 and the foundation for Milestone 1 only. Create the
workspace with `crates/common`, `crates/model`, `crates/client`,
`services/agent`, `services/control`. Compiles without PostgreSQL, OVS, Tonic or
the frontend. Define initial domain types (`VmId`, `HostId`,
`VirtualMachineSpec`, `CpuSpec`, `MemorySpec`, `BootSpec`, `VmPhase`,
`DesiredPowerState`).

In `vquasar-client`, define the `Hypervisor` trait (`create`, `boot`, `shutdown`,
`info`) with `CloudHypervisor` and `FakeHypervisor` implementations, separated
into process management, API client and configuration translation. Unit-test VM
spec validation, CH config translation and fake state transitions. Add CI
(rustfmt, clippy, cargo test, cargo deny). Model the CH client against the real
Cloud Hypervisor OpenAPI; keep CH-specific request/response types inside
`vquasar-client` (they must not leak into the domain model).

## 44. Architectural invariants (ADRs)

* **ADR-001** The control plane does not directly manage local host resources.
* **ADR-002** The host agent communicates directly with Cloud Hypervisor.
* **ADR-003** libvirt is not part of the execution path.
* **ADR-004** Kubernetes is not required.
* **ADR-005** Desired and observed VM state remain separate.
* **ADR-006** Persistent resource identity uses UUIDs.
* **ADR-007** Long-running operations are asynchronous tasks.
* **ADR-008** Networking is abstracted behind a backend interface.
* **ADR-009** Storage is abstracted behind a backend interface.
* **ADR-010** The initial network backend is Open vSwitch.
* **ADR-011** OVN is the preferred future distributed virtual-network control plane.
* **ADR-012** The initial storage architecture does not attempt distributed storage.
* **ADR-013** Cloud Hypervisor-specific types do not form the public orchestration API.
* **ADR-014** Host fencing is mandatory before automatic HA restart is introduced.
* **ADR-015** The web UI never becomes the authoritative source of infrastructure state.

## 45. Longer-term architecture

An HA control plane (API, controllers, scheduler, task engine, identity/RBAC)
over PostgreSQL and OVN, with host agents running CH/OVS/storage on each host.
The design target is a virtualization-native distributed control plane where
Cloud Hypervisor is not hidden behind legacy abstractions, exposing modern
hardware, networking and isolation capabilities as schedulable primitives.

## 46. Success criterion

Three physical Linux hosts (Cloud Hypervisor + vquasar-agent + OVS) under vquasar-control
with a web UI, from which one can: discover all three hosts; see CPU/memory;
create a Linux VM; have the scheduler choose a host; connect it to a virtual
network; access its serial console; stop and restart it; migrate it to another
host; and observe migration and resource state in real time.

At that point the project demonstrates its central proposition: Cloud Hypervisor
can serve as the foundation for a standalone, modern, distributed virtualization
platform without requiring libvirt or Kubernetes.
