# Booting VMs

What a host needs before vquasar can start a guest on it, and the two boot
styles the platform models. For the platform-level view see
[`../DESIGN.md`](../DESIGN.md) sections 21 and 24.

## Host prerequisites

Each hypervisor host needs:

* `/dev/kvm` and a `cloud-hypervisor` binary (the agent's
  `hypervisor.binary`, default `/var/lib/vquasar/bin/cloud-hypervisor`).
* Open vSwitch and the integration bridge, if VMs will have NICs:

  ```bash
  scripts/setup-ovs.sh --bridge br-int
  ```

  The script installs OVS, creates the bridge, and opens UDP 4789 on firewalld
  for VXLAN overlays. The agent must run privileged to manage TAPs and OVS.
* A guest image. `scripts/prepare-ubuntu-image.sh` downloads the latest Ubuntu
  cloud image, converts it to raw, extracts the guest's own kernel and initrd,
  and builds a NoCloud cloud-init seed ISO — everything either boot style
  needs:

  ```bash
  scripts/prepare-ubuntu-image.sh [--release 26.04] [--base-dir /var/lib/vquasar] \
                                  [--password ubuntu]
  ```

  Requires `curl`, `python3`, `qemu-img`, `qemu-nbd` (sudo) and `genisoimage`.

## The two boot styles

Both are first-class in the model ([`BootSpec`](../crates/model/src/vm.rs)) and
both are verified booting an Ubuntu cloud image.

### Firmware (UEFI)

`BootSpec::Firmware` boots the disk image's *own* bootloader through the EDK2
`CLOUDHV.fd` firmware — a full modern UEFI (shim → GRUB → kernel, Secure Boot
infrastructure present). No kernel extraction: point it at the disk and go.
This is the path for whole cloud images, and the only path for Windows guests.

Build the firmware once per host (or build once and copy):

```bash
scripts/build-cloudhv-firmware.sh [--base-dir /var/lib/vquasar] \
                                  [--edk2-tag edk2-stable202502]
```

Cloud Hypervisor's `--firmware` requires this **CloudHv** EDK2 build
specifically. The QEMU OVMF packages (`/usr/share/OVMF/*`) are not compatible
and fail with `KernelLoad(Bzimage(InvalidBzImage))`;
rust-hypervisor-firmware is a PVH firmware and cannot complete modern Ubuntu's
shim/GRUB chain. Hence `CLOUDHV.fd`.

### Direct kernel

`BootSpec::DirectKernel` boots a kernel (bzImage or PVH `vmlinux`) plus an
optional initrd with an explicit command line. `prepare-ubuntu-image.sh`
extracts the image's own kernel and initrd for exactly this. Use it when you
want to pin the kernel or its cmdline; it is also the fastest path for microVMs,
whose profile (`machine_type = "microvm"`) *requires* direct-kernel boot.

## Booting one by hand

The `boot_vm` example in `vquasar-client` launches a real `cloud-hypervisor`
through the same `Hypervisor` trait the agent uses, then tails the serial
console until a marker string appears. It is the quickest way to prove a host's
image and firmware assets are good, with no control plane involved.

```bash
cp --reflink=auto /var/lib/vquasar/images/ubuntu-26.04.raw \
                  /var/lib/vquasar/volumes/vm01.raw

cargo run -p vquasar-client --example boot_vm -- \
  --binary        /var/lib/vquasar/bin/cloud-hypervisor \
  --kernel        /var/lib/vquasar/images/vmlinuz-<ver> \
  --initramfs     /var/lib/vquasar/images/initrd.img-<ver> \
  --cmdline       "root=/dev/vda1 rw console=ttyS0" \
  --disk          /var/lib/vquasar/volumes/vm01.raw \
  --readonly-disk /var/lib/vquasar/seed/seed.iso \
  --runtime-dir   /var/lib/vquasar/vms/vm01
```

For UEFI boot, drop `--kernel`/`--initramfs`/`--cmdline` and pass
`--firmware /var/lib/vquasar/firmware/CLOUDHV.fd` instead. Other options:
`--cpus`, `--memory-mib`, `--wait-secs`, `--marker`, `--keep-running`
(`--help` lists them all). On success it prints
`BOOT OK — marker observed on serial console`.

The same flow is covered by the opt-in integration test
`crates/client/tests/boot_integration.rs`, which skips unless its `CH_IT_*`
environment variables are set.

## Booting one through the API

With an agent registered, `POST /api/v1/vms` persists the desired state and
returns a task id; the reconcile loop does the work. A direct-kernel example:

```bash
curl -X POST localhost:8080/api/v1/vms -H 'content-type: application/json' -d '{
  "name":"demo",
  "spec":{"desired_power_state":"Running","cpu":{"boot_vcpus":2,"max_vcpus":2},
    "memory":{"size_mib":2048},
    "boot":{"type":"direct_kernel",
      "kernel":"/var/lib/vquasar/images/vmlinuz-<ver>",
      "initramfs":"/var/lib/vquasar/images/initrd.img-<ver>",
      "cmdline":"root=/dev/vda1 rw console=ttyS0"},
    "disks":[{"path":"/var/lib/vquasar/volumes/demo.raw"},
             {"path":"/var/lib/vquasar/seed/seed.iso","readonly":true}],
    "network_interfaces":[],"placement":{}}}'
```

Poll `GET /api/v1/vms/{id}` until its phase is `Running`. In practice you would
register an image (or import one) and create VMs from an image or template
rather than hand-writing paths; the UI does this.

## Networking a VM

Define a network, then reference it from a NIC:

```bash
curl -X POST localhost:8080/api/v1/networks -d '{"name":"provider"}'
curl -X POST localhost:8080/api/v1/networks -d '{"name":"vlan-100","vlan":100}'
# ... "network_interfaces":[{"network_id":"<id>"}] ... in the VM spec
```

The control plane allocates a MAC deterministically from the VM id and NIC
index ([`netalloc.rs`](../services/control/src/netalloc.rs)), allocates an IP if
the network has a subnet, and sends per-NIC bindings to the agent. The agent's
`OvsNetworkBackend` ([`network.rs`](../services/agent/src/network.rs)) creates
`tap<vmid8><idx>`, brings it up, and attaches it to the bridge (VLAN-tagged, or
on a per-VNI `vxbr<vni>` bridge for an overlay network). TAP names are derived
from the VM id, so teardown works even for a VM the agent re-attached to after a
restart, with no persisted per-NIC state.

## Live migration

`POST /api/v1/vms/{id}/migrate` with `{"target_host_id":"..."}` moves a running
VM, assuming shared storage — the same disk path must resolve on both hosts.
Migration is a persisted state machine (Pending → Sending → Finalizing →
Completed/Failed) advanced one step per reconcile tick, so it survives a
control-plane restart. On failure the VM stays on its source host. The
destination's CPU must be feature-compatible with the source (same vendor,
superset of the curated guest-ISA feature set) unless the migration is forced.

For a single-host lab with two co-located agents, run the agents with
`hypervisor.serial_mode = "file"`: they share a filesystem, so the serial path
carried in the migrated config would otherwise collide. Separate hosts use
identical path strings on distinct filesystems and need no workaround.
