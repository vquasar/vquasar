#!/usr/bin/env bash
# Generate an internal CA and mTLS certificates for ch-orchestrator (design
# M12a). One CA signs the control-plane certificate and one certificate per
# host agent. Certs carry both serverAuth and clientAuth EKUs so the control
# cert can act as the gRPC client to agents (mutual TLS) and as the REST/TLS
# server, while each agent cert serves its gRPC endpoint.
#
# Usage:
#   scripts/gen-certs.sh --out DIR --control-host HOST[,HOST...] \
#       --agent FQDN[:IP] [--agent FQDN[:IP] ...]
#
#   --out DIR           Output directory (created; holds ca.crt/key + certs)
#   --control-host LIST Comma-separated DNS names / IPs the control plane is
#                       reached at (for the API server cert SAN)
#   --agent FQDN[:IP]   A host agent's FQDN and optional IP (repeatable)
#   -h, --help          Show this help
#
# Distribute: ca.crt to everyone; control.{crt,key} to the control host;
# agent-<fqdn>.{crt,key} + ca.crt to that agent.
set -euo pipefail

OUT=""; CONTROL_HOSTS=""; AGENTS=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --out) OUT="$2"; shift 2 ;;
    --control-host) CONTROL_HOSTS="$2"; shift 2 ;;
    --agent) AGENTS+=("$2"); shift 2 ;;
    -h|--help) grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ -n "$OUT" && -n "$CONTROL_HOSTS" ]] || { grep '^#' "$0" | sed 's/^# \{0,1\}//'; exit 2; }

mkdir -p "$OUT"; cd "$OUT"

# --- CA (reused if it already exists) ---
if [[ ! -f ca.crt ]]; then
  openssl genrsa -out ca.key 4096 2>/dev/null
  openssl req -x509 -new -key ca.key -sha256 -days 3650 -out ca.crt \
    -subj "/CN=ch-orchestrator-ca" 2>/dev/null
  echo "==> created CA (ca.crt)"
fi

# san_from "a,b,1.2.3.4" -> "DNS:a,DNS:b,IP:1.2.3.4"
san_from() {
  local out="" item
  IFS=',' read -ra parts <<< "$1"
  for item in "${parts[@]}"; do
    [[ -z "$item" ]] && continue
    if [[ "$item" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
      out+="IP:$item,"
    else
      out+="DNS:$item,"
    fi
  done
  echo "${out%,}"
}

gen_cert() {
  local name="$1" sans="$2"
  openssl genrsa -out "$name.key" 2048 2>/dev/null
  openssl req -new -key "$name.key" -out "$name.csr" -subj "/CN=$name" 2>/dev/null
  cat > "$name.ext" <<EOF
subjectAltName = $sans
extendedKeyUsage = serverAuth, clientAuth
keyUsage = digitalSignature, keyEncipherment
EOF
  openssl x509 -req -in "$name.csr" -CA ca.crt -CAkey ca.key -CAcreateserial \
    -sha256 -days 825 -out "$name.crt" -extfile "$name.ext" 2>/dev/null
  rm -f "$name.csr" "$name.ext"
  chmod 600 "$name.key"
  echo "==> issued $name.crt ($sans)"
}

# Control plane: reachable names + always localhost for local API calls.
gen_cert control "$(san_from "localhost,127.0.0.1,$CONTROL_HOSTS")"

# One cert per agent, named agent-<fqdn>. Accept "fqdn" or "fqdn:ip".
for a in "${AGENTS[@]}"; do
  fqdn="${a%%:*}"
  if [[ "$a" == *:* ]]; then names="$fqdn,${a#*:}"; else names="$fqdn"; fi
  gen_cert "agent-$fqdn" "$(san_from "$names")"
done

chmod 600 ca.key
echo "==> done. Certificates in $(pwd)"
