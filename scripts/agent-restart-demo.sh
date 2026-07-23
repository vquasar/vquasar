#!/usr/bin/env bash
# Milestone 2 acceptance: drive ch-agent over gRPC to run a real VM, then prove
# the VM survives an agent restart (design document, section 11, milestone 2).
#
# Requires a prepared lab (scripts/prepare-ubuntu-image.sh) under BASE_DIR and a
# built workspace. Run on a host with /dev/kvm.
set -euo pipefail

BASE_DIR="${BASE_DIR:-/var/lib/ch-orchestrator}"
KVER="${KVER:-7.0.0-28-generic}"
LISTEN="127.0.0.1:9500"
ENDPOINT="http://${LISTEN}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

echo "==> Building agent + client"
cargo build -q -p ch-agent
cargo build -q -p ch-agent --example agent_client
AGENT="$ROOT/target/debug/ch-agent"
CLIENT="$ROOT/target/debug/examples/agent_client"

export CH_AGENT_AGENT__NAME="dome"
export CH_AGENT_GRPC__LISTEN="$LISTEN"
export CH_AGENT_HYPERVISOR__BINARY="$BASE_DIR/bin/cloud-hypervisor"
export CH_AGENT_HYPERVISOR__RUNTIME_DIR="$BASE_DIR"

start_agent() {
  "$AGENT" >>"$BASE_DIR/agent.log" 2>&1 &
  echo $!
}
wait_port() {
  for _ in $(seq 1 50); do
    (exec 3<>"/dev/tcp/127.0.0.1/9500") 2>/dev/null && { exec 3>&-; return 0; }
    sleep 0.2
  done
  echo "agent gRPC did not come up" >&2; return 1
}
ch_pid() { pgrep -f "api-socket .*/vms/$1/api.sock" | head -1; }

VMID="$(cat /proc/sys/kernel/random/uuid)"
DISK="$BASE_DIR/volumes/$VMID.raw"
cp --reflink=auto "$BASE_DIR/images/ubuntu-26.04.raw" "$DISK"
qemu-img resize -f raw "$DISK" +8G >/dev/null

echo "==> Starting agent"
AGENT_PID="$(start_agent)"; wait_port
"$CLIENT" --endpoint "$ENDPOINT" host-info | grep -E "cloud_hypervisor_version|host_id"

echo "==> EnsureVm ($VMID)"
"$CLIENT" --endpoint "$ENDPOINT" ensure \
  --vm-id "$VMID" --name demo \
  --kernel "$BASE_DIR/images/vmlinuz-$KVER" \
  --initramfs "$BASE_DIR/images/initrd.img-$KVER" \
  --cmdline "root=/dev/vda1 rw console=ttyS0 systemd.mask=systemd-networkd-wait-online.service" \
  --disk "$DISK" --readonly-disk "$BASE_DIR/seed/seed.iso"

sleep 2
VM_PID_BEFORE="$(ch_pid "$VMID")"
echo "    cloud-hypervisor pid = ${VM_PID_BEFORE:-NONE}"
[ -n "$VM_PID_BEFORE" ] || { echo "FAIL: VM not running"; exit 1; }

echo "==> Stopping the agent (SIGINT) — the VM must keep running"
kill -INT "$AGENT_PID"; wait "$AGENT_PID" 2>/dev/null || true
sleep 1
if kill -0 "$VM_PID_BEFORE" 2>/dev/null; then
  echo "    OK: cloud-hypervisor $VM_PID_BEFORE still alive after agent stop"
else
  echo "FAIL: VM died when the agent stopped"; exit 1
fi

echo "==> Restarting the agent — it must recover the VM"
AGENT_PID="$(start_agent)"; wait_port
"$CLIENT" --endpoint "$ENDPOINT" get "$VMID" | grep -E "phase|vm_id"
VM_PID_AFTER="$(ch_pid "$VMID")"
if [ "$VM_PID_BEFORE" = "$VM_PID_AFTER" ]; then
  echo "    OK: recovered the same VM process (pid $VM_PID_AFTER)"
else
  echo "FAIL: pid changed ($VM_PID_BEFORE -> ${VM_PID_AFTER:-NONE})"; exit 1
fi

echo "==> DeleteVm — tears down the process and state"
"$CLIENT" --endpoint "$ENDPOINT" delete "$VMID"
sleep 1
if kill -0 "$VM_PID_AFTER" 2>/dev/null; then
  echo "FAIL: VM still running after delete"; exit 1
else
  echo "    OK: VM process gone after delete"
fi

kill -INT "$AGENT_PID" 2>/dev/null || true
wait "$AGENT_PID" 2>/dev/null || true
echo "==> Milestone 2 acceptance PASSED"
