# vquasar

vquasar manages fleets of Linux hosts running
[Cloud Hypervisor](https://www.cloudhypervisor.org/) and exposes virtual
machines as first-class, reconciled resources: you declare what a VM should be,
and the platform converges the fleet on that. It talks to Cloud Hypervisor's
API directly — there is no libvirt in the execution path, and no Kubernetes
anywhere.

Written in Rust, with a PostgreSQL-backed control plane, a per-host agent, a
REST API and a React web UI.

## How it differs

* **Cloud Hypervisor natively.** The agent drives `cloud-hypervisor` processes
  over their HTTP-on-Unix-socket API, so CH features (live migration,
  CPU/memory hot-plug, `vm.counters`) stay reachable instead of being flattened
  into a lowest-common-denominator abstraction (ADR-002, ADR-003).
* **Desired vs observed state.** Writes persist intent and return a task id; a
  reconcile loop drives hosts towards it and records what it observes
  (ADR-005, ADR-007). Long operations — live migration, host drain, reboot
  recovery — are persisted state machines that survive a control-plane restart.
* **Not a Kubernetes add-on.** No cluster, no CRDs, no kubelet (ADR-004). The
  unit of management is a VM on a Linux host.
* **Host compromise ≠ control-plane compromise.** Control ↔ agent is mutual
  TLS, and the agent is the only component with privileged host access
  (ADR-001).

## Architecture

```
Web UI ──REST/WS(HTTPS)──▶ vquasar-control ──gRPC/mTLS──▶ vquasar-agent ──▶ cloud-hypervisor
                             │                             (one per host)      + Open vSwitch
                             └── PostgreSQL (desired state, tasks, events)
```

`vquasar-control` owns global intent: the REST API, scheduling, desired VM
state, IPAM, tasks and events. It never touches host resources directly.
`vquasar-agent` is the local authority on each host: it launches and supervises
Cloud Hypervisor, programs TAP/OVS and the on-host storage layout, and reports
inventory. Dropping the agent does not kill running VMs — it re-attaches to
them on restart.

## Status

Alpha. Verified on a real multi-host lab cluster, not in production anywhere.

Working today:

* **VM lifecycle** — create/start/stop/delete, direct-kernel and UEFI
  (`CLOUDHV.fd`) boot, cloud-init, templates, a microVM profile, a browser
  serial console, per-VM CPU/memory/disk/network metrics.
* **Live editing** — CPU and memory hot-plug, add disk/NIC, disk grow, rename,
  retarget a NIC to another network, all without restarting the VM.
* **Scheduling & migration** — placement by committed capacity, shared-storage
  live migration with CPU-feature compatibility gating, host cordon/drain, and
  automatic recovery of Running VMs after a host reboots.
* **Networking** — Open vSwitch dataplane, flat and 802.1Q VLAN networks,
  VXLAN overlays for cross-host L2, control-plane IPAM (IPv4/IPv6, rendered
  into cloud-init netplan), stateful per-NIC security groups, agentless
  guest-IP discovery.
* **Storage** — images (register by path, import from URL, upload),
  first-class volumes with attach/detach, qcow2 snapshots, image-backed
  bootable volumes.
* **Security** — mutual TLS with token-gated agent certificate enrollment,
  OIDC authentication with built-in and custom RBAC roles, AES-256-GCM field
  encryption of secret-bearing cloud-init data at rest.
* **Operations** — systemd install/uninstall scripts, Prometheus `/metrics`,
  JSON logging, OTLP trace export, in-process e2e tests and GitHub Actions CI.
* **Guests** — Ubuntu and Rocky verified end to end; Windows is *enabled*
  (UEFI boot, ISO attach, virtio-win staging) but cannot be installed
  interactively — see [`docs/windows-guests.md`](docs/windows-guests.md).

Not there yet:

* Control-plane HA — one control node, one PostgreSQL.
* Quotas, projects, multi-tenancy — including per-tenant network isolation.
* Storage backends beyond local/shared-filesystem, and per-VM storage policy.
* Automatic HA restart of VMs after host failure (blocked on fencing, ADR-014).

Both authentication and TLS are config-gated and **off unless configured**, so
a default build is unauthenticated and plaintext. Do not expose one outside a
trusted network.

## Quickstart

Needs a stable Rust toolchain (see
[`rust-toolchain.toml`](rust-toolchain.toml)), `protoc`
(`apt install protobuf-compiler`), Node (CI builds the UI on Node 20), and a
PostgreSQL to talk to.

```bash
cargo build --workspace
cargo test  --workspace --lib --bins
```

Run the control plane. It applies its migrations on boot:

```bash
docker run -d --name vquasar-pg -p 5432:5432 \
  -e POSTGRES_USER=ch -e POSTGRES_PASSWORD=ch -e POSTGRES_DB=vquasar postgres:16

VQUASAR_CONTROL_DATABASE__URL=postgres://ch:ch@127.0.0.1:5432/vquasar \
VQUASAR_CONTROL_SERVER__LISTEN=127.0.0.1:8080 \
cargo run -p vquasar-control
```

Run an agent (on a host with `/dev/kvm` and `cloud-hypervisor` installed), and
register it:

```bash
VQUASAR_AGENT_GRPC__LISTEN=127.0.0.1:9500 \
VQUASAR_AGENT_HYPERVISOR__BINARY=/var/lib/vquasar/bin/cloud-hypervisor \
VQUASAR_AGENT_HYPERVISOR__RUNTIME_DIR=/var/lib/vquasar \
cargo run -p vquasar-agent

curl -X POST localhost:8080/api/v1/hosts -H 'content-type: application/json' \
  -d '{"name":"host-01","endpoint":"http://127.0.0.1:9500"}'
```

Then the UI:

```bash
cd ui && npm install
npm run dev        # http://localhost:5173, proxies /api -> http://127.0.0.1:8080
```

Every value in [`config/control.toml`](config/control.toml) and
[`config/agent.toml`](config/agent.toml) can be set in a TOML file passed with
`--config`, or overridden by an environment variable: `VQUASAR_CONTROL_` /
`VQUASAR_AGENT_` prefix, `__` for nesting.

To boot actual VMs you also need an image, a bridge, and (for UEFI guests)
firmware — see [`docs/booting-vms.md`](docs/booting-vms.md). For a persistent
install as systemd services with TLS and OIDC, see
[`scripts/install.sh`](scripts/install.sh) (`--help` lists the options) and
[`scripts/README.md`](scripts/README.md).

## Repository layout

```
crates/
  common/     # vquasar-common  — error taxonomy, telemetry
  model/      # vquasar-model   — orchestration domain model (hosts, VMs, specs)
  client/     # vquasar-client  — Cloud Hypervisor client (process + API + translation)
  proto/      # vquasar-proto   — generated agent gRPC bindings
services/
  control/    # vquasar-control — control-plane binary
  agent/      # vquasar-agent   — host-agent binary
ui/           # React + TypeScript + MUI single-page app
proto/        # agent gRPC schema
migrations/   # SQLx migrations
config/       # example TOML configuration
scripts/      # host setup, image prep, certs, install/uninstall
docs/         # operator and developer guides
```

`vquasar-client` deliberately separates its three concerns:
[`process`](crates/client/src/process.rs) (the `cloud-hypervisor` process),
[`socket`](crates/client/src/socket.rs) (the HTTP-over-Unix-socket API client),
and [`config`](crates/client/src/config.rs) (translation between the domain
model and CH's wire types). The
[`Hypervisor`](crates/client/src/hypervisor.rs) trait has a real
(`CloudHypervisor`) and a test (`FakeHypervisor`) implementation.

## Where to go next

* [`docs/`](docs/) — booting VMs, Windows guests, local development.
* [`DESIGN.md`](DESIGN.md) — the full architecture. The load-bearing decisions
  are recorded as ADR-001 … ADR-015 in section 44; code comments reference
  design sections by number.
* [`ROADMAP.md`](ROADMAP.md) — what has landed and what is queued.
* [`scripts/README.md`](scripts/README.md) — what each helper script does.

## License

Licensed under the [Apache License 2.0](LICENSE).
