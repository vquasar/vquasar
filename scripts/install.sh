#!/usr/bin/env bash
# Install a vquasar component as a systemd service.
#
# Installs the binary to /usr/local/bin, writes a config env file under
# /etc/vquasar, creates a systemd unit, and enables + starts it.
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
#   --tls-control-cn CN  (agent) Common Name the control plane's client cert must
#                        carry (default: control). Chaining to the CA is not
#                        identity — see design §30.
#   --log-format FMT     Log output: text (default) or json (design M17)
#   --otlp-endpoint URL  OpenTelemetry collector for OTLP/gRPC span export (M17)
#   --tls-issuer-cert P  (control) Intermediate issuing-CA cert for enrollment (M16)
#   --tls-issuer-key P   (control) Intermediate issuing-CA key (0600); enables signing
#   --enrollment-url URL (control) HTTPS URL agents reach for enrollment
#   -h, --help           Show this help
#
# agent options:
#   --name NAME          Agent/host name           (default: `hostname -s`)
#   --advertise-host IP  Migration advertise addr  (default: primary IPv4; use an IP, not a hostname)
#   --ch-binary PATH     cloud-hypervisor path     (default: /var/lib/vquasar/bin/cloud-hypervisor)
#   --grpc-listen ADDR   gRPC listen               (default: 0.0.0.0:9500)
#   --seccomp MODE       CH seccomp                (default: log)
#   --phone-home-url URL Control base URL for cloud-init phone_home IP discovery
#   --bootstrap-token T  Auto-enroll: one-time token from `POST /hosts/enroll` (M16)
#   --bootstrap-url URL  Auto-enroll: control's sign endpoint (…/api/v1/enroll/sign)
#   --bootstrap-ca PATH  Auto-enroll: root CA to trust control during bootstrap
#                        (design M13e), e.g. https://172.16.56.8:8080
#
# control options:
#   --db-url URL         Postgres URL              (default: postgres://ch:ch@127.0.0.1:5432/vquasar)
#   --db-ssl-mode MODE   TLS to Postgres: disable|allow|prefer|require|verify-ca|
#                        verify-full. Unset ⇒ prefer, which silently accepts an
#                        unencrypted connection. Use verify-full in production.
#   --allowed-path DIR   (control) Root a caller-supplied disk/kernel/firmware
#                        path must sit under; repeatable (default: /var/lib/vquasar)
#   --db-ca PATH         CA that signed the Postgres server certificate
#   --db-cert PATH       Client certificate for Postgres cert authentication
#   --db-key PATH        Client key matching --db-cert (0600)
#   --listen ADDR        REST/UI listen            (default: 0.0.0.0:8080)
#   --ui-dir PATH        UI dist to serve          (default: install ./ui/dist to /usr/local/share/vquasar/ui)
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
CONF_DIR=/etc/vquasar
UNIT_DIR=/etc/systemd/system
UI_DEST=/usr/local/share/vquasar/ui
STATE_DIR=/var/lib/vquasar

