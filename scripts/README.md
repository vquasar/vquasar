Helper scripts (design document, section 39).

- `prepare-ubuntu-image.sh` — download the latest Ubuntu cloud image, convert to
  raw, extract its kernel/initrd, and build a NoCloud cloud-init seed. Produces
  everything `ch-client`'s `boot_vm` example needs. See the README for usage.
