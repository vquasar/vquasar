# Prerequisites

What each machine needs before [installing](install.md). Everything here was
taken from what the code actually runs and binds, not from a template — if
something is listed, removing it breaks a feature named beside it.

Two roles. One machine can be both, which is what a single-host lab is.

| | Control plane | Hypervisor host |
| --- | --- | --- |
| Runs | API, console, scheduler, reconcile loop | VMs, their networking and storage |
| How many | one (or several behind a load balancer, [ADR-021](control-plane-ha.md)) | one per machine that runs VMs |
| Needs virtualisation | no | yes |

## Both roles

* **x86-64 Linux with systemd.** The installer writes systemd units; there is
  no other supervision path.
* **glibc 2.35 or newer** for the published binaries — Ubuntu 22.04+, Debian 12,
  RHEL 9. Older distributions can build from source.
* **A synchronised clock.** Certificate validity, the controller lease
  ([ADR-021](control-plane-ha.md)) and the reconcile heartbeat are all
  wall-clock judgements; a host minutes out of step will lose a lease it holds
  or hold one it has lost. Run `chrony` or `systemd-timesyncd`.
* **`openssl`** — certificate handling on both sides.

## Control plane

### PostgreSQL 13 or newer

Not optional: the control plane keeps all desired state there and applies
migrations at start-up. 13 is the floor because the schema uses
`gen_random_uuid()` from core (it was an extension before), alongside `JSONB`,
expression and partial unique indexes, `ON CONFLICT` and `LATERAL`.

It can live anywhere the control plane can reach. Encrypt the connection —
the driver's default silently accepts plaintext, which is why
[Encrypting the connection to PostgreSQL](postgres-tls.md) exists.

### Commands

| Command | Used for |
| --- | --- |
| `qemu-img` | provisioning and snapshotting volumes on shared storage |
| `curl` | importing an image from a URL |
| `openssl` | signing agent certificates during enrolment (M16) |

`apt install qemu-utils curl openssl` — or `dnf install qemu-img curl openssl`.

### Storage

The control plane writes volumes to a storage pool it can reach itself
(typically the NFS server, or the same shared mount the agents have). Volumes in
a pool that is *local to a host* are built by that host instead, so the control
plane does not need to see those — see [storage pools](storage-pools.md).

## Hypervisor hosts

### Virtualisation

* **`/dev/kvm`** — hardware virtualisation enabled in firmware (VT-x / AMD-V),
  the `kvm_intel` or `kvm_amd` module loaded, and the agent's user able to open
  the device. Cloud Hypervisor will not start without it.
* **Cloud Hypervisor v53 or newer**, at `/var/lib/vquasar/bin/cloud-hypervisor`
  or wherever `--ch-binary` points. v53 is the floor because vquasar sets the
  disk image type explicitly rather than relying on the auto-detection that
  older builds needed.
* **UEFI firmware** (`CLOUDHV.fd`) for firmware boot; direct-kernel boot needs
  none. [`scripts/build-cloudhv-firmware.sh`](../scripts/README.md) builds one.

### Commands

| Command | Used for |
| --- | --- |
| `qemu-img` | per-VM disks: create, clone, resize |
| `xorriso` | building the cloud-init NoCloud seed ISO |
| `ovs-vsctl`, `ovs-ofctl` | bridges, ports, VXLAN tunnels, and the security-group flows |
| `ip` | TAP devices and addresses |
| `nft` | the host firewall ([design §30](../DESIGN.md)) |
| `mount` | mounting an `nfs` storage pool the agents own |
| `ping` | reachability checks between overlay peers |
| `cp` | cloning a raw disk (with `--reflink=auto`) |

On Debian or Ubuntu:

```bash
apt install qemu-utils xorriso openvswitch-switch iproute2 nftables nfs-common
```

`nfs-common` only if you use `nfs` pools; `xorriso` only if guests use
cloud-init, which in practice they do.

### Open vSwitch

Required for all VM networking, with an integration bridge in place before the
agent starts. [`scripts/setup-ovs.sh`](../scripts/README.md) installs the
package and creates it.

For encrypted VXLAN ([overlay encryption](overlay-encryption.md)) you also need
`openvswitch-ipsec`. Without it, tenant overlays run in cleartext on the
underlay — the control plane warns about this at every start rather than
letting it pass quietly.

## Network

Open between the machines, not to the world:

| Port | Protocol | From → to | For |
| --- | --- | --- | --- |
| 8080 | TCP | operators, agents → control | API, console, cloud-init `phone_home` |
| 9500 | TCP | control → agent | the agent's gRPC API (mutual TLS) |
| 9600–9700 | TCP | agent → agent | live migration (`[migration] port_min`/`port_max`) |
| 4789 | UDP | agent ↔ agent | VXLAN, when tenant networks are used |
| 500, 4500 | UDP + ESP | agent ↔ agent | IPsec, when overlay encryption is on |
| 5432 | TCP | control → PostgreSQL | wherever the database lives |

The migration range is a range because several migrations can be in flight at
once; narrowing it narrows how many.

There is deliberately **no control-plane gRPC port**. The control plane dials
the agents; nothing dials it over gRPC.

## Authentication

Optional to install, and the installer makes you say so: without
`--allow-no-auth` it refuses to bring up a control plane that anyone can talk
to. For anything beyond a lab you want an OIDC provider —
[Authentication and RBAC](oidc-keycloak.md) sets one up with Keycloak as the
reference, and [`scripts/keycloak-setup.sh`](../scripts/README.md) does the
realm.

## Building from source

Only needed if you are not installing a published build: a stable Rust
toolchain (see [`rust-toolchain.toml`](../rust-toolchain.toml)),
`protobuf-compiler` for the gRPC definitions, and Node 20+ for the console.
`make check` runs everything CI runs.
