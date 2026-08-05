#!/usr/bin/env bash
# Generate an internal CA and mTLS certificates for vquasar (design
# M12a). One CA signs the control-plane certificate and one certificate per
# host agent. The control certificate carries serverAuth + clientAuth — it is
# the REST/TLS server and the gRPC client to every agent. Agent certificates
# carry serverAuth only: they serve their own gRPC endpoint and must not be
# usable as a credential into another agent (design §30).
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
# Distribute: ca.crt to everyone; control.{crt,key} + int.{crt,key} to the
# control host (int.* is the intermediate issuing CA for auto-enrollment — keep
# int.key offline-safe/0600); agent-<fqdn>.{crt,key} + ca.crt to that agent
# (only needed for the legacy manual path; auto-enrolled agents get their cert
# signed by the intermediate at join time).
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

# --- Root CA (reused if it already exists) ---
if [[ ! -f ca.crt ]]; then
  openssl genrsa -out ca.key 4096 2>/dev/null
  openssl req -x509 -new -key ca.key -sha256 -days 3650 -out ca.crt \
    -subj "/CN=vquasar-ca" 2>/dev/null
  echo "==> created root CA (ca.crt)"
fi

# --- Intermediate issuing CA (design M16) ---
# Signs agent certificates at auto-enrollment; only the intermediate key goes on
# the control host, so the root key can stay offline. Agents present the chain
# leaf->intermediate->root; everyone trusts the root (ca.crt) as anchor.
if [[ ! -f int.crt ]]; then
  openssl genrsa -out int.key 4096 2>/dev/null
  openssl req -new -key int.key -out int.csr -subj "/CN=vquasar-intermediate" 2>/dev/null
  cat > int.ext <<EOF
basicConstraints = critical, CA:TRUE, pathlen:0
keyUsage = critical, keyCertSign, cRLSign
EOF
  openssl x509 -req -in int.csr -CA ca.crt -CAkey ca.key -CAcreateserial \
    -sha256 -days 1825 -out int.crt -extfile int.ext 2>/dev/null
  rm -f int.csr int.ext
  chmod 600 int.key
  echo "==> created intermediate CA (int.crt) — put int.crt + int.key on control"
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

# gen_cert NAME SANS [EKU]
#
# EKU defaults to serverAuth only. Only the control plane needs clientAuth: it
# dials the agents. An agent certificate with clientAuth would also be a valid
# credential *into* every other agent, so one compromised host could drive the
# whole fleet — design §30 says a host compromise must not become a
# control-plane (or fleet) compromise.
gen_cert() {
  local name="$1" sans="$2" eku="${3:-serverAuth}"
  openssl genrsa -out "$name.key" 2048 2>/dev/null
  openssl req -new -key "$name.key" -out "$name.csr" -subj "/CN=$name" 2>/dev/null
  cat > "$name.ext" <<EOF
subjectAltName = $sans
extendedKeyUsage = $eku
keyUsage = digitalSignature, keyEncipherment
EOF
  openssl x509 -req -in "$name.csr" -CA ca.crt -CAkey ca.key -CAcreateserial \
    -sha256 -days 825 -out "$name.crt" -extfile "$name.ext" 2>/dev/null
  rm -f "$name.csr" "$name.ext"
  chmod 600 "$name.key"
  echo "==> issued $name.crt ($sans)"
}

# Control plane: reachable names + always localhost for local API calls.
# The control plane is both a TLS server (REST/UI) and a client (agent gRPC).
gen_cert control "$(san_from "localhost,127.0.0.1,$CONTROL_HOSTS")" "serverAuth, clientAuth"

# One cert per agent, named agent-<fqdn>. Accept "fqdn" or "fqdn:ip".
for a in "${AGENTS[@]}"; do
  fqdn="${a%%:*}"
  if [[ "$a" == *:* ]]; then names="$fqdn,${a#*:}"; else names="$fqdn"; fi
  gen_cert "agent-$fqdn" "$(san_from "$names")"
done

chmod 600 ca.key
echo "==> done. Certificates in $(pwd)"
