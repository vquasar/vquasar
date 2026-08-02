#!/usr/bin/env bash
# Install a ch-orchestrator component as a systemd service.
#
# Installs the binary to /usr/local/bin, writes a config env file under
# /etc/ch-orchestrator, creates a systemd unit, and enables + starts it.
# Designed to be self-contained (units + config generated inline) so it can
# later back a `curl … | sh` bootstrap installer.
#
# Usage:
#   scripts/install.sh agent   [options]
#   scripts/install.sh control [options]
#
# Common options:
#   --binary PATH        Component binary (default: auto-detect target/{release,debug} or ./ch-<role>)
#   --no-start           Install and enable, but do not start now
#   --force-config       Overwrite an existing env file (default: keep existing)
#   -h, --help           Show this help
#
# agent options:
#   --name NAME          Agent/host name           (default: `hostname -s`)
#   --advertise-host IP  Migration advertise addr  (default: primary IPv4; use an IP, not a hostname)
#   --ch-binary PATH     cloud-hypervisor path     (default: /var/lib/ch-orchestrator/bin/cloud-hypervisor)
#   --grpc-listen ADDR   gRPC listen               (default: 0.0.0.0:9500)
#   --seccomp MODE       CH seccomp                (default: log)
#
# control options:
#   --db-url URL         Postgres URL              (default: postgres://ch:ch@127.0.0.1:5432/ch_orchestrator)
#   --listen ADDR        REST/UI listen            (default: 0.0.0.0:8080)
#   --ui-dir PATH        UI dist to serve          (default: install ./ui/dist to /usr/local/share/ch-orchestrator/ui)
set -euo pipefail

BIN_DIR=/usr/local/bin
CONF_DIR=/etc/ch-orchestrator
UNIT_DIR=/etc/systemd/system
UI_DEST=/usr/local/share/ch-orchestrator/ui
STATE_DIR=/var/lib/ch-orchestrator

ROLE="${1:-}"; shift || true
[[ "$ROLE" == "agent" || "$ROLE" == "control" ]] || { grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

BINARY=""; NO_START=0; FORCE_CONFIG=0
NAME="$(hostname -s 2>/dev/null || hostname)"
ADVERTISE_HOST=""; CH_BINARY="$STATE_DIR/bin/cloud-hypervisor"; GRPC_LISTEN="0.0.0.0:9500"; SECCOMP="log"
DB_URL="postgres://ch:ch@127.0.0.1:5432/ch_orchestrator"; LISTEN="0.0.0.0:8080"; UI_DIR=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) BINARY="$2"; shift 2 ;;
    --no-start) NO_START=1; shift ;;
    --force-config) FORCE_CONFIG=1; shift ;;
    --name) NAME="$2"; shift 2 ;;
    --advertise-host) ADVERTISE_HOST="$2"; shift 2 ;;
    --ch-binary) CH_BINARY="$2"; shift 2 ;;
    --grpc-listen) GRPC_LISTEN="$2"; shift 2 ;;
    --seccomp) SECCOMP="$2"; shift 2 ;;
    --db-url) DB_URL="$2"; shift 2 ;;
    --listen) LISTEN="$2"; shift 2 ;;
    --ui-dir) UI_DIR="$2"; shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ $EUID -eq 0 ]] || { echo "error: must run as root" >&2; exit 1; }

SVC="ch-$ROLE"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# Locate the binary if not given: prefer release, then debug, then repo/cwd.
if [[ -z "$BINARY" ]]; then
  for c in "$REPO_ROOT/target/release/$SVC" "$REPO_ROOT/target/debug/$SVC" "./$SVC" "$SVC"; do
    if [[ -x "$c" ]] || command -v "$c" >/dev/null 2>&1; then BINARY="$c"; break; fi
  done
fi
if [[ -z "$BINARY" ]] || { [[ ! -x "$BINARY" ]] && ! command -v "$BINARY" >/dev/null 2>&1; }; then
  echo "error: could not find $SVC binary; pass --binary PATH" >&2; exit 1
fi

echo "==> installing $SVC from $BINARY"
install -d "$CONF_DIR" "$STATE_DIR"
install -m 0755 "$BINARY" "$BIN_DIR/$SVC"