ROLE="${1:-}"; shift || true
[[ "$ROLE" == "agent" || "$ROLE" == "control" ]] || { grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

BINARY=""; NO_START=0; FORCE_CONFIG=0
NAME="$(hostname -s 2>/dev/null || hostname)"
ADVERTISE_HOST=""; CH_BINARY="$STATE_DIR/bin/cloud-hypervisor"; GRPC_LISTEN="0.0.0.0:9500"; SECCOMP="log"; PHONE_HOME_URL=""
DB_URL="postgres://ch:ch@127.0.0.1:5432/vquasar"; LISTEN="0.0.0.0:8080"; UI_DIR=""
DB_SSL_MODE=""; DB_CA=""; DB_CERT=""; DB_KEY=""; ALLOWED_PATHS=()
TLS_CA=""; TLS_CERT=""; TLS_KEY=""; TLS_CONTROL_CN=""
LOG_FORMAT=""; OTLP_ENDPOINT=""
TLS_ISSUER_CERT=""; TLS_ISSUER_KEY=""; ENROLLMENT_URL=""
BOOTSTRAP_TOKEN=""; BOOTSTRAP_URL=""; BOOTSTRAP_CA=""
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
    --phone-home-url) PHONE_HOME_URL="$2"; shift 2 ;;
    --db-url) DB_URL="$2"; shift 2 ;;
    --allowed-path) ALLOWED_PATHS+=("$2"); shift 2 ;;
    --db-ssl-mode) DB_SSL_MODE="$2"; shift 2 ;;
    --db-ca) DB_CA="$2"; shift 2 ;;
    --db-cert) DB_CERT="$2"; shift 2 ;;
    --db-key) DB_KEY="$2"; shift 2 ;;
    --listen) LISTEN="$2"; shift 2 ;;
    --ui-dir) UI_DIR="$2"; shift 2 ;;
    --tls-ca) TLS_CA="$2"; shift 2 ;;
    --tls-control-cn) TLS_CONTROL_CN="$2"; shift 2 ;;
    --tls-cert) TLS_CERT="$2"; shift 2 ;;
    --tls-key) TLS_KEY="$2"; shift 2 ;;
    --log-format) LOG_FORMAT="$2"; shift 2 ;;
    --otlp-endpoint) OTLP_ENDPOINT="$2"; shift 2 ;;
    --tls-issuer-cert) TLS_ISSUER_CERT="$2"; shift 2 ;;
    --tls-issuer-key) TLS_ISSUER_KEY="$2"; shift 2 ;;
    --enrollment-url) ENROLLMENT_URL="$2"; shift 2 ;;
    --bootstrap-token) BOOTSTRAP_TOKEN="$2"; shift 2 ;;
    --bootstrap-url) BOOTSTRAP_URL="$2"; shift 2 ;;
    --bootstrap-ca) BOOTSTRAP_CA="$2"; shift 2 ;;
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
# escape hatch is an explicit --allow-no-auth. Only enforce this when we are
# actually (re)writing the config — a binary/UI-only upgrade keeps the existing
# env file (which already carries the auth settings), so it must not be blocked.
if [[ "$ROLE" == "control" && -z "$OIDC_ISSUER" && $ALLOW_NO_AUTH -eq 0 ]]; then
  if [[ ! -f "$CONF_DIR/$ROLE.env" || $FORCE_CONFIG -eq 1 ]]; then
    echo "error: authentication is required. Pass --oidc-issuer/--oidc-client-id/--oidc-audience" >&2
    echo "       and --bootstrap-admin, or --allow-no-auth for a dev/lab install." >&2
    exit 1
  fi
fi

SVC="vquasar-$ROLE"
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
  [[ -n "$TLS_CA" ]] && echo "VQUASAR_${prefix}_TLS__CA=$TLS_CA"
  [[ "$prefix" == "AGENT" && -n "$TLS_CONTROL_CN" ]] && echo "VQUASAR_AGENT_TLS__CONTROL_CN=$TLS_CONTROL_CN"
  [[ -n "$TLS_CERT" ]] && echo "VQUASAR_${prefix}_TLS__CERT=$TLS_CERT"
  [[ -n "$TLS_KEY" ]] && echo "VQUASAR_${prefix}_TLS__KEY=$TLS_KEY"
  if [[ "$prefix" == "CONTROL" ]]; then
    # Intermediate issuing CA + enrollment URL enable agent auto-enrollment (M16).
    [[ -n "$TLS_ISSUER_CERT" ]] && echo "VQUASAR_CONTROL_TLS__ISSUER_CERT=$TLS_ISSUER_CERT"
    [[ -n "$TLS_ISSUER_KEY" ]] && echo "VQUASAR_CONTROL_TLS__ISSUER_KEY=$TLS_ISSUER_KEY"
    [[ -n "$ENROLLMENT_URL" ]] && echo "VQUASAR_CONTROL_ENROLLMENT__CONTROL_URL=$ENROLLMENT_URL"
  fi
}

# Emit the OIDC/RBAC env vars (design M12b) for the control plane.
auth_env() {
  [[ -n "$OIDC_ISSUER" ]] && echo "VQUASAR_CONTROL_AUTH__ISSUER=$OIDC_ISSUER"
  [[ -n "$OIDC_CLIENT_ID" ]] && echo "VQUASAR_CONTROL_AUTH__CLIENT_ID=$OIDC_CLIENT_ID"
  [[ -n "$OIDC_AUDIENCE" ]] && echo "VQUASAR_CONTROL_AUTH__AUDIENCE=$OIDC_AUDIENCE"
  [[ -n "$OIDC_CA" ]] && echo "VQUASAR_CONTROL_AUTH__CA=$OIDC_CA"
  [[ -n "$BOOTSTRAP_ADMIN" ]] && echo "VQUASAR_CONTROL_AUTH__BOOTSTRAP_ADMIN=$BOOTSTRAP_ADMIN"
}

