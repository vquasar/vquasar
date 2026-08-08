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

Storage is organised around **pools** (ADR-023): a pool is a named place to put
bytes, a volume belongs to exactly one, and which hosts can reach a pool is
*observed from the agents* rather than declared. The scheduler refuses a host
that does not report the pools a VM's disks need, so a missing mount is a
placement refusal with a reason instead of a launch failure.

The initial kind is `shared_dir` — a path assumed mounted on the hosts that
report it, which is what live migration has always depended on. Planned kinds:
LVM thin, NFS, Ceph RBD/CephFS, iSCSI, NVMe-oF, SPDK, vhost-user-blk. Do not
implement distributed storage.

This supersedes the original sketch here, which proposed a `StorageBackend`
trait in the agent as the first move. See ADR-023 for why the pool came first:
the trait puts an abstraction where the plugin goes, and the problem was the
undeclared assumption about who can see what.

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

The browser is inside this boundary too: it holds an operator's bearer token and
talks to the control plane as that operator. So the control plane serves the
console itself, from its own origin, under a content-security policy that is
`'self'` throughout — the one cross-origin destination is the OIDC issuer, which
the sign-in flow contacts directly. Fonts, styles and scripts all ship in the
bundle; the console loads nothing from a third party, which keeps it working in
an air-gapped lab and keeps a CDN from observing an operator at work. Responses
also carry `nosniff`, `no-referrer`, `frame-ancestors 'none'` and a blanket
denial of device permissions.

The console's permission checks are UX, never enforcement: every endpoint is
guarded server-side regardless. What the UI owes is honesty — it hides an action
the caller cannot perform, and issues no query for a resource the caller cannot
read, so a scoped role sees a coherent console instead of a wall of 403s.

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

Until the stream exists, polling is tiered rather than flat. The console shell
carries live counts for every resource, so every open browser holds a query on
every list: a single interval for all of them means fetching the entire
inventory every few seconds, per operator, forever. Instead, hosts, VMs, tasks
and events poll at 2s only while a task is running or a VM is in a transitional
phase and at 10s otherwise; images speed up only during an import; and resources
that change only when somebody acts poll at 60s and are invalidated by their own
mutations. This is a stopgap with the right shape — when the event stream lands
it replaces the fast tier, and the slow tier stays as the reconciliation
backstop.

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
* **ADR-016** A network's type declares its isolation guarantee, and the platform
  owns segment identifiers.
* **ADR-017** Every guest port carries an explicit dataplane policy.
* **ADR-018** The project is the unit of tenancy, enforced in the control plane.
* **ADR-019** Quotas are admission control on committed intent.
* **ADR-020** A role binding names the project it applies in; the request's
  project is resolved, never believed.
* **ADR-021** The control plane is active/active for the API and single-leader
  for the controllers, elected by a lease row in PostgreSQL.
* **ADR-022** The agent rejects a superseded controller by lease epoch.
* **ADR-023** A storage pool is a named place to put bytes, and reachability is
  observed rather than declared.

### ADR-016 — A network's type declares its isolation guarantee

*Status:* Accepted.

*Context.* A `Network` row was an IPAM record. Every network without a VLAN or
VNI shared one untagged L2 domain on `br-int`, so two "different" networks were
the same broadcast domain; and any caller holding `network:create` could choose
its own 802.1Q tag or VXLAN VNI — reaching whatever provider segment the trunk
carried, or joining an existing overlay. Yet some networks legitimately *are*
the physical LAN and must not claim to isolate anything.

*Decision.* Every network declares a `kind`. `provider` (untagged) and `vlan`
(tagged) attach to physical infrastructure, are created by platform
administrators only, and guarantee nothing beyond what that infrastructure
provides. `tenant` is a VXLAN overlay and is the only kind that guarantees a
distinct broadcast domain. One network corresponds to exactly one L2 segment,
enforced by a unique `segment_key` (`uplink:tag` or `vxlan:vni`).

