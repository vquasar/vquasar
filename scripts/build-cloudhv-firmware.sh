#!/usr/bin/env bash
# Build the EDK2 CLOUDHV.fd firmware for Cloud Hypervisor.
#
# CLOUDHV.fd is the OVMF/EDK2 build targeting Cloud Hypervisor's platform
# (OvmfPkg/CloudHv/CloudHvX64.dsc). Unlike rust-hypervisor-firmware, it provides
# a full modern UEFI (shim, GRUB, Secure Boot infrastructure), so an Ubuntu
# cloud image boots its *own* bootloader — no kernel extraction required. Pass
# the result as `BootSpec::Firmware { firmware }` (design document, section 24).
#
# CH's `--firmware` requires this CloudHv build specifically; the QEMU OVMF
# packages (/usr/share/OVMF/*, Kata's OVMF.fd) are NOT compatible and fail with
# `KernelLoad(Bzimage(InvalidBzImage))`.
#
# Requires sudo (to apt-install build deps). Build takes a couple of minutes on
# a multi-core host.
#
# Usage: scripts/build-cloudhv-firmware.sh [--base-dir DIR] [--edk2-tag TAG]
set -euo pipefail

BASE_DIR="/var/lib/vquasar"
EDK2_TAG="edk2-stable202502"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --base-dir) BASE_DIR="$2"; shift 2 ;;
    --edk2-tag) EDK2_TAG="$2"; shift 2 ;;
    -h|--help)  grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

echo "==> Installing build dependencies"
sudo DEBIAN_FRONTEND=noninteractive apt-get install -y \
  build-essential git nasm acpica-tools uuid-dev python3

BUILD_DIR="$BASE_DIR/build"
mkdir -p "$BUILD_DIR" "$BASE_DIR/firmware"
cd "$BUILD_DIR"

if [[ ! -d edk2 ]]; then
  echo "==> Cloning EDK2 ($EDK2_TAG)"
  git clone --depth 1 --recurse-submodules --shallow-submodules -b "$EDK2_TAG" \
    https://github.com/tianocore/edk2.git
fi

cd edk2
export PYTHON_COMMAND=python3
echo "==> Building BaseTools"
make -C BaseTools -j"$(nproc)"
# shellcheck disable=SC1091
source ./edksetup.sh

echo "==> Building CLOUDHV.fd (RELEASE, GCC5)"
build -a X64 -t GCC5 -b RELEASE -p OvmfPkg/CloudHv/CloudHvX64.dsc -n "$(nproc)"

FD="Build/CloudHvX64/RELEASE_GCC5/FV/CLOUDHV.fd"
[[ -f "$FD" ]] || { echo "build did not produce $FD" >&2; exit 1; }
cp "$FD" "$BASE_DIR/firmware/CLOUDHV.fd"

echo
echo "Done: $BASE_DIR/firmware/CLOUDHV.fd"
echo "Boot a cloud image with it (no kernel extraction needed):"
echo "  cargo run -p ch-client --example boot_vm -- \\"
echo "    --binary        $BASE_DIR/bin/cloud-hypervisor \\"
echo "    --firmware      $BASE_DIR/firmware/CLOUDHV.fd \\"
echo "    --disk          $BASE_DIR/volumes/<vm>.raw \\"
echo "    --readonly-disk $BASE_DIR/seed/seed.iso \\"
echo "    --runtime-dir   $BASE_DIR/vms/<vm>"
