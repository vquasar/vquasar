# ch-orchestrator roadmap

Status of the platform and the backlog beyond the initial MVP. The originally
scoped MVP (design milestones M0–M8) is complete and verified on real
distributed hardware; M9–M11 and the operational hardening below were added on
top.

## Done

### MVP (M0–M8)
- **M0/M1** — Cargo workspace, stable domain model, `ch-client` Cloud Hypervisor
  adapter; boots real Ubuntu VMs via direct-kernel and UEFI firmware.
- **M2** — Host agent: `HostAgent` gRPC, CH process manager, restart survival.
- **M3** — Control plane: PostgreSQL, REST API, scheduler, reconcile loop.
- **M4** — Open vSwitch dataplane: TAP/OVS ports, deterministic MAC allocation.
- **M5** — Web UI: React + TypeScript + MUI.
- **M6** — Interactive serial console (browser WS → control → agent gRPC → VM).
- **M7** — Multi-host scheduling by committed capacity.
- **M8** — Shared-storage live migration.

### Beyond the MVP
- **M9** — Disk provisioning, images catalog, VM templates, cloud-init
  (generated + raw user-data), blank data disks.
- **M10** — Live VM editing: CPU/memory hot-plug, add disk/NIC, rename, disk
  grow, true power-off; edit for networks/images/templates.
- **M11** — Agentless guest-IP discovery (host ARP/neighbor snooping).
- **Operations** — systemd units + install/uninstall scripts; OVS + `br-int` on
  every host; flat-bridge to the lab LAN made reboot-persistent via
  NetworkManager-ovs; distributed cluster verified (create/boot, cross-host
  console, capacity scheduling, restart survival, same-CPU live migration);
  Ubuntu (direct-kernel) and Rocky (UEFI firmware) guests.

## Backlog

Ordered roughly by priority. **M12 (security) is the top priority** — the
platform is currently unauthenticated and its internal control plane is
plaintext, so it must not leave a trusted lab until this lands.

### M12 — Security (next)
- **Authentication & IAM/RBAC.** Multi-user with built-in general roles
  (e.g. admin / operator / viewer) *and* user-defined granular custom roles —
  full RBAC. Identity via an external **OIDC** provider (Keycloak as the
  reference IdP): the API becomes an OAuth2 resource server validating JWTs;
  the UI does OIDC Authorization Code + PKCE. Authorization (roles →
  permissions over our resources, scoped to projects) lives in `ch-control`.
- **mTLS everywhere internal.** Control ↔ agent gRPC and the control API/console
  must be TLS; the control↔agent plane uses **mutual TLS** with an internal CA
  and per-agent certificates issued at enrollment. No plaintext on the wire.
- **Encryption of sensitive data at rest.** Application-level envelope
  encryption for sensitive fields (cloud-init passwords / SSH keys / user-data,
  and any stored credentials/secrets), master key from config/KMS; TLS to
  PostgreSQL; DB host disk encryption (ops).

### Networking
- IPAM: control-plane-managed / static IP assignment (stop relying on external
  DHCP).
- VLAN-isolated and cross-host L2 networks via a VXLAN (or similar) overlay.
- Security groups / per-tenant network isolation.
- Change an existing NIC's network on a running VM (needs a detach/reattach or
  recreate path; CH cannot re-tag a live TAP).
- Cloud-init `phone_home` as a fallback for guest-IP discovery when a guest
  filters ICMP or sits off the flat L2 (see the corresponding memory note).

### Storage
- First-class **Volumes API**: create/attach/detach/delete/list independent of a
  VM.
- **Image lifecycle**: upload / download / build workflow (images are currently
  pre-placed files registered by path).
- Snapshots and backups.
- Additional storage backends and per-VM storage policy.

### Compute & lifecycle
- Cross-CPU live migration (common CPU-model masking to bridge heterogeneous
  hosts; CH offers no masking today).
- microVMs.
- Windows guest support (spike exists, not integrated).
- Per-VM metrics/stats (CPU / memory / disk / network).

### Platform & resilience
- Control-plane HA (multiple control nodes; PostgreSQL HA).
- Host lifecycle: maintenance mode, drain/evacuate, and automated enrollment
  (ties into M12 certificate issuance).
- End-to-end host-reboot recovery validation.
- Quotas, projects, multi-tenancy.

### Quality & delivery
- Automated integration/e2e tests against a cluster; fix the known flaky
  parallel tempdir-teardown unit test.
- Metrics/tracing export (the events table is basic).
- `curl | sh` bootstrap installer (the install scripts are structured for it).
- Keep DESIGN.md and API reference docs current with M9–M12.