Segment identifiers are allocated by the control plane and are never
caller-selectable. VLAN tags may be chosen only by platform administrators and
only from a configured allowlist, because a tag must match what the physical
switch trunks. A released VNI is quarantined before reuse, so a host returning
with a stale `vxbr<vni>` and a live tunnel mesh can never have it adopted by a
different network.

*Consequences.* Isolation claims exist only where they can be enforced; a
provider network says out loud that it guarantees nothing. Networks predating
this ADR are grandfathered with a NULL segment key — excluded from the unique
index, flagged `legacy_segment` — so a running cluster is untouched and
consolidating them stays an operator action rather than something a migration
does to live workloads. A `tenant` network is L2-only: nothing routes off it
until an L3 gateway exists. Rejected: overlay-only networking (breaks provider
connectivity on day one); caller-chosen VNIs; and `max(vni)+1` allocation, whose
release-then-reuse is the stale-mesh bug this replaces.

### ADR-017 — Every guest port carries an explicit dataplane policy

*Status:* Accepted.

*Context.* A NIC with no security group was unfiltered: control sent
`filtered=false` and the agent cleared every OpenFlow rule for that TAP. Absence
of configuration meant absence of enforcement, which is indistinguishable from a
misconfiguration and leaves nothing to audit.

*Decision.* Effective policy for a NIC is its network's default security group
unioned with the NIC's own groups. An empty NIC group set therefore means "the
network's default applies", never "unfiltered". "Open" is expressed as an
explicit allow-any rule inside a real security group — an object that can be
read, audited and tightened — never as the absence of one. A network's default
group is `managed`: it cannot be deleted on its own, because that would leave
every NIC on that network silently unpoliced.

*Consequences.* Migration seeds every existing network a permissive default, so
reachability on upgrade is unchanged, and tightening becomes a visible,
reversible, per-network action. Because moving a NIC from no flows to conntrack
flows is still a real dataplane change, enforcement is gated on
`[network] policy_mode`, which defaults to the previous behaviour. Rejected: a
cluster-wide default-deny flag — a flag day for every VM at once, invisible in
the API, and unable to express "this network stays open while that one closes".

### ADR-018 — The project is the unit of tenancy

*Status:* Accepted. Schema, objects, scoping (reads and writes) and per-project
RBAC (ADR-020) landed; quotas (ADR-019) follow. Still gated on
`[tenancy] enabled`, off by default — an existing deployment keeps behaving
exactly as it did until an operator opts in.

*Context.* The platform is single-tenant: any authenticated caller holding a
permission sees every resource. Introducing tenancy touches the ownership of
most tables, the RBAC binding model and admission control at once. Doing those
separately would migrate the same tables three times, each pass rewriting the
previous one's queries.

*Decision.* A **project** is the sole unit of tenancy: flat, non-hierarchical,
identified by a UUID. VMs, volumes, templates, security groups and tasks are
owned by exactly one project. Images and networks are **shareable** — an unset
project means platform-shared and usable by every project. Hosts, users, role
definitions, enrollment tokens and the CA are **platform** resources and are
never project-scoped: a host inventory is a leak to a tenant with no tenant
benefit, and placement is a platform concern (§17, §30).

Scoping is enforced in the control plane by a scope-carrying store handle, not
by PostgreSQL row-level security. The control plane uses one database role and
one pool, and most of its queries are single statements off that pool; RLS would
need a session GUC set inside a transaction for every one of them, plus a second
`BYPASSRLS` role for the reconcile loop — the same two-scope model, expressed
where the compiler cannot check it, and turning authorization bugs into silent
empty result sets rather than errors.

Tenancy is a control-plane concept. `project_id` is a column, never a field of
`VirtualMachineSpec`: the host agent and `proto/agent.proto` learn nothing about
projects, which preserves the privilege boundary of ADR-001 and §30.

Two rules follow from "shareable" and are not symmetric. A shared row (NULL
owner) is **readable** from every project and **writable** from none: sharing a
resource must not hand every project the power to delete it out from under the
others. And a shareable row created while tenancy is off is left NULL rather
than stamped `default` — with the feature off there is no project context to
record, and inventing one would make every image and network created today
invisible to every other project the day tenancy is switched on.