# Emit the database TLS env vars for the control plane. Absent ⇒ the driver's
# `prefer`, which falls back to plaintext without complaining.
storage_env() {
  if [[ ${#ALLOWED_PATHS[@]} -gt 0 ]]; then
    local joined="" p
    for p in "${ALLOWED_PATHS[@]}"; do joined+="\"$p\","; done
    echo "VQUASAR_CONTROL_STORAGE__ALLOWED_PATHS=[${joined%,}]"
  fi
}

db_tls_env() {
  [[ -n "$DB_SSL_MODE" ]] && echo "VQUASAR_CONTROL_DATABASE__SSL_MODE=$DB_SSL_MODE"
  [[ -n "$DB_CA" ]] && echo "VQUASAR_CONTROL_DATABASE__CA=$DB_CA"
  [[ -n "$DB_CERT" ]] && echo "VQUASAR_CONTROL_DATABASE__CERT=$DB_CERT"
  [[ -n "$DB_KEY" ]] && echo "VQUASAR_CONTROL_DATABASE__KEY=$DB_KEY"
}

# Emit the field-encryption env vars (design M12c) for the control plane.
enc_env() {
  [[ -n "$ENC_KEY" ]] && echo "VQUASAR_CONTROL_ENCRYPTION__KEY=$ENC_KEY"
  [[ -n "$ENC_KEY_ID" ]] && echo "VQUASAR_CONTROL_ENCRYPTION__KEY_ID=$ENC_KEY_ID"
  [[ -n "$ENC_OLD_KEYS" ]] && echo "VQUASAR_CONTROL_ENCRYPTION__OLD_KEYS=$ENC_OLD_KEYS"
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
  # Auto-enrollment (design M16): with a one-time token, self-provision the mTLS
  # cert — generate a keypair + CSR locally and have control sign it. No hand-
  # copied agent certs. The private key never leaves this host.
  if [[ -n "$BOOTSTRAP_TOKEN" ]]; then
    [[ -n "$BOOTSTRAP_URL" && -n "$BOOTSTRAP_CA" ]] || {
      echo "error: --bootstrap-token requires --bootstrap-url and --bootstrap-ca" >&2; exit 1; }
    TLSDIR="$CONF_DIR/tls"; install -d -m 0755 "$TLSDIR"
    bkey="$TLSDIR/agent.key"; bcsr="$TLSDIR/agent.csr"; bcert="$TLSDIR/agent.crt"; bca="$TLSDIR/ca.crt"
    install -m 0644 "$BOOTSTRAP_CA" "$bca"
    echo "==> enrolling (CN=$NAME): generating key + CSR"
    openssl genrsa -out "$bkey" 2048 2>/dev/null; chmod 600 "$bkey"
    openssl req -new -key "$bkey" -out "$bcsr" -subj "/CN=$NAME" 2>/dev/null
    echo "==> requesting certificate from $BOOTSTRAP_URL"
    code=$(curl -sS --cacert "$bca" -H "X-Enrollment-Token: $BOOTSTRAP_TOKEN" \
      -H "Content-Type: application/x-pem-file" --data-binary @"$bcsr" \
      -o "$bcert" -w '%{http_code}' "$BOOTSTRAP_URL" || echo 000)
    if [[ "$code" != "200" ]] || ! openssl x509 -in "$bcert" -noout 2>/dev/null; then
      echo "error: enrollment failed (HTTP $code): $(cat "$bcert" 2>/dev/null)" >&2; exit 1
    fi
    rm -f "$bcsr"
    echo "==> enrolled: wrote $bcert (signed by the control-plane issuing CA)"
    TLS_CA="$bca"; TLS_CERT="$bcert"; TLS_KEY="$bkey"
  fi
  if [[ -z "$ADVERTISE_HOST" ]]; then
    ADVERTISE_HOST="$(ip -4 route get 1.1.1.1 2>/dev/null | awk '{for(i=1;i<=NF;i++) if($i=="src"){print $(i+1); exit}}')"
    ADVERTISE_HOST="${ADVERTISE_HOST:-127.0.0.1}"
  fi
  write_env <<EOF
# ch-agent configuration (systemd EnvironmentFile). See design section 36.
VQUASAR_AGENT_AGENT__NAME=$NAME
VQUASAR_AGENT_GRPC__LISTEN=$GRPC_LISTEN
VQUASAR_AGENT_HYPERVISOR__BINARY=$CH_BINARY
VQUASAR_AGENT_HYPERVISOR__RUNTIME_DIR=$STATE_DIR
VQUASAR_AGENT_HYPERVISOR__SECCOMP=$SECCOMP
VQUASAR_AGENT_MIGRATION__TRANSPORT=tcp
# Advertise an IP, not a hostname: the static CH binary has no working resolver.
VQUASAR_AGENT_MIGRATION__ADVERTISE_HOST=$ADVERTISE_HOST
${PHONE_HOME_URL:+VQUASAR_AGENT_PHONE_HOME__URL=$PHONE_HOME_URL}
${LOG_FORMAT:+VQUASAR_AGENT_LOGGING__FORMAT=$LOG_FORMAT}
${OTLP_ENDPOINT:+VQUASAR_AGENT_LOGGING__OTLP_ENDPOINT=$OTLP_ENDPOINT}
$(tls_env AGENT)
EOF

  # The agent must NOT kill Cloud Hypervisor on stop/restart: VMs survive an
  # agent restart and the new instance re-attaches (design section 11). CH
  # processes are spawned into the service cgroup, so KillMode=process ensures
  # only the agent is signalled on stop.
  cat > "$UNIT_DIR/$SVC.service" <<EOF
[Unit]
Description=vquasar host agent (Cloud Hypervisor)
Documentation=https://github.com/vquasar/vquasar
Wants=network-online.target
After=network-online.target openvswitch.service remote-fs.target
After=openvswitch.service
# Host-reboot recovery (design M16): don't start until the shared storage the
# VMs' disks/kernels live on is mounted, so recovery can actually see them.
# Make the NFS mount reboot-persistent in /etc/fstab (_netdev,nofail).
RequiresMountsFor=$STATE_DIR/shared

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

  # Host-reboot recovery (design M16): the agent uses KillMode=process so VMs
  # survive an *agent* restart — but that orphans the cloud-hypervisor processes,
  # and on a full reboot systemd-shutdown hangs forever "Waiting for process:
  # cloud-hypervisor" (worse, it can wedge in NFS I/O as storage tears down).
  # This oneshot's ExecStop runs at shutdown *before* remote-fs is unmounted
  # (it is ordered After=remote-fs.target, so it stops before it), terminating
  # the VMs while their storage is still mounted. It never fires on an agent
  # restart (separate unit), so VM survival across agent restarts is preserved.
  cat > "$UNIT_DIR/vquasar-vm-shutdown.service" <<EOF
[Unit]
Description=Terminate Cloud Hypervisor VMs before host shutdown (vquasar)
After=vquasar-agent.service remote-fs.target network-online.target
[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/bin/true
ExecStop=-/usr/bin/pkill -TERM -f $CH_BINARY
ExecStop=-/bin/sleep 3
ExecStop=-/usr/bin/pkill -KILL -f $CH_BINARY
TimeoutStopSec=30
[Install]
WantedBy=multi-user.target
EOF
  echo "==> wrote unit $UNIT_DIR/vquasar-vm-shutdown.service"

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
VQUASAR_CONTROL_DATABASE__URL=$DB_URL
VQUASAR_CONTROL_SERVER__LISTEN=$LISTEN
VQUASAR_CONTROL_RECONCILE__INTERVAL_SECS=3
VQUASAR_CONTROL_STORAGE__SHARED_VOLUMES_DIR=$STATE_DIR/shared/volumes
${UI_DIR:+VQUASAR_CONTROL_SERVER__UI_DIR=$UI_DIR}
${LOG_FORMAT:+VQUASAR_CONTROL_LOGGING__FORMAT=$LOG_FORMAT}
${OTLP_ENDPOINT:+VQUASAR_CONTROL_LOGGING__OTLP_ENDPOINT=$OTLP_ENDPOINT}
$(storage_env)
$(db_tls_env)
$(tls_env CONTROL)
$(auth_env)
$(enc_env)
EOF

  cat > "$UNIT_DIR/$SVC.service" <<EOF
[Unit]
Description=vquasar control plane
Documentation=https://github.com/vquasar/vquasar
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
# The shutdown-hook unit must be enabled so it's active at runtime and its
# ExecStop fires on host shutdown (design M16).
[[ "$ROLE" == "agent" ]] && systemctl enable vquasar-vm-shutdown.service >/dev/null 2>&1 && systemctl start vquasar-vm-shutdown.service >/dev/null 2>&1 || true
if [[ $NO_START -eq 0 ]]; then
  systemctl restart "$SVC"
  sleep 1
  systemctl --no-pager --lines=0 status "$SVC" | sed -n '1,3p' || true
  echo "==> $SVC installed and started. Logs: journalctl -u $SVC -f"
else
  echo "==> $SVC installed and enabled (not started). Start: systemctl start $SVC"
fi
