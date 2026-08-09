# vquasar documentation

Guides for running and developing vquasar. Start at the
[project README](../README.md) for what it is and a quickstart.

## Installing

* [**Prerequisites**](prerequisites.md) — what each machine needs before you
  start: packages, ports, PostgreSQL, KVM and Cloud Hypervisor, and why each
  one is on the list.
* [**Installing vquasar**](install.md) — one command per machine, the release
  channels, what the installer verifies (and what it does not), upgrades, and
  the pieces it deliberately leaves to you.

## Operating

* [**Booting VMs**](booting-vms.md) — host prerequisites, image preparation,
  the firmware (UEFI) and direct-kernel boot styles, attaching a VM to a
  network, and live migration.
* [**Running more than one control plane**](control-plane-ha.md) — what an
  extra instance needs (one certificate CN, a stable address in front), how to
  tell who is leading, and what happens when the leader goes away.
* [**Storage pools**](storage-pools.md) — where volumes put their bytes, why
  reachability is reported by the hosts rather than configured, what `pending`
  means, and how to reclaim files whose VM is long gone.
* [**Windows guests**](windows-guests.md) — what Cloud Hypervisor's headless,
  virtio-only device model means for Windows, and the two workable install
  paths.

## Securing an install

* [**Authentication and RBAC**](oidc-keycloak.md) — wiring an OIDC provider
  (Keycloak as the reference), mapping its groups to vquasar roles, the first-
  admin bootstrap, and how to verify the result.
* [**Projects and multi-tenancy**](multi-tenancy.md) — what a project owns,
  how a request picks one, why a role binding names the project it applies in,
  and what is still missing before it bounds consumption as well as access.
* [**Security hardening**](security-hardening.md) — three defects found in
  review (an agent certificate that was also a control credential, cloud-init
  secrets returned to any reader, unconfined host paths in a VM spec), what
  changed, and what to check when upgrading.
* [**Encrypting the VXLAN underlay**](overlay-encryption.md) — why tenant
  networks are not isolated until this is on, the two-step MTU-then-IPsec
  rollout, and how to verify encryption is actually happening.
* [**Encrypting the connection to PostgreSQL**](postgres-tls.md) — why the
  driver's default silently accepts plaintext, and how to make TLS mandatory
  and verified.

## Developing

* [**Development**](development.md) — toolchain and checks, running control and
  agent by hand, configuration and environment overrides, the UI dev loop,
  tests, and the example binaries.

## Reference

* [`../scripts/README.md`](../scripts/README.md) — what each helper script
  does: systemd install/uninstall, OVS setup, image prep, certificate
  generation, Keycloak realm setup, firmware build.
* [`../DESIGN.md`](../DESIGN.md) — the architecture in full. Section 44 records
  the load-bearing decisions as ADR-001 … ADR-025; code comments cite design
  sections by number.
* [`../ROADMAP.md`](../ROADMAP.md) — what has landed, and what is queued.
* [`../proto/agent.proto`](../proto/agent.proto) — the control ↔ agent gRPC
  contract.
* [`../migrations/`](../migrations/) — the control-plane database schema.

There is no generated API reference yet; the REST surface is defined by the
handlers under [`../services/control/src/api/`](../services/control/src/api/).