*Consequences.* Existing rows are assigned to a `default` project by column
default, so the backfill is free and an older binary that omits `project_id` on
insert still works — a rollback path for a control plane that migrates at
startup. Shareable catalogues stay shared, so a fleet's curated images and its
provider network keep working the moment a second project appears. Project
deletion is refused while the project owns anything: cascading would mean
deleting VMs, which is a long, agent-touching, restartable operation and does
not belong behind a DELETE (§7). A hierarchy is left unbuilt but not foreclosed
— `parent_id` exists, unenforced, because recursive permission inheritance and
quota rollup are load-bearing decisions that cannot be guessed correctly in
advance.

Scoping data is not on its own isolation — the request's project arrives in a
header, and authority has to be scoped too. That is ADR-020, which completes the
boundary and is what allows the feature to be switched on.

### ADR-020 — A role binding names the project it applies in

*Status:* Accepted.

*Context.* ADR-018 made the project the unit of ownership and every query
carries its predicate. That scopes *data*. It does not scope *authority*: the
project a request acts in arrived in a header, and permissions were a single
global set. A caller holding `vm:read` anywhere held it everywhere, so naming
another project in the header was enough to read it. Scoping without this is
worse than no tenancy at all, because the boundary is visible and does not hold.

*Decision.* A role binding — a user's direct grant and an OIDC group mapping
alike — carries a `project_id`. **NULL means platform-wide**, which is exactly
what every binding meant before this existed, so the migration changes no
behaviour and the first-admin bootstrap keeps working.

A caller's permissions are then resolved **in the project the request names**:
the union of their platform-wide bindings and their bindings in that project. A
caller with no binding there resolves to the empty permission set and fails
every guard. That is the entire enforcement mechanism — there is no separate
membership check, because a membership check is a thing one can forget to call
at a new endpoint, and an empty permission set is not.

The scope is resolved once per request and memoised, because two extractors
consume it: the one that picks rows and the one that decides permissions. A
request authorized in one project while reading another is precisely the failure
this exists to prevent, so they read one value rather than each parsing the
request again.

`X-Vquasar-Project: *` selects the platform view. It is **not** a privilege:
permissions are resolved against it the same way, so a caller holding only
project bindings resolves to nothing there. It exists because a platform admin
needs a cross-project view, and because a platform-wide binding needs a scope it
can be created from.

A binding is created in the scope the request is acting in — the same scope the
caller's own `iam:manage` was resolved in. This is what closes the escalation: a
project administrator cannot mint a platform-wide grant, because doing so
requires acting in platform scope, where their permissions are empty.

*Consequences.* With tenancy enabled and no project named, a request acts in the
default project — including an IAM write, which therefore creates a
default-project binding rather than a platform-wide one. That is deliberate:
where authorization is concerned the quiet default must be the narrower one.
Platform-wide bindings remain possible and explicit, via `*`.

Which projects exist is itself tenancy information, so `GET /projects` returns
only those the caller holds a binding in; a platform-wide binding sees them all.

Alone, this makes tenancy enforceable but does not bound consumption — a project
can still exhaust the fleet. That is ADR-019.

### ADR-021 — One leader for the controllers, elected by a lease row

*Status:* Accepted and implemented, except for agent-side fencing (below).

*Context.* A single `vquasar-control` is a single point of failure. Running two
is safe for the API — the instances hold no authoritative state — but not for
the controllers. Most of what they do is idempotent: `EnsureVm` against a
generation counter converges to the same place however many times it runs, IP
allocation is protected by a unique constraint, segment allocation and quota
admission both take `FOR UPDATE`. The migration controller is not. It is a
persisted state machine advanced one step per tick with no claim on the row, so
two instances would both call `prepare_receive` and produce two receivers for
one guest — a broken guest, not a retryable error.

*Decision.* Every instance serves the API. Exactly one runs the controllers,
and holds a **lease row** in PostgreSQL to say so: one row, a holder, an epoch,
and an `expires_at` renewed on a timer. Acquisition is a single `UPDATE ...
WHERE expires_at < now() OR holder = $me`, so two instances racing produce
exactly one winner and the loser simply sees no row returned.