ENV_FILE="$CONF_DIR/$ROLE.env"
write_env() {
  if [[ -f "$ENV_FILE" && $FORCE_CONFIG -eq 0 ]]; then
    echo "==> keeping existing config $ENV_FILE (use --force-config to overwrite)"
  else
    cat > "$ENV_FILE"
    chmod 0644 "$ENV_FILE"
    echo "==> wrote config $ENV_FILE"
  fi
}

if [[ "$ROLE" == "agent" ]]; then
  if [[ -z "$ADVERTISE_HOST" ]]; then
    ADVERTISE_HOST="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src"){print $(i+1); exit}}')"
    ADVERTISE_HOST="${ADVERTISE_HOST:-127.0.0.1}"
  fi
  write_env <<EOF
# ch-agent configuration (systemd EnvironmentFile). See design section 36.
CH_AGENT_AGENT__NAME=$NAME
CH_AGENT_GRPC__LISTEN=$GRPC_LISTEN
CH_AGENT_HYPERVISOR__BINARY=$CH_BINARY
CH_AGENT_HYPERVISOR__RUNTIME_DIR=$STATE_DIR
CH_AGENT_HYPERVISOR__SECCOMP=$SECCOMP
CH_AGENT_MIGRATION__TRANSPORT=tcp
# Advertise an IP, not a hostname: the static CH binary has no working resolver.
CH_AGENT_MIGRATION__ADVERTISE_HOST=$ADVERTISE_HOST
EOF

  # The agent must NOT kill Cloud Hypervisor on stop/restart: VMs survive an
  # agent restart and the new instance re-attaches (design section 11). CH
  # processes are spawned into the service cgroup, so KillMode=process ensures
  # only the agent is signalled on stop.
  cat > "$UNIT_DIR/$SVC.service" <<EOF
[Unit]
Description=ch-orchestrator host agent (Cloud Hypervisor)
Documentation=https://github.com/wrkode/ch-orchestrator
Wants=network-online.target
After=network-online.target openvswitch.service
After=openvswitch.service

[Service]
Type=exec
EnvironmentFile=$ENV_FILE
ExecStart=$BIN_DIR/$SVC
Restart=on-failure
RestartSec=3
# Do not tear down running VMs when the agent stops/restarts (section 11).
KillMode=process

[Install]
WantedBy=multi-user.target
EOF

else # control
  if [[ -z "$UI_DIR" ]]; then
    if [[ -d "$REPO_ROOT/ui/dist" ]]; then
      echo "==> installing UI bundle to $UI_DEST"
      install -d "$UI_DEST"
      cp -a "$REPO_ROOT/ui/dist/." "$UI_DEST/"
      UI_DIR="$UI_DEST"
    fi
  fi
  write_env <<EOF
# ch-control configuration (systemd EnvironmentFile). See design section 36.
CH_CONTROL_DATABASE__URL=$DB_URL
CH_CONTROL_SERVER__LISTEN=$LISTEN
CH_CONTROL_RECONCILE__INTERVAL_SECS=3
CH_CONTROL_STORAGE__SHARED_VOLUMES_DIR=$STATE_DIR/shared/volumes
${UI_DIR:+CH_CONTROL_SERVER__UI_DIR=$UI_DIR}
EOF

  cat > "$UNIT_DIR/$SVC.service" <<EOF
[Unit]
Description=ch-orchestrator control plane
Documentation=https://github.com/wrkode/ch-orchestrator
Wants=network-online.target
After=network-online.target postgresql.service docker.service

[Service]
Type=exec
EnvironmentFile=$ENV_FILE
ExecStart=$BIN_DIR/$SVC
Restart=on-failure
RestartSec=3

[Install]
WantedBy=multi-user.target
EOF
fi

echo "==> wrote unit $UNIT_DIR/$SVC.service"
systemctl daemon-reload
systemctl enable "$SVC" >/dev/null 2>&1 || true
if [[ $NO_START -eq 0 ]]; then
  systemctl restart "$SVC"
  sleep 1
  systemctl --no-pager --lines=0 status "$SVC" | sed -n '1,3p' || true
  echo "==> $SVC installed and started. Logs: journalctl -u $SVC -f"
else
  echo "==> $SVC installed and enabled (not started). Start: systemctl start $SVC"
fi
