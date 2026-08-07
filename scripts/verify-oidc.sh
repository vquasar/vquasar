#!/usr/bin/env bash
# Verify OIDC authentication and RBAC against a running control plane
# (design M12b). Exits non-zero if any check fails.
#
# The same script runs in CI against a throwaway Keycloak and against a real
# install — a verification that only exists in CI tends not to match what
# operators actually run.
#
#   API=https://control.lab:8080/api/v1 \
#   KC=https://keycloak.lab:8443 REALM=vquasar CLIENT=vquasar \
#   scripts/verify-oidc.sh
#
# Requires the demo users from `keycloak-setup.sh --demo-users` (alice in
# vquasar-admins, bob in vquasar-viewers, carol in no group) and the client's
# direct access grant, which that script leaves enabled.
set -uo pipefail

API="${API:-http://127.0.0.1:8080/api/v1}"
KC="${KC:-http://127.0.0.1:8081}"
REALM="${REALM:-vquasar}"
CLIENT="${CLIENT:-vquasar}"
CURL_OPTS="${CURL_OPTS:--sk}"

FAILED=0
pass() { printf '  PASS  %-58s %s\n' "$1" "$2"; }
fail() { printf '  FAIL  %-58s got %s want %s\n' "$1" "$2" "$3"; FAILED=1; }

tok() { # tok USER PASS
  curl $CURL_OPTS -X POST "$KC/realms/$REALM/protocol/openid-connect/token" \
    -d "client_id=$CLIENT" -d grant_type=password \
    -d "username=$1" -d "password=$2" \
  | python3 -c 'import sys,json;print(json.load(sys.stdin).get("access_token",""))'
}

check() { # check LABEL WANT METHOD PATH [TOKEN] [BODY]
  local label="$1" want="$2" method="$3" path="$4" token="${5:-}" body="${6:-}"
  local args=($CURL_OPTS -o /dev/null -w '%{http_code}' -X "$method" "$API$path")
  [[ -n "$token" ]] && args+=(-H "Authorization: Bearer $token")
  [[ -n "$body" ]] && args+=(-H 'content-type: application/json' -d "$body")
  local got; got=$(curl "${args[@]}")
  [[ "$got" == "$want" ]] && pass "$label" "$got" || fail "$label" "$got" "$want"
}

ALICE=$(tok alice alice); BOB=$(tok bob bob); CAROL=$(tok carol carol)
for v in ALICE BOB CAROL; do
  [[ -n "${!v}" ]] || { echo "could not obtain a token for $v — is the client's direct access grant enabled?"; exit 1; }
done
echo "tokens acquired for alice/bob/carol"

# Start from a known state: drop any mapping left by an earlier run.
VIEWER=$(curl $CURL_OPTS -H "Authorization: Bearer $ALICE" "$API/roles" \
  | python3 -c 'import sys,json;print([r["id"] for r in json.load(sys.stdin) if r["name"]=="viewer"][0])')
curl $CURL_OPTS -o /dev/null -X DELETE -H "Authorization: Bearer $ALICE" \
  "$API/group-mappings/vquasar-viewers/$VIEWER"

echo
echo "== authentication =="
check "no token -> 401"                          401 GET /vms
check "malformed token -> 401"                   401 GET /vms "not.a.jwt"
check "bootstrap admin -> 200"                   200 GET /vms "$ALICE"

echo
echo "== the token's claims reach the control plane =="
python3 - "$ALICE" <<'PY'
import base64, json, sys
p = sys.argv[1].split('.')[1]
c = json.loads(base64.urlsafe_b64decode(p + '=' * (-len(p) % 4)))
print(f"  aud={c.get('aud')} groups={c.get('groups')} iss={c.get('iss')}")
assert c.get('groups'), "no groups claim — the group-membership mapper is missing"
PY
[[ $? -eq 0 ]] || FAILED=1

echo
echo "== RBAC before any group mapping =="
check "no group, no role -> 403"                 403 GET /vms "$CAROL"
check "group present but unmapped -> 403"        403 GET /vms "$BOB"
check "admin can read roles -> 200"              200 GET /roles "$ALICE"

echo
echo "== map a group to a role =="
check "admin creates the mapping -> 201"         201 POST /group-mappings "$ALICE" \
  "{\"group\":\"vquasar-viewers\",\"role_id\":\"$VIEWER\"}"

echo
echo "== RBAC after mapping =="
check "viewer can read VMs -> 200"               200 GET  /vms   "$BOB"
check "viewer can read hosts -> 200"             200 GET  /hosts "$BOB"
check "viewer cannot create a VM -> 403"         403 POST /vms   "$BOB" '{"name":"nope","spec":{}}'
check "viewer cannot read roles -> 403"          403 GET  /roles "$BOB"
check "viewer cannot register a host -> 403"     403 POST /hosts "$BOB" '{"name":"h","endpoint":"http://x"}'
check "ungrouped user still has nothing -> 403"  403 GET  /vms   "$CAROL"