A lease row rather than `pg_try_advisory_lock`, because **sqlx hands out
arbitrary pooled connections**: a session-scoped lock ends up held by whichever
connection took it rather than by the instance, and returning that connection to
the pool makes ownership unobservable. A row also answers "who is the leader"
with a `SELECT`, which is the question an operator actually has, and is what
`GET /leader` returns.

Every timestamp comes from PostgreSQL's clock. Instances whose clocks disagree
still agree about the lease, because none of them is asked what time it is.

*Fencing.* The failure to survive is a leader paused past its expiry — a long GC
pause, a hypervisor freezing the control VM — that wakes after another instance
has taken over. Renewal alone does not prevent it: the check and the act are not
atomic, so any gap between them is a window.

Two measures bound it. The controller acts only while **more than half the TTL
remains**, so a pause longer than that is noticed before the next action rather
than after it. And the migration controller **re-confirms the lease against the
database immediately before each step**, because it is the one operation where a
duplicate corrupts rather than converges. The residual window is a pause that
begins inside the margin and outlasts it, on migration only.

Closing it completely means the agent carrying and checking the epoch, so a
stale caller is rejected at the far end. That is deferred as its own milestone:
it changes `proto/agent.proto` and the privilege boundary, it forces agent-side
change into what is otherwise a control-plane-only feature, and it must be
sequenced across two releases so a deployed fleet keeps working. The epoch
column exists and is monotonic per term, so the token is ready when that lands.

Two deployment constraints follow from decisions made elsewhere, and both were
confirmed on a two-node lab rather than predicted. **Every instance must present
a certificate with the same CN**, because the agent pins it (ADR-001, §30) — a
peer with a different CN is refused by every agent, and the symptom is an
unreachable host rather than an authentication error. And **the shared address
must appear in every instance's SAN**, or verification fails precisely when the
VIP moves. Neither is enforced by code; both belong in the runbook, because the
alternative — relaxing the pin — would give back the property it exists for.

The shared address also has to be settled *before* any VM is created:
`[enrollment] control_url` is rendered into cloud-init at seed time, so a guest
keeps the address it was built with. That is the same class of problem as
overlay MTU (ADR-016) and has the same remedy, which is to decide it early.

*Consequences.* The instance identity is **stable across restarts** (hostname by
default, `[server] instance_id` to override). That is deliberate on two counts:
a restarted instance resumes its own lease immediately instead of making the
fleet wait out the TTL, and it can recognise its own orphaned in-flight work.
The trade is that two control planes on one host must be given distinct ids.

Startup reclaim of orphaned work is now scoped by owner. Reclaiming everything
transitional was exact for one control plane, and would be destructive with
several — a restarting instance would kill a download another instance is still
running. Rows record the instance that started the work; a NULL owner predates
the column and can only have been written by a binary that is no longer running.

Not addressed here: PostgreSQL's own availability, which is Patroni or a managed
instance and not something vquasar should grow a worse version of; and splitting
the leader per controller, which the same mechanism supports and which should
wait until something demonstrates it is needed.

### ADR-022 — The agent rejects a superseded controller by lease epoch

*Status:* Accepted and implemented (M21), lenient by default. Strict mode is
`[grpc] require_controller_epoch` on the agent and stays off until a fleet is
fully upgraded.

*Context.* ADR-021 bounds — but does not close — the window in which a leader
that has lost its lease can still act. A process paused past its margin (a long
GC pause, a frozen VM, a partitioned host) can wake and issue an agent RPC after
another instance has taken over. The lease-margin check narrows this to roughly
half a TTL; it cannot eliminate it, because the check happens in the caller,
which is precisely the component that cannot be trusted to be running.

Most of the surface tolerates this. `EnsureVm` converges, so a stale duplicate
is wasted work. Migration does not: two `PrepareReceive` calls mean two
receivers for one guest, which is a corrupted VM rather than a retryable error.

