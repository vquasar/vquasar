#!/usr/bin/env bash
# Uninstall a ch-orchestrator systemd component.
#
# Stops and disables the service, removes its unit and binary. Config and state
# are kept by default; --purge also removes the config env file (and the served
# UI bundle for control). It never deletes /var/lib/ch-orchestrator (VM disks,
# volumes, shared storage) — remove that by hand if you really mean to.
#
# Usage:
#   scripts/uninstall.sh agent   [--purge]
#   scripts/uninstall.sh control [--purge]
#   scripts/uninstall.sh all     [--purge]
#
#   -h, --help   Show this help
set -euo pipefail

BIN_DIR=/usr/local/bin
CONF_DIR=/etc/ch-orchestrator
UNIT_DIR=/etc/systemd/system
UI_DEST=/usr/local/share/ch-orchestrator/ui

ROLE="${1:-}"; shift || true
case "$ROLE" in agent|control|all) ;; *) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 2 ;; esac

PURGE=0
while [[ $# -gt 0 ]]; do
  case "$1" in
    --purge) PURGE=1; shift ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ $EUID -eq 0 ]] || { echo "error: must run as root" >&2; exit 1; }

remove_one() {
  local role="$1" svc="ch-$1"
  echo "==> removing $svc"
  if [[ "$role" == "agent" ]]; then
    echo "    note: running VMs survive (KillMode=process); delete them via the"
    echo "          control plane first if you want them gone."
  fi
  systemctl disable --now "$svc" >/dev/null 2>&1 || true
  rm -f "$UNIT_DIR/$svc.service"
  rm -f "$BIN_DIR/$svc"
  if [[ $PURGE -eq 1 ]]; then
    rm -f "$CONF_DIR/$role.env"
    [[ "$role" == "control" ]] && rm -rf "$UI_DEST"
    echo "    purged config $CONF_DIR/$role.env"
  fi
}

if [[ "$ROLE" == "all" ]]; then
  remove_one agent
  remove_one control
else
  remove_one "$ROLE"
fi

systemctl daemon-reload
# Drop the config dir only if empty (i.e. everything purged).
rmdir "$CONF_DIR" 2>/dev/null || true
echo "==> done. State under /var/lib/ch-orchestrator was left intact."
