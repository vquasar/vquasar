#!/usr/bin/env bash
# Prepare a Ubuntu cloud image for Cloud Hypervisor direct-kernel boot.
#
# This captures the Milestone 1 image workflow (design document, sections 21,
# 24). It:
#   1. resolves and downloads the latest Ubuntu cloud image for a release,
#   2. converts it to raw (Cloud Hypervisor's most robust disk format),
#   3. extracts the guest's own kernel + initrd from the image (so direct-kernel
#      boot uses the image's kernel — a first-class citizen alongside the disk),
#   4. builds a NoCloud cloud-init seed ISO so the guest is usable on serial.
#
# Both artifacts — the raw cloud image AND its kernel/initrd — are produced so
# either boot style works. Requires: curl, python3, qemu-img, qemu-nbd (sudo),
# genisoimage.
#
# Usage:
#   scripts/prepare-ubuntu-image.sh [--release 26.04] [--base-dir DIR] [--password PW]
set -euo pipefail

RELEASE="26.04"
BASE_DIR="/var/lib/vquasar"
PASSWORD="ubuntu"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --release)  RELEASE="$2"; shift 2 ;;
    --base-dir) BASE_DIR="$2"; shift 2 ;;
    --password) PASSWORD="$2"; shift 2 ;;
    -h|--help)  grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

IMAGES="$BASE_DIR/images"
SEED="$BASE_DIR/seed"
mkdir -p "$IMAGES" "$SEED"

echo "==> Resolving latest Ubuntu $RELEASE cloud image (amd64)"
read -r IMG_URL IMG_NAME < <(
  curl -fsSL "https://cloud-images.ubuntu.com/releases/streams/v1/com.ubuntu.cloud:released:download.json" \
  | python3 - "$RELEASE" <<'PY'
import json, sys
release = sys.argv[1]
data = json.load(sys.stdin)
for name, prod in data["products"].items():
    if prod.get("arch") != "amd64":
        continue
    if release not in prod.get("release_title", ""):
        continue
    latest = sorted(prod["versions"])[-1]
    for _, meta in prod["versions"][latest]["items"].items():
        if meta.get("ftype") in ("disk1.img", "disk-kvm.img"):
            print("https://cloud-images.ubuntu.com/" + meta["path"], meta["path"].split("/")[-1])
            sys.exit(0)
sys.exit("could not resolve image for release " + release)
PY
)
echo "    $IMG_URL"

QCOW="$IMAGES/ubuntu-$RELEASE.qcow2"
RAW="$IMAGES/ubuntu-$RELEASE.raw"
echo "==> Downloading $IMG_NAME"
curl -fsSL -o "$QCOW" "$IMG_URL"

echo "==> Converting qcow2 -> raw"
qemu-img convert -O raw "$QCOW" "$RAW"

echo "==> Extracting kernel + initrd from the image (via qemu-nbd)"
sudo modprobe nbd max_part=16
NBD=/dev/nbd0
cleanup() {
  sudo umount /mnt/ch-boot 2>/dev/null || true
  sudo qemu-nbd --disconnect "$NBD" >/dev/null 2>&1 || true
}
trap cleanup EXIT
sudo qemu-nbd --disconnect "$NBD" >/dev/null 2>&1 || true
sudo qemu-nbd --connect="$NBD" -r -f raw "$RAW"
sleep 2
# The kernel/initrd live on the ext4 partition labelled BOOT (or on rootfs when
# there is no separate /boot). Find the partition that actually has /boot.
sudo mkdir -p /mnt/ch-boot
BOOTPART=""
for p in "${NBD}p13" "${NBD}p1"; do
  [[ -e "$p" ]] || continue
  sudo mount -o ro "$p" /mnt/ch-boot 2>/dev/null || continue
  if ls /mnt/ch-boot/vmlinuz-* >/dev/null 2>&1 || ls /mnt/ch-boot/boot/vmlinuz-* >/dev/null 2>&1; then
    BOOTPART="$p"; break
  fi
  sudo umount /mnt/ch-boot
done
[[ -n "$BOOTPART" ]] || { echo "could not locate /boot in image" >&2; exit 1; }
BOOTDIR=/mnt/ch-boot
[[ -e "$BOOTDIR/vmlinuz-"* ]] || BOOTDIR=/mnt/ch-boot/boot
KVER=$(ls "$BOOTDIR"/vmlinuz-* | sed 's|.*/vmlinuz-||' | sort -V | tail -1)
sudo cp "$BOOTDIR/vmlinuz-$KVER"    "$IMAGES/vmlinuz-$KVER"
sudo cp "$BOOTDIR/initrd.img-$KVER" "$IMAGES/initrd.img-$KVER"
sudo chown "$(id -un):$(id -gn)" "$IMAGES/vmlinuz-$KVER" "$IMAGES/initrd.img-$KVER"
chmod 0644 "$IMAGES/vmlinuz-$KVER" "$IMAGES/initrd.img-$KVER"
cleanup
trap - EXIT

echo "==> Building NoCloud cloud-init seed (serial-loginable)"
cat > "$SEED/meta-data" <<EOF
instance-id: ch-demo-0001
local-hostname: ch-demo
EOF
cat > "$SEED/user-data" <<EOF
#cloud-config
hostname: ch-demo
password: $PASSWORD
chpasswd:
  expire: false
ssh_pwauth: true
final_message: "VQUASAR-BOOT-OK cloud-init finished after \$UPTIME seconds"
EOF
genisoimage -quiet -output "$SEED/seed.iso" -volid cidata -joliet -rock \
  "$SEED/user-data" "$SEED/meta-data"

cat <<EOF

Done. Artifacts in $BASE_DIR:
  raw disk : $RAW
  kernel   : $IMAGES/vmlinuz-$KVER
  initrd   : $IMAGES/initrd.img-$KVER
  seed ISO : $SEED/seed.iso

Boot a VM through the Hypervisor trait with:
  cargo run -p vquasar-client --example boot_vm -- \\
    --binary        $BASE_DIR/bin/cloud-hypervisor \\
    --kernel        $IMAGES/vmlinuz-$KVER \\
    --initramfs     $IMAGES/initrd.img-$KVER \\
    --cmdline       "root=/dev/vda1 rw console=ttyS0 systemd.mask=systemd-networkd-wait-online.service" \\
    --disk          $BASE_DIR/volumes/<vm>.raw \\
    --readonly-disk $SEED/seed.iso \\
    --runtime-dir   $BASE_DIR/vms/<vm>
EOF