*Decision.* The controller stamps every agent RPC with the **epoch** of the
lease it holds, and the agent refuses any request whose epoch is lower than the
highest it has seen. The epoch already exists — `controller_lease.epoch`
increments on every acquisition — so this adds a field, not a mechanism.

The check belongs in the agent because the agent is the only party that can
still be right when the caller is wrong. That is uncomfortable next to ADR-001
and §30, which keep the agent free of orchestration authority, and the shape is
chosen to keep it that way: the agent does not evaluate *who* should be leader,
does not read the lease, and holds no opinion about the control plane. It
compares one integer against the largest it has been told, which is bookkeeping,
not judgement.

**The epoch is persisted** alongside the agent's other runtime state. A restart
that forgot it would reopen exactly the window this closes — a stale controller
would be believed by an agent that had just come back.

**Rejection is per-RPC, not per-connection.** A connection outlives a lease, and
tearing one down on a stale call would take working requests with it.

**An absent epoch is accepted, and logged.** This is the migration path: agents
must tolerate a controller that does not send one, or a rolling upgrade breaks a
deployed cluster — which §44/ADR-005 treats as a hard requirement. Agents are
upgraded first, then controllers begin stamping. Once a fleet is fully upgraded
an operator can make the agent strict; until then the field is advisory, and the
warning is what tells you the fleet is not yet ready to enforce it.

*Amended during implementation.* The epoch travels as **gRPC metadata**, not as
a field on every request message. Implementation made the difference obvious:
the agent already enforces the control plane's identity in a tonic interceptor
(`RequireControlIdentity`), which sees request metadata and sits in front of
every one of the thirteen RPCs. Putting the epoch there means:

* `proto/agent.proto` does not change at all, so there is no generated-code
  skew to sequence — a strictly smaller upgrade than the one described below;
* the check lives beside the identity check, which is the same kind of question
  asked at the same point, rather than being repeated in thirteen handlers;
* the domain contract stays free of control-plane bookkeeping, which is a better
  answer to the boundary concern than "one field, deliberately the only one".

Everything else stands: persisted across restarts, per-RPC, absent-is-accepted.
The original wording is kept below because the reasoning that led to it is still
the reasoning for the mechanism — only the carrier changed.

