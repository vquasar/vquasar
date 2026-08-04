# Windows guests on ch-orchestrator

This platform runs Windows guests on Cloud Hypervisor. Two properties of Cloud
Hypervisor shape the whole workflow and are worth understanding up front:

- **Headless.** Cloud Hypervisor has no emulated VGA/GPU and no VNC/SPICE
  console — the only console is the serial port. There is no way to drive the
  graphical Windows Setup interactively.
- **virtio-only.** The only disk and network devices are virtio-blk and
  virtio-net. There is no emulated SATA/IDE/AHCI or e1000. Windows has no
  in-box virtio drivers, so it cannot see a virtio-blk disk (including a
  virtio-blk install ISO) until those drivers are loaded.

Together these mean **you cannot run a stock interactive Windows install on
Cloud Hypervisor.** The supported paths are below.

## What the platform provides

- **UEFI firmware boot.** Select *Firmware* boot with the EDK II firmware at
  `/var/lib/ch-orchestrator/shared/firmware/CLOUDHV.fd`. This reaches the UEFI
  environment (verified: EDK II UEFI v2.70 shell), which is what the Windows
  boot manager needs.
- **Read-only ISO/CD attachment.** Any `*.iso` placed under the image store's
  `isos/` directory is listed by `GET /api/v1/isos` and can be attached to a VM
  read-only (as a virtio-blk device). Use this for the Windows install ISO and
  the virtio-win driver ISO.
- **The virtio-win driver ISO** is staged at
  `<images>/isos/virtio-win.iso` (Fedora's redistributable build). It carries
  the virtio-blk (`viostor`/`vioscsi`), virtio-net (`NetKVM`), balloon, and
  serial drivers Windows needs.
- **The "Windows guest preset"** in *Create VM* scaffolds a Windows-shaped VM:
  UEFI firmware boot, a blank virtio system disk, 2 vCPU / 4 GiB, and the
  virtio-win ISO attached read-only. Add your Windows install ISO to the
  attachment list.

## Path A — bring a pre-built image (recommended)

Build the Windows image once on a hypervisor that has a graphical console
(e.g. QEMU/virt-manager), installing the virtio drivers during setup, then run
it here:

1. On a QEMU host, install Windows onto a qcow2/raw disk, loading the
   virtio-win `viostor` driver when Setup asks for a disk, and install NetKVM +
   the guest agent afterwards.
2. Copy the resulting disk to shared storage and register it as an image with
   **Firmware** boot (`CLOUDHV.fd`).
3. Create a VM from that image (or attach the disk to a firmware-boot VM). It
   boots straight to Windows over virtio — no console needed.

## Path B — unattended serial install

Windows Server can install and run **headless over the serial port** using SAC
(Special Administration Console) and EMS, driven by an `autounattend.xml`:

1. Author an `autounattend.xml` that: enables EMS/SAC on the serial port,
   loads the virtio storage driver from the attached virtio-win ISO, and
   partitions/installs unattended onto the virtio system disk.
2. Create a firmware-boot VM with three read-only ISOs attached — the Windows
   install ISO, the virtio-win ISO, and a small ISO containing
   `autounattend.xml` — plus the blank virtio system disk.
3. Watch progress on the serial console. This is involved and version-specific;
   Path A is easier when a graphical hypervisor is available.

## Notes

- Give Windows ≥ 2 vCPU and ≥ 4 GiB RAM.
- Keep the machine type **standard** (not microVM — Windows needs the full
  device model and firmware boot; the microVM profile forbids firmware).
- ACPI is always on (Cloud Hypervisor requires it), which Windows needs.
