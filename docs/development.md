# Development

The local loop for working on vquasar itself.

## Toolchain and checks

Stable Rust (see [`../rust-toolchain.toml`](../rust-toolchain.toml)) plus
`protoc` — `tonic-build` needs it to generate the agent gRPC bindings
(`apt install protobuf-compiler`). Node for the UI (CI uses Node 20).

```bash
cargo build --workspace
cargo test  --workspace --lib --bins
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cargo deny check          # requires: cargo install cargo-deny
```

CI ([`../.github/workflows/ci.yml`](../.github/workflows/ci.yml)) runs the same
set on a pinned toolchain, plus the e2e suite and the UI build. Pin bumps are
deliberate — clippy lint sets drift between releases.

## Database

The control plane applies its own migrations from
[`../migrations/`](../migrations/) on boot, so a fresh empty database is enough:

```bash
docker run -d --name vquasar-pg -p 5432:5432 \
  -e POSTGRES_USER=ch -e POSTGRES_PASSWORD=ch -e POSTGRES_DB=vquasar postgres:16
```

All queries use `sqlx`'s runtime API, not the compile-time-checked macros, so
`cargo build` and CI never need a database.

## Running control and agent by hand

Each binary takes `--config <file.toml>` (or `VQUASAR_CONTROL_CONFIG` /
`VQUASAR_AGENT_CONFIG`) and merges environment overrides on top:
`VQUASAR_CONTROL_` / `VQUASAR_AGENT_` prefix, `__` between nesting levels. So
`[server] listen` on control is `VQUASAR_CONTROL_SERVER__LISTEN`, and
`[hypervisor] binary` on the agent is `VQUASAR_AGENT_HYPERVISOR__BINARY`. See
[`../config/control.toml`](../config/control.toml) and
[`../config/agent.toml`](../config/agent.toml) for the full shape.

```bash
# Control plane.
VQUASAR_CONTROL_DATABASE__URL=postgres://ch:ch@127.0.0.1:5432/vquasar \
VQUASAR_CONTROL_SERVER__LISTEN=127.0.0.1:8080 \
cargo run -p vquasar-control

# Agent, on a host with /dev/kvm and cloud-hypervisor.
VQUASAR_AGENT_AGENT__NAME=host-01 \
VQUASAR_AGENT_GRPC__LISTEN=127.0.0.1:9500 \
VQUASAR_AGENT_HYPERVISOR__BINARY=/var/lib/vquasar/bin/cloud-hypervisor \
VQUASAR_AGENT_HYPERVISOR__RUNTIME_DIR=/var/lib/vquasar \
cargo run -p vquasar-agent

# Register the agent with the control plane.
curl -X POST localhost:8080/api/v1/hosts -H 'content-type: application/json' \
  -d '{"name":"host-01","endpoint":"http://127.0.0.1:9500"}'
```

Neither TLS nor OIDC is on unless configured, which is what makes this loop
short. Both are required for anything beyond a trusted lab — see
`scripts/install.sh --help`.

Agents that manage TAPs and OVS must run as root; run the agent under `sudo -E`
(or set the environment inside the sudo invocation) when working on networking.

## The UI

```bash
cd ui
npm install
npm run dev        # http://localhost:5173
npm run typecheck
npm run build      # tsc --noEmit && vite build -> ui/dist
```

The dev server proxies `/api` (including the console WebSocket upgrade) to
`http://127.0.0.1:8080`; override with `VQUASAR_CONTROL_URL`. To serve the built
bundle from the control plane instead, point it at the output:

```bash
VQUASAR_CONTROL_SERVER__UI_DIR=$(pwd)/ui/dist cargo run -p vquasar-control
```

The UI is strictly API-only (ADR-015): every page talks to `/api/v1` and holds
no authoritative state of its own. It is React 18 + TypeScript + Vite + MUI,
with MUI DataGrid for resource tables, React Query for polling-based live state,
xterm.js for the serial console, and `oidc-client-ts` for Authorization Code +
PKCE login. DESIGN.md §34 suggested plain React/Vite; MUI is a deliberate,
recorded deviation for the DataGrid and the Material language, not an ADR
change.

## Tests

* **Unit tests** — `cargo test --workspace --lib --bins`. The `Hypervisor`
  trait has a `FakeHypervisor` implementation, so most agent logic is testable
  without Cloud Hypervisor.
* **End-to-end** — [`../services/control/tests/e2e.rs`](../services/control/tests/e2e.rs)
  spawns the real `vquasar-control` binary against a throwaway database while
  the test itself acts as the host agent (an in-process tonic `HostAgent` with
  in-memory VM state). It covers REST → reconcile → gRPC for VM lifecycle,
  scheduling, migration and drain, with no hardware:

  ```bash
  E2E_PG_ADMIN_URL=postgres://ch:ch@127.0.0.1:5432/postgres \
    cargo test -p vquasar-control --test e2e -- --test-threads=1
  ```

  It creates a uniquely-named database per run and drops it on teardown. Auth
  is disabled, so no IdP is needed.
* **Boot integration** — `crates/client/tests/boot_integration.rs` boots a real
  VM. Opt-in: it skips unless its `CH_IT_*` environment variables are set.

## Example binaries

Two examples drive the lower layers directly, without a control plane.

`boot_vm` (in `vquasar-client`) launches a real `cloud-hypervisor` through the
`Hypervisor` trait and waits for a marker on the serial console — see
[`booting-vms.md`](booting-vms.md).

`agent_client` (in `vquasar-agent`) drives a running agent over its `HostAgent`
gRPC API, the same way the control plane does:

```bash
cargo run -p vquasar-agent --example agent_client -- host-info
cargo run -p vquasar-agent --example agent_client -- list
cargo run -p vquasar-agent --example agent_client -- ensure \
  --vm-id "$(cat /proc/sys/kernel/random/uuid)" --name demo \
  --kernel /var/lib/vquasar/images/vmlinuz-<ver> \
  --initramfs /var/lib/vquasar/images/initrd.img-<ver> \
  --disk /var/lib/vquasar/volumes/demo.raw \
  --readonly-disk /var/lib/vquasar/seed/seed.iso
cargo run -p vquasar-agent --example agent_client -- delete <vm-id>
```

`--endpoint` defaults to `http://127.0.0.1:9500`. Subcommands: `host-info`,
`list`, `get`, `ensure`, `start`, `stop`, `delete`.

[`../scripts/agent-restart-demo.sh`](../scripts/agent-restart-demo.sh) scripts
the restart-survival scenario end to end — start a VM, kill the agent, confirm
the VM keeps running, restart the agent, confirm it re-attaches to the same
process, then delete. (It still refers to the pre-rename `ch-agent` crate and
needs updating before it will run.)

## Conventions

* Code comments reference [`../DESIGN.md`](../DESIGN.md) sections by number;
  keep that habit, and update DESIGN.md when a decision changes.
* Load-bearing decisions are ADRs (DESIGN.md section 44). Deviating from one is
  a design change, not an implementation detail.
* Cloud Hypervisor's wire types stay inside `vquasar-client` (ADR-013). The
  orchestration API speaks `vquasar-model` types.
