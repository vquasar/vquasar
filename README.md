# ch-orchestrator

A modern, open-source virtualization management platform built directly around
[Cloud Hypervisor](https://www.cloudhypervisor.org/) — no libvirt, no
Kubernetes. It manages fleets of Linux hypervisor hosts and exposes virtual
machines as first-class, reconciled resources.

> **Status: Milestone 8 (shared-storage live migration) — verified. All 8 design milestones complete.** On top of Milestones 0–7, a running VM can now live-migrate between hosts. Migration is a persisted state machine (Pending → Sending → Finalizing → Completed) that orchestrates the source and destination agents, which drive Cloud Hypervisor's send/receive-migration API. Verified on real hardware: `POST /api/v1/vms/{id}/migrate` moved a running guest host-02 → host-01 while an in-memory counter continued uninterrupted (a live migration, not a recreation). See [`DESIGN.md`](DESIGN.md) section 42.

## Architecture at a glance

```
Web UI  ──REST/WS──▶  ch-control  ──gRPC/mTLS──▶  ch-agent  ──▶  Cloud Hypervisor + OVS
(later)               (control plane)             (per host)      (per VM)
```

* **`ch-control`** owns global intent: the public REST API, scheduling, desired
  VM state, tasks and events. It never touches host resources directly.
* **`ch-agent`** is the local authority on each host: it launches and supervises
  Cloud Hypervisor, manages TAP/OVS and storage, and reports inventory.

Two architectural rules drive everything (design sections 7 and 30):
desired state and observed state are kept separate and reconciled; and a host
compromise must not imply a control-plane compromise.

## Repository layout

```
crates/
  common/     # ch-common  — error taxonomy, telemetry
  model/      # ch-model   — stable orchestration domain model (hosts, VMs, specs)
  ch-client/  # ch-client  — direct Cloud Hypervisor client (process + API + translation)
services/
  control/    # ch-control — control-plane binary
  agent/      # ch-agent   — host-agent binary
config/       # example TOML configuration
proto/        # agent gRPC schema (Milestone 2)
migrations/   # SQLx migrations (Milestone 3)
docs/         # additional documentation
```

The `ch-client` crate keeps its three concerns deliberately separate
(design section 43): [`process`](crates/ch-client/src/process.rs) (the
`cloud-hypervisor` process), [`socket`](crates/ch-client/src/socket.rs) (the
HTTP-over-Unix-socket API client), and [`config`](crates/ch-client/src/config.rs)
(translation between the domain model and CH's wire types). The
[`Hypervisor`](crates/ch-client/src/hypervisor.rs) trait has both a real
(`CloudHypervisor`) and a test (`FakeHypervisor`) implementation.

## Building and testing

Requires a stable Rust toolchain (see [`rust-toolchain.toml`](rust-toolchain.toml)).

```bash
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets --all-features
cargo fmt --all -- --check
cargo deny check          # requires: cargo install cargo-deny
```

## Running (Milestone 0)

Both binaries currently load config, initialise structured logging, and report
what they would do:

```bash
cargo run --bin ch-agent   -- --config config/agent.toml
cargo run --bin ch-control -- --config config/control.toml
```

Any config value can be overridden by an environment variable, e.g.
`CH_CONTROL_SERVER__LISTEN=127.0.0.1:9000` or `CH_AGENT_AGENT__NAME=host-02`.

## Booting a real VM (Milestone 1)

On a Linux host with `/dev/kvm`, Cloud Hypervisor, and the image assets:

```bash
# 1. Fetch cloud-hypervisor + firmware into /var/lib/ch-orchestrator/bin (once).
# 2. Prepare the latest Ubuntu cloud image (raw disk + its kernel/initrd + seed):
scripts/prepare-ubuntu-image.sh --release 26.04

# 3. Make a per-VM copy of the base disk and boot it through the Hypervisor trait:
cp --reflink=auto /var/lib/ch-orchestrator/images/ubuntu-26.04.raw \
                  /var/lib/ch-orchestrator/volumes/vm01.raw
cargo run -p ch-client --example boot_vm -- \
  --binary        /var/lib/ch-orchestrator/bin/cloud-hypervisor \
  --kernel        /var/lib/ch-orchestrator/images/vmlinuz-<ver> \
  --initramfs     /var/lib/ch-orchestrator/images/initrd.img-<ver> \
  --cmdline       "root=/dev/vda1 rw console=ttyS0 systemd.mask=systemd-networkd-wait-online.service" \
  --disk          /var/lib/ch-orchestrator/volumes/vm01.raw \
  --readonly-disk /var/lib/ch-orchestrator/seed/seed.iso \
  --runtime-dir   /var/lib/ch-orchestrator/vms/vm01
```

The harness reports `BOOT OK — marker observed on serial console` once cloud-init
finishes inside the guest. The same flow is covered by the opt-in integration
test `crates/ch-client/tests/boot_integration.rs` (set `CH_IT_*` env vars to run
it; otherwise it skips).

**Boot styles.** Two are first-class in the model ([`BootSpec`](crates/model/src/vm.rs)),
and both are verified booting the latest Ubuntu cloud image on CH v53:

* **Firmware** (`Firmware`) — boots the cloud image's *own* bootloader via the
  EDK2 **`CLOUDHV.fd`** firmware (full modern UEFI: shim → GRUB → kernel, Secure
  Boot infrastructure present). No kernel extraction needed — point it at the
  disk and go. Build the firmware with
  [`scripts/build-cloudhv-firmware.sh`](scripts/build-cloudhv-firmware.sh), then
  boot with `--firmware /var/lib/ch-orchestrator/firmware/CLOUDHV.fd` instead of
  `--kernel/--initramfs`. This is the recommended path for whole cloud images.
* **Direct-kernel** (`DirectKernel`) — boots a kernel (bzImage/PVH `vmlinux`) +
  optional initrd with an explicit cmdline; `prepare-ubuntu-image.sh` extracts
  the image's own kernel/initrd for this. Ideal for a controlled kernel and the
  design's developer default (section 24).

> Note on firmware: CH's `--firmware` requires the EDK2 **CloudHv** build
> specifically. The QEMU OVMF packages (`/usr/share/OVMF/*`) are *not*
> compatible, and rust-hypervisor-firmware (a PVH firmware, loaded via
> `payload.kernel`) can't complete modern Ubuntu's shim/GRUB chain — hence
> `CLOUDHV.fd`.

## Host agent over gRPC (Milestone 2)

`ch-agent` is the local authority for one host (design section 9). It serves the
`HostAgent` gRPC API (schema: [`proto/agent.proto`](proto/agent.proto), generated
by the [`ch-proto`](crates/proto) crate) backed by a real Cloud Hypervisor
process manager. Run it and drive it with the bundled client:

```bash
# Start the agent (points at the lab from Milestone 1).
CH_AGENT_GRPC__LISTEN=127.0.0.1:9500 \
CH_AGENT_HYPERVISOR__BINARY=/var/lib/ch-orchestrator/bin/cloud-hypervisor \
CH_AGENT_HYPERVISOR__RUNTIME_DIR=/var/lib/ch-orchestrator \
cargo run -p ch-agent

# In another shell — inventory, create a VM, inspect, delete:
cargo run -p ch-agent --example agent_client -- host-info
cargo run -p ch-agent --example agent_client -- ensure \
  --vm-id "$(cat /proc/sys/kernel/random/uuid)" --name demo \
  --kernel /var/lib/ch-orchestrator/images/vmlinuz-<ver> \
  --initramfs /var/lib/ch-orchestrator/images/initrd.img-<ver> \
  --disk /var/lib/ch-orchestrator/volumes/demo.raw \
  --readonly-disk /var/lib/ch-orchestrator/seed/seed.iso
```

The full **restart-survival** acceptance (start a VM, stop the agent, confirm
the VM keeps running, restart the agent, confirm it recovers the same process,
then delete) is scripted in
[`scripts/agent-restart-demo.sh`](scripts/agent-restart-demo.sh).

Key design points, verified on real hardware: dropping the agent never kills VMs
(`ch-agent` recovers them on restart, section 11); tearing a VM down uses Cloud
Hypervisor's `vmm.shutdown` API so it works even for a re-attached VM the agent
no longer owns as a child process.

## Control plane (Milestone 3)

`ch-control` persists desired state in PostgreSQL and reconciles it against the
host agents. It needs a database; the reconcile loop and REST API are otherwise
self-contained. Compile-time never touches a database — `sqlx` runtime queries
keep builds and CI DB-free.

```bash
# 1. A PostgreSQL to talk to (any will do):
docker run -d --name ch-pg -p 5432:5432 \
  -e POSTGRES_USER=ch -e POSTGRES_PASSWORD=ch -e POSTGRES_DB=ch_orchestrator postgres:16

# 2. Start the control plane (applies migrations on boot):
CH_CONTROL_DATABASE__URL=postgres://ch:ch@127.0.0.1:5432/ch_orchestrator \
CH_CONTROL_SERVER__LISTEN=127.0.0.1:8080 \
cargo run -p ch-control

# 3. With a ch-agent running (Milestone 2), register it and create a VM:
curl -X POST localhost:8080/api/v1/hosts -H 'content-type: application/json' \
  -d '{"name":"dome","endpoint":"http://127.0.0.1:9500"}'

curl -X POST localhost:8080/api/v1/vms -H 'content-type: application/json' -d '{
  "name":"demo",
  "spec":{"desired_power_state":"Running","cpu":{"boot_vcpus":2,"max_vcpus":2},
    "memory":{"size_mib":2048},
    "boot":{"type":"direct_kernel","kernel":"/var/lib/ch-orchestrator/images/vmlinuz-<ver>",
      "initramfs":"/var/lib/ch-orchestrator/images/initrd.img-<ver>",
      "cmdline":"root=/dev/vda1 rw console=ttyS0 systemd.mask=systemd-networkd-wait-online.service"},
    "disks":[{"path":"/var/lib/ch-orchestrator/volumes/demo.raw"},
             {"path":"/var/lib/ch-orchestrator/seed/seed.iso","readonly":true}],
    "network_interfaces":[],"placement":{}}}'
# -> {"vm_id":"...","task_id":"..."}; poll GET /api/v1/vms/{id} until phase=Running.
```

Endpoints (all under `/api/v1`): `hosts` (register/list/get), `vms`
(create/list/get/delete + `/start` + `/stop`), `tasks` (list/get), `events`
(list). Writes persist desired state and return a `task_id` immediately; the
reconcile loop ([`reconcile.rs`](services/control/src/reconcile.rs)) does the
work asynchronously (section 15). The scheduler
([`scheduler.rs`](services/control/src/scheduler.rs)) filters hosts that can't
fit the VM and scores the rest by free-memory fraction (section 17).

## Networking (Milestone 4)

VMs attach to virtual networks over Open vSwitch (design section 18). The split
follows the design: the control plane owns the model, the agent owns the
privileged dataplane (ADR-001/ADR-010).

```bash
# On each host, install OVS and create the integration bridge (once):
scripts/setup-ovs.sh --bridge br-int
# The agent must run privileged to manage TAPs/OVS:
sudo CH_AGENT_NETWORK__BRIDGE=br-int ... ch-agent

# Define a network, then reference it from a VM's NIC:
curl -X POST localhost:8080/api/v1/networks -d '{"name":"provider"}'         # flat
curl -X POST localhost:8080/api/v1/networks -d '{"name":"vlan-100","vlan":100}'  # tagged
# ... "network_interfaces":[{"network_id":"<id>"}] ... in the VM spec.
```

What happens on `POST /vms` with a NIC: the control plane allocates a MAC
(deterministic from VM id + NIC index, [`netalloc.rs`](services/control/src/netalloc.rs))
and resolves the network to a VLAN, then sends per-NIC bindings to the agent.
The agent's [`OvsNetworkBackend`](services/agent/src/network.rs) creates
`tap<vmid8><idx>`, brings it up, and attaches it to `br-int` (with `tag=<vlan>`
when set); Cloud Hypervisor then drives that TAP. On delete, the TAP and OVS
port are removed. TAP names are derived from the VM id, so teardown works even
for a VM re-attached after an agent restart, with no persisted per-NIC state.

## Web UI (Milestone 5)

A single-page app in [`ui/`](ui/) — React 18 + TypeScript + Vite + MUI (Material
UI), with MUI DataGrid for the resource tables and React Query for polling-based
live state (design section 33). It is strictly API-only (ADR-015): every page
talks to `/api/v1`. Pages: Dashboard, Hosts (+ register), Virtual Machines
(list / create form / detail, with start/stop/delete), Networks (+ create),
Tasks (with progress), and Events. Each VM detail page has a **Console** button
that opens an xterm.js serial console over a WebSocket
(`/api/v1/vms/{id}/console`), proxied through to the VM's serial port
(design section 25).

```bash
cd ui
npm install
npm run dev        # http://localhost:5173, proxies /api -> http://127.0.0.1:8080
# or build a static bundle and let the control plane serve it:
npm run build      # -> ui/dist
CH_CONTROL_SERVER__UI_DIR=$(pwd)/dist cargo run -p ch-control   # UI at http://127.0.0.1:8080
```

> **UI framework note.** The design (§34) suggested plain React/Vite. We build
> on **React + MUI** instead: the same Material design language and an
> enterprise-grade DataGrid out of the box, while staying lighter and closer to
> the design than Angular Material (which was also considered). This is a
> deliberate, recorded deviation, not a change to any ADR.

## Live migration (Milestone 8)

A running VM can be migrated to another host with `POST /api/v1/vms/{id}/migrate`
(`{ "target_host_id": "..." }`), or from a VM's detail page in the UI. Shared
storage is assumed — the same disk path is reachable on both hosts (design
section 28).

Migration is a **persisted state machine** in the `migrations` table, advanced
one step per reconcile tick so it survives a control-plane restart rather than
living in a single RPC:

* **Pending** → the destination agent launches an empty VMM and starts a
  receiver (`vm.receive-migration`), returning the migration URL.
* **Sending** → the source agent streams the live VM state
  (`vm.send-migration`).
* **Finalizing** → the destination adopts the now-running VM; the source discards
  the husk; the VM's `host_id` moves to the target.
* **Completed** / **Failed** (on failure the VM stays on its source host).

Verified on real hardware: a running guest migrated between two agents while an
in-memory counter continued uninterrupted — a live migration, not a recreation.

> Note for a single-host lab: two co-located agents share a filesystem, so the
> destination's serial path (carried in the migrated config) collides with the
> source's. Run the agents with `serial_mode = "file"` there. Separate hosts use
> identical path strings on distinct filesystems and need no such workaround.

## Design & invariants

The full design lives in [`DESIGN.md`](DESIGN.md). The load-bearing
architectural decisions are recorded as ADR-001 … ADR-015 in section 44; code
comments reference design sections by number.

## License

Licensed under the [Apache License 2.0](LICENSE).
