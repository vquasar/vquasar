# ch-orchestrator

A modern, open-source virtualization management platform built directly around
[Cloud Hypervisor](https://www.cloudhypervisor.org/) — no libvirt, no
Kubernetes. It manages fleets of Linux hypervisor hosts and exposes virtual
machines as first-class, reconciled resources.

> **Status: Milestone 2 (host agent) — verified.** Milestones 0–1 (workspace,
> domain model, and a `ch-client` that boots the latest Ubuntu cloud image on
> Cloud Hypervisor v53) are in place. The `ch-agent` now serves the `HostAgent`
> gRPC API backed by a real Cloud Hypervisor process manager: a gRPC client can
> create / start / stop / delete VMs, and **VMs survive an agent restart** — the
> agent recovers its inventory and re-attaches to already-running VMMs
> (design section 11). Verified end-to-end on a host with `/dev/kvm`. The
> control plane, database, networking and UI land in later milestones — see
> [`DESIGN.md`](DESIGN.md) section 42.

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

## Design & invariants

The full design lives in [`DESIGN.md`](DESIGN.md). The load-bearing
architectural decisions are recorded as ADR-001 … ADR-015 in section 44; code
comments reference design sections by number.

## License

Licensed under the [Apache License 2.0](LICENSE).
