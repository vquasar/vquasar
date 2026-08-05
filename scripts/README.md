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
- `prepare-ubuntu-image.sh` — download the latest Ubuntu cloud image, convert to
  raw, extract its kernel/initrd, and build a NoCloud cloud-init seed. Produces
  everything `ch-client`'s `boot_vm` example needs. See the README for usage.
