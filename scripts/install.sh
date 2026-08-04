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
#   --tls-ca PATH        CA cert for mutual TLS (design M12a)
#   --tls-cert PATH      This component's certificate (agent: server; control: server + gRPC client)
#   --tls-key PATH       This component's private key
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
#   --oidc-issuer URL    OIDC issuer (enables auth; design M12b)
#   --oidc-client-id ID  OIDC client id (for the UI login)
#   --oidc-audience AUD  Expected token audience
#   --oidc-ca PATH       Extra CA to trust for the IdP (internal-CA Keycloak)
#   --bootstrap-admin ID Email/subject granted admin on first login
#   --allow-no-auth      Dev/lab only: install control without authentication
#   --enc-key B64        AES-256 key, base64 (openssl rand -base64 32); enables
#                        field encryption at rest of sensitive cloud-init (M12c)
#   --enc-key-id ID      Key id stamped into sealed values (default: default)
#   --enc-old-keys LIST  Decrypt-only keys during rotation: id:b64,id2:b64
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
TLS_CA=""; TLS_CERT=""; TLS_KEY=""
OIDC_ISSUER=""; OIDC_CLIENT_ID=""; OIDC_AUDIENCE=""; OIDC_CA=""; BOOTSTRAP_ADMIN=""; ALLOW_NO_AUTH=0
ENC_KEY=""; ENC_KEY_ID=""; ENC_OLD_KEYS=""

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
    --tls-ca) TLS_CA="$2"; shift 2 ;;
    --tls-cert) TLS_CERT="$2"; shift 2 ;;
    --tls-key) TLS_KEY="$2"; shift 2 ;;
    --oidc-issuer) OIDC_ISSUER="$2"; shift 2 ;;
    --oidc-client-id) OIDC_CLIENT_ID="$2"; shift 2 ;;
    --oidc-audience) OIDC_AUDIENCE="$2"; shift 2 ;;
    --oidc-ca) OIDC_CA="$2"; shift 2 ;;
    --bootstrap-admin) BOOTSTRAP_ADMIN="$2"; shift 2 ;;
    --allow-no-auth) ALLOW_NO_AUTH=1; shift ;;
    --enc-key) ENC_KEY="$2"; shift 2 ;;
    --enc-key-id) ENC_KEY_ID="$2"; shift 2 ;;
    --enc-old-keys) ENC_OLD_KEYS="$2"; shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[[ $EUID -eq 0 ]] || { echo "error: must run as root" >&2; exit 1; }

# Authentication is mandatory for the control plane (design M12b). The dev/lab
# escape hatch is an explicit --allow-no-auth.
if [[ "$ROLE" == "control" && -z "$OIDC_ISSUER" && $ALLOW_NO_AUTH -eq 0 ]]; then
  echo "error: authentication is required. Pass --oidc-issuer/--oidc-client-id/--oidc-audience" >&2
  echo "       and --bootstrap-admin, or --allow-no-auth for a dev/lab install." >&2
  exit 1
fi

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

# Emit the mTLS env vars (design M12a) when cert paths were provided.
tls_env() {
  local prefix="$1"
  [[ -n "$TLS_CA" ]] && echo "CH_${prefix}_TLS__CA=$TLS_CA"
  [[ -n "$TLS_CERT" ]] && echo "CH_${prefix}_TLS__CERT=$TLS_CERT"
  [[ -n "$TLS_KEY" ]] && echo "CH_${prefix}_TLS__KEY=$TLS_KEY"
}

# Emit the OIDC/RBAC env vars (design M12b) for the control plane.
auth_env() {
  [[ -n "$OIDC_ISSUER" ]] && echo "CH_CONTROL_AUTH__ISSUER=$OIDC_ISSUER"
  [[ -n "$OIDC_CLIENT_ID" ]] && echo "CH_CONTROL_AUTH__CLIENT_ID=$OIDC_CLIENT_ID"
  [[ -n "$OIDC_AUDIENCE" ]] && echo "CH_CONTROL_AUTH__AUDIENCE=$OIDC_AUDIENCE"
  [[ -n "$OIDC_CA" ]] && echo "CH_CONTROL_AUTH__CA=$OIDC_CA"
  [[ -n "$BOOTSTRAP_ADMIN" ]] && echo "CH_CONTROL_AUTH__BOOTSTRAP_ADMIN=$BOOTSTRAP_ADMIN"
}

# Emit the field-encryption env vars (design M12c) for the control plane.
enc_env() {
  [[ -n "$ENC_KEY" ]] && echo "CH_CONTROL_ENCRYPTION__KEY=$ENC_KEY"
  [[ -n "$ENC_KEY_ID" ]] && echo "CH_CONTROL_ENCRYPTION__KEY_ID=$ENC_KEY_ID"
  [[ -n "$ENC_OLD_KEYS" ]] && echo "CH_CONTROL_ENCRYPTION__OLD_KEYS=$ENC_OLD_KEYS"
}

write_env() {
  if [[ -f "$ENV_FILE" && $FORCE_CONFIG -eq 0 ]]; then
    echo "==> keeping existing config $ENV_FILE (use --force-config to overwrite)"
  else
    cat > "$ENV_FILE"
    # 0600: the env file holds secrets (DB URL, and the M12c encryption key).
    chmod 0600 "$ENV_FILE"
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
$(tls_env AGENT)
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
$(tls_env CONTROL)
$(auth_env)
$(enc_env)
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