echo
echo "== per-project RBAC (design §47, ADR-020) =="
# The header names the project; it is not believed. A caller's permissions are
# resolved *in* that project, so naming one they hold no binding in leaves them
# with nothing. That is what makes tenancy an isolation boundary rather than a
# label. `*` is the platform view — also not a privilege, since permissions are
# resolved against it the same way.
check_in() { # check_in LABEL WANT PROJECT METHOD PATH [TOKEN] [BODY]
  local label="$1" want="$2" project="$3" method="$4" path="$5" token="${6:-}" body="${7:-}"
  local args=($CURL_OPTS -o /dev/null -w '%{http_code}' -X "$method" "$API$path"
              -H "X-Vquasar-Project: $project")
  [[ -n "$token" ]] && args+=(-H "Authorization: Bearer $token")
  [[ -n "$body" ]] && args+=(-H 'content-type: application/json' -d "$body")
  local got; got=$(curl "${args[@]}")
  [[ "$got" == "$want" ]] && pass "$label" "$got" || fail "$label" "$got" "$want"
}
bind_in() { # bind_in PROJECT   (map vquasar-viewers -> viewer in that scope)
  curl $CURL_OPTS -o /dev/null -X POST -H "Authorization: Bearer $ALICE" \
    -H 'content-type: application/json' -H "X-Vquasar-Project: $1" \
    "$API/group-mappings" -d "{\"group\":\"vquasar-viewers\",\"role_id\":\"$VIEWER\"}"
}
unbind_in() {
  curl $CURL_OPTS -o /dev/null -X DELETE -H "Authorization: Bearer $ALICE" \
    -H "X-Vquasar-Project: $1" "$API/group-mappings/vquasar-viewers/$VIEWER"
}

# alice is the bootstrap admin; that grant is platform-wide, so it applies in
# every project and in the platform view.
curl $CURL_OPTS -o /dev/null -X POST -H "Authorization: Bearer $ALICE" \
  -H 'content-type: application/json' -H 'X-Vquasar-Project: *' \
  "$API/projects" -d '{"name":"tenant-a"}'

check_in "a platform-wide grant applies inside a project" 200 tenant-a GET /vms "$ALICE"
check_in "...and in the platform view"                    200 '*'      GET /vms "$ALICE"

# A binding is made in the scope the request acts in. The mapping created
# earlier carried no header, so it landed in the default project.
check_in "bob's default-project binding works there"      200 default  GET /vms "$BOB"
check_in "...and not in another project"                  403 tenant-a GET /vms "$BOB"

# Re-make it platform-wide, and it applies everywhere.
unbind_in default; bind_in '*'
check_in "a platform-wide binding applies in every project" 200 tenant-a GET /vms "$BOB"

# Re-make it inside tenant-a only, and it applies nowhere else.
unbind_in '*'; bind_in tenant-a
check_in "a project binding works in its project"        200 tenant-a GET /vms "$BOB"
check_in "...and nowhere else"                           403 default  GET /vms "$BOB"
check_in "...and not in the platform view"               403 '*'      GET /vms "$BOB"
check    "no header falls back to the default project"   403 GET /vms "$BOB"

# The escalation this design has to refuse: a project-scoped caller minting a
# platform-wide grant. Reaching platform scope needs `*`, where a caller holding
# only project bindings resolves to no permissions at all.
check_in "a project-scoped caller cannot write platform bindings" 403 '*' POST \
  /group-mappings "$BOB" "{\"group\":\"vquasar-viewers\",\"role_id\":\"$VIEWER\"}"

# Which projects exist is itself tenancy information.
projects=$(curl $CURL_OPTS -H "Authorization: Bearer $BOB" \
  -H 'X-Vquasar-Project: tenant-a' "$API/projects" \
  | python3 -c 'import sys,json;d=json.load(sys.stdin);print(len(d) if isinstance(d,list) else -1)')
[[ "$projects" == "1" ]] \
  && pass "a tenant sees only the projects they are bound to" "$projects" \
  || fail "a tenant sees only the projects they are bound to" "$projects" 1

echo
echo "== console WebSocket =="
VM=00000000-0000-0000-0000-000000000000
ws() { curl $CURL_OPTS -o /dev/null -w '%{http_code}' \
  -H "Connection: Upgrade" -H "Upgrade: websocket" -H "Sec-WebSocket-Version: 13" \
  -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" "$API/vms/$VM/console$1"; }
got=$(ws ""); [[ "$got" == "401" ]] && pass "console without a token -> 401" "$got" || fail "console without a token -> 401" "$got" 401
got=$(ws "?access_token=bogus"); [[ "$got" == "401" ]] && pass "console with an invalid token -> 401" "$got" || fail "console with an invalid token -> 401" "$got" 401

echo
if [[ $FAILED -eq 0 ]]; then echo "ALL CHECKS PASSED"; else echo "SOME CHECKS FAILED"; fi
exit $FAILED