*Scope, established by testing (#42, #45).* This fences a controller that is
superseded **but alive**. It does not prevent the duplicate `PrepareReceive`
described above when the old leader is *dead*: the successor carries a **higher**
epoch and is admitted by design. Interrupting a real migration showed that the
dead-leader case is the common one, so the corruption this ADR cites as its
motivation is closed by at-most-once semantics on the agent (#45), not by
fencing — `PrepareReceive` returns the receiver that already exists, and
`FinalizeReceive` is idempotent once the guest is adopted. What fencing uniquely
closes is the paused-process window ADR-021 can only bound, which no amount of
idempotency addresses. Both are needed and neither substitutes for the other.

That correction is worth keeping visible: the mechanism was designed from the
migration hazard, and the hazard turned out to need a different mechanism. The
lesson is not that fencing was wrong, but that "a duplicated call corrupts this"
and "a duplicated call can be *refused*" are separate claims, and only the first
was ever established.

*Consequences.* The epoch crosses the privilege boundary, which is the first
time the control plane's internal bookkeeping does. It is deliberately the only
such value, and it is opaque to the agent.

A rejected call surfaces to the controller as an error, so it counts against the
reconcile budget (M20a) and eventually marks the VM `Failed` — correct, since a
controller being refused by its agents is not going to converge and should say
so rather than retry silently.

The window does not close until the fleet is upgraded *and* strict mode is on.
Between those points this is observability: the warning says a superseded
controller reached an agent, which is a fact worth knowing even when it is
tolerated.

Rejected: fencing by connection identity (the control plane presents one CN by
design, so every instance looks alike — ADR-021); and having the agent read the
lease from PostgreSQL, which would give the agent a database credential and the
authority to decide who leads, undoing the boundary this design exists to hold.

### ADR-023 — A storage pool is a named place to put bytes, and reachability is observed

*Status:* Accepted. The resource and agent reporting are implemented (M23a,
M23b); volumes referencing a pool, and the scheduler refusal that depends on
them, are not yet.

*Context.* Storage today is one directory. `[storage] shared_volumes_dir` names
it, every volume is a file under it, and every host is *assumed* to have it
mounted at the same path. That assumption is invisible: nothing records it,
nothing checks it, and a host that does not have the mount fails at launch with
a path error rather than being refused at placement. §20 has always said storage
must become pluggable; the reason to do it now is not the plugins but the
assumption, which live migration silently depends on.

Two things are tangled in that one config value: *where bytes go* and *which
hosts can reach them*. Adding backends without separating them would multiply
the assumption rather than remove it.

*Decision.* A **storage pool** is a first-class resource: an id, a name, a
`kind`, and kind-specific parameters. The initial kind is `shared_dir` (a path
assumed mounted on the hosts that report it); `lvm_thin`, `nfs` and `rbd` are
the shapes it was designed to accept. A volume belongs to exactly one pool, and
a pool is where its file lives — `<pool>/volumes/<uuid>.<fmt>` replaces the
global directory.

**Reachability is observed, not declared.** Each agent reports which pools it can
actually use, and the control plane records that as observed state (§7). The
scheduler then refuses a host that does not report every pool a VM's disks need,
so "this host cannot see that storage" becomes a placement refusal with a reason
instead of a launch failure. An operator declaring reachability would be
recording an intention that the filesystem is free to contradict — which is the
failure this ADR exists to remove, restated one level up.

Capacity is reported the same way and for the same reason: a number the operator
typed is a number that goes stale.

**Pools are platform resources**, like hosts and unlike images. Any project may
place a volume in any pool. Per-project pool restrictions are a real requirement
and deliberately not this change: they belong with quotas (ADR-019), which
already count storage per project, rather than with ownership.

*Consequences.* Migration creates a `default` pool from the existing
`shared_volumes_dir` and points every existing volume at it, so a running cluster
keeps working and its paths do not move — the backward-compatibility requirement
in ADR-005. `shared_volumes_dir` becomes the seed for that one row rather than a
value read at run time.

The `Pending`/`Ready` distinction a pool needs is the same one hosts have: a pool
no host reports is not usable, however correct its configuration looks. It is
reported as such rather than being deleted or hidden, because the usual cause is
a mount that has not come back yet.

This also gives the orphaned-seed sweep (#41) a root it can address. The agent
cannot sweep shared storage — a seed there may belong to a VM on any host — but
the control plane can, once a pool tells it where "there" is.

Rejected: a `StorageBackend` trait in the agent as the first move (§20's original
sketch), which puts the abstraction where the plugin goes rather than where the
assumption is, and would have to be re-cut once pools exist; and letting a VM
name a raw path per disk, which is the current behaviour and the reason
`allowed_paths` has to exist at all.

### ADR-019 — Quotas are admission control on committed intent

*Status:* Accepted and implemented.

*Context.* The control plane persists intent and converges asynchronously (§7,
§15). A resource limit could be enforced when intent is recorded or when the
reconcile loop tries to realise it. The two are not equivalent.

*Decision.* A quota is a ceiling on **committed intent** — the resources
described by rows that exist — enforced **only at API admission**, in the same
transaction that persists the intent. The reconcile loop never rejects work for
quota reasons; it may only report observed usage. A resource consumes quota from
the moment its row exists until the row is gone, including while `Pending`,
`Failed` or `Deleting`. That differs deliberately from the scheduler's per-host
commitment model, which excludes `Deleting`: the two answer different questions.

Usage is **derived, not stored**. Admission locks the project row, aggregates
current usage from the owning tables, compares against the limits and inserts —
all in one transaction. That serialises writes per project and only per project,
and leaves no cached counter that can drift or need repair after a crash.

*Consequences.* Every write that changes a counted quantity must pass admission,
including in-place VM edits, not only creation. In practice that meant reducing
the spec-writing paths to one: two existed, and only one had been gated. A
second door is how this gets bypassed six months later.

Operations that do expensive external work before persisting — cloning a volume
from an image — insert a `provisioning` row inside the admission transaction and
finalise afterwards. The old order (convert, then insert) cannot be admitted at
all: the expensive part would happen before anything was counted, so two
concurrent creates would both convert gigabytes and only then discover one did
not fit. Because a clone grows to the image's virtual size, which `qemu-img`
only reveals after converting, the reservation is the largest figure known up
front and the finalise admits the difference.

Storage counts volumes *and* the disks a VM spec asks the agent to provision.
Counting only volumes would leave the cap bypassable by asking for the space as
a VM disk. vCPU and memory count the hot-plug *ceiling* (`max_vcpus`,
`max_size_mib`), because that is what was committed to, whatever the VM boots
with.

A refusal is `409` with a `QUOTA_EXCEEDED` code, and the message carries the
dimension, the limit and current usage — "over quota" alone sends an operator to
the database to work out which limit and by how much. It is deliberately not
`INSUFFICIENT_RESOURCES`: that means the fleet is full, whereas a quota refusal
would happen on an empty cluster and is fixed by an operator raising a limit.
Lowering a quota below current usage is permitted and non-destructive: it blocks
new commitments and is reported as over-quota. Rejected: reconcile-time
enforcement, which would strand persisted intent and make the reconcile loop a
second authority on admissibility; and denormalized usage counters, which
introduce a second source of truth needing a repair pass.

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

## 47. Multi-tenancy

A **project** is the unit of tenancy. VMs, volumes, templates, security groups
and tasks belong to exactly one; images and networks are shareable, where no
owner means platform-shared. Hosts, users, role definitions, enrollment tokens
and the CA are platform resources and are never project-scoped.

Scoping is enforced in the control plane by a scope-carrying store handle whose
queries all share one predicate shape, not by PostgreSQL row-level security
(ADR-018). A request resolves to exactly one project — from `X-Vquasar-Project`,
or `?project=` where a WebSocket handshake cannot set headers, or the caller's
default — never to "everything" by omission. `*` selects the platform view.

Authority is scoped the same way: a role binding names the project it applies
in, and a caller's permissions are resolved *in* the project the request names
(ADR-020). A caller with no binding there resolves to the empty permission set,
which is the whole enforcement — there is no separate membership check to
forget. Consumption is bounded by quotas, admitted against committed intent in
the transaction that persists it (ADR-019).

The whole feature is gated on `[tenancy] enabled`, off by default: with it off
every caller runs at platform scope and behaviour is exactly what a
single-tenant deployment had. `project_id` is a column and never a field of
`VirtualMachineSpec` — the agent learns nothing about projects, which preserves
the privilege boundary of ADR-001 and §30.

## 48. Control-plane high availability

Several `vquasar-control` instances run against one PostgreSQL. All of them
serve the REST API — they hold no authoritative state, so the API is
active/active and any of them can answer any request. Exactly one runs the
**controllers**: the reconcile loop, the migration controller and the sweeps.

Which one is decided by a lease row rather than an advisory lock or an external
coordinator (ADR-021). A standby keeps ticking and does nothing; the tick is
where it notices it has been promoted, so there is no loop to start on promotion
that could fail to start.

Most controller work is idempotent by construction — `EnsureVm` against a
generation counter converges however many times it runs — so a brief overlap
between an old and a new leader is harmless. Migration is the exception, and is
fenced separately (ADR-021).

Three things this does *not* do, deliberately:

* **PostgreSQL HA.** vquasar does not implement database failover; that is
  Patroni or a managed instance. Growing a second, worse one here would put the
  platform's durability in the least-tested part of it.
* **Sharding the controllers.** One leader runs all of them. Splitting per
  controller is possible on the same mechanism and is worth doing only when
  something demonstrates it is needed.
* **Fencing at the agent.** See ADR-021.

Two deployment constraints follow and are not optional:

* Every instance must present a certificate with the **same CN**, because the
  agent pins the control plane's identity (§30, ADR-021). Otherwise agents
  reject every instance but one.
* Agent-to-control traffic — phone-home, enrollment — needs a stable address in
  front of the instances: a VIP, or a DNS name resolving to all of them.

