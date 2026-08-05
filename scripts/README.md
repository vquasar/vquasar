Helper scripts (design document, section 39).

- `install.sh <agent|control> [options]` — install a component as a systemd
  service: copies the binary to `/usr/local/bin`, writes an `EnvironmentFile`
  under `/etc/vquasar`, generates the unit, and enables + starts it.
  Self-contained (units + config generated inline) so it can back a future
  `curl … | sh` bootstrap. The agent unit uses `KillMode=process` so running
  VMs survive an agent restart (section 11). Run as root. `--help` for options.
- `uninstall.sh <agent|control|all> [--purge]` — stop/disable and remove the
  unit + binary; `--purge` also removes the config. Never touches
  `/var/lib/vquasar` (VM disks/volumes/shared storage).
- `setup-ovs.sh` — install Open vSwitch and create the integration bridge.
- `gen-certs.sh` — internal CA + control and per-agent certificates for the
  mutual TLS between the control plane and the agents (design M12a).
- `keycloak-setup.sh` — create the Keycloak realm, PKCE client, protocol mappers
  and role groups the control plane's OIDC authentication expects (design M12b).
  Idempotent; reads the admin password from `$KEYCLOAK_ADMIN_PASSWORD`. See
  [`../docs/oidc-keycloak.md`](../docs/oidc-keycloak.md).
- `prepare-ubuntu-image.sh` — download the latest Ubuntu cloud image, convert to
  raw, extract its kernel/initrd, and build a NoCloud cloud-init seed. Produces
  everything `vquasar-client`'s `boot_vm` example needs. See the README for usage.
