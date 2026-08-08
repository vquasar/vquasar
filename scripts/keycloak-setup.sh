#!/usr/bin/env bash
# Configure a Keycloak realm for vquasar (design M12b).
#
# Creates (idempotently) a realm, a public OIDC client for the web UI, the two
# protocol mappers vquasar's token validation depends on, and the groups that
# map to its RBAC roles. Prints the [auth] block to paste into control.toml.
#
# Keycloak is the reference IdP; vquasar itself speaks standard OIDC, so any
# compliant provider works if it can produce the same two claims.
#
#   KEYCLOAK_ADMIN_PASSWORD=... scripts/keycloak-setup.sh \
#       --url https://keycloak.lab:8443 --ui-origin https://control.lab:8080
#
set -euo pipefail

KC_URL=""
REALM="vquasar"
CLIENT="vquasar"
ADMIN_USER="admin"
ADMIN_REALM="master"
UI_ORIGIN=""
DEMO_USERS=0
INSECURE=0

usage() {
    sed -n '2,12p' "$0" | sed 's/^# \?//'
    cat <<'EOF'

Options:
  --url URL           Keycloak base URL (required), e.g. https://keycloak.lab:8443
  --realm NAME        Realm to create/update            (default: vquasar)
  --client ID         OIDC client id for the web UI     (default: vquasar)
  --ui-origin URL     Where the UI is served from; sets the redirect URI and
                      web origin. Repeatable. Defaults to http://localhost:5173
                      Repeat it once per address the console can be reached at.
                      With more than one control plane that means the VIP *and*
                      each node: the console is served by every instance, and a
                      login at an unregistered address fails with redirect_uri.
                      (the Vite dev server) when not given.
  --admin-user NAME   Keycloak admin username           (default: admin)
  --admin-realm NAME  Realm the admin lives in          (default: master)
  --demo-users        Also create alice/bob/carol in the three groups, for
                      testing an install. Never use on a real deployment.
  --insecure          Skip TLS verification when talking to Keycloak (self-
                      signed dev certificate).
  -h, --help          Show this help.

The admin password is read from $KEYCLOAK_ADMIN_PASSWORD so it never appears
in the process list or your shell history.
EOF
}

UI_ORIGINS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --url)          KC_URL="$2"; shift 2 ;;
        --realm)        REALM="$2"; shift 2 ;;
        --client)       CLIENT="$2"; shift 2 ;;
        --ui-origin)    UI_ORIGINS+=("$2"); shift 2 ;;
        --admin-user)   ADMIN_USER="$2"; shift 2 ;;
        --admin-realm)  ADMIN_REALM="$2"; shift 2 ;;
        --demo-users)   DEMO_USERS=1; shift ;;
        --insecure)     INSECURE=1; shift ;;
        -h|--help)      usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
done

[[ -n "$KC_URL" ]] || { echo "error: --url is required" >&2; exit 2; }
[[ -n "${KEYCLOAK_ADMIN_PASSWORD:-}" ]] || {
    echo "error: set KEYCLOAK_ADMIN_PASSWORD" >&2; exit 2; }
command -v python3 >/dev/null || { echo "error: python3 is required" >&2; exit 2; }

KC_URL="${KC_URL%/}"
[[ ${#UI_ORIGINS[@]} -gt 0 ]] || UI_ORIGINS=("${UI_ORIGIN:-http://localhost:5173}")

CURL=(curl -sS --fail-with-body)
[[ $INSECURE -eq 1 ]] && CURL+=(-k)

# ---- admin token ------------------------------------------------------------

TOKEN=$("${CURL[@]}" -X POST \
    "$KC_URL/realms/$ADMIN_REALM/protocol/openid-connect/token" \
    -d client_id=admin-cli -d grant_type=password \
    -d "username=$ADMIN_USER" --data-urlencode "password=$KEYCLOAK_ADMIN_PASSWORD" \
    | python3 -c 'import sys,json; print(json.load(sys.stdin)["access_token"])')

api() { # api METHOD PATH [body-on-stdin]
    local method="$1" path="$2"
    "${CURL[@]}" -X "$method" "$KC_URL/admin$path" \
        -H "Authorization: Bearer $TOKEN" \
        -H "Content-Type: application/json" "${@:3}"
}

# Does an admin path exist? (200 => yes)
exists() {
    local code
    code=$(curl -sS -o /dev/null -w '%{http_code}' $([[ $INSECURE -eq 1 ]] && echo -k) \
        -H "Authorization: Bearer $TOKEN" "$KC_URL/admin$1")
    [[ "$code" == "200" ]]
}

# ---- realm ------------------------------------------------------------------

if exists "/realms/$REALM"; then
    echo "realm '$REALM' already exists — leaving it alone"
else
    api POST /realms -d "$(python3 -c "
import json; print(json.dumps({'realm': '$REALM', 'enabled': True,
  'displayName': 'vquasar'}))")" >/dev/null
    echo "created realm '$REALM'"
fi

# ---- client -----------------------------------------------------------------
#
# Public client, Authorization Code + PKCE (S256) — the browser SPA holds no
# secret. Direct access grants stay on so tokens can be fetched from a script
# for testing; turn that off for a production realm.
#
# The two mappers are load-bearing:
#   * audience  — vquasar validates `aud` against [auth] audience. Keycloak
#                 does not put the client id in `aud` by default, so without
#                 this mapper every token is rejected as InvalidAudience.
#   * groups    — vquasar maps token groups to RBAC roles via [auth]
#                 groups_claim (default "groups"). No mapper, no group roles.

redirect_uris=$(python3 -c "
import json,sys
print(json.dumps([u.rstrip('/') + '/*' for u in sys.argv[1:]]))" "${UI_ORIGINS[@]}")
web_origins=$(python3 -c "
import json,sys
print(json.dumps([u.rstrip('/') for u in sys.argv[1:]]))" "${UI_ORIGINS[@]}")

client_json=$(python3 - "$CLIENT" "$redirect_uris" "$web_origins" <<'PY'
import json, sys
client_id, redirects, origins = sys.argv[1], json.loads(sys.argv[2]), json.loads(sys.argv[3])
print(json.dumps({
    "clientId": client_id,
    "name": "vquasar",
    "enabled": True,
    "publicClient": True,
    "standardFlowEnabled": True,
    "directAccessGrantsEnabled": True,
    "serviceAccountsEnabled": False,
    "redirectUris": redirects,
    "webOrigins": origins,
    "attributes": {"pkce.code.challenge.method": "S256"},
    "protocolMappers": [
        {
            "name": "vquasar-audience",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-audience-mapper",
            "config": {
                "included.client.audience": client_id,
                "access.token.claim": "true",
                "id.token.claim": "false",
            },
        },
        {
            "name": "groups",
            "protocol": "openid-connect",
            "protocolMapper": "oidc-group-membership-mapper",
            "config": {
                "claim.name": "groups",
                "full.path": "false",
                "access.token.claim": "true",
                "id.token.claim": "true",
                "userinfo.token.claim": "true",
            },
        },
    ],
}))
PY
)

existing_id=$(api GET "/realms/$REALM/clients?clientId=$CLIENT" \
    | python3 -c 'import sys,json; c=json.load(sys.stdin); print(c[0]["id"] if c else "")')
if [[ -n "$existing_id" ]]; then
    api PUT "/realms/$REALM/clients/$existing_id" -d "$client_json" >/dev/null
    echo "updated client '$CLIENT'"
else
    api POST "/realms/$REALM/clients" -d "$client_json" >/dev/null
    echo "created client '$CLIENT'"
fi

# ---- groups -----------------------------------------------------------------
#
# Names match vquasar's built-in roles; `vquasar-role-map.sh` (or the UI's
# Access page) binds each group to its role in the control plane. vquasar does
# not read Keycloak roles — only groups.

for g in vquasar-admins vquasar-operators vquasar-viewers; do
    if api GET "/realms/$REALM/groups?search=$g" \
        | python3 -c "import sys,json; sys.exit(0 if any(x['name']=='$g' for x in json.load(sys.stdin)) else 1)"; then
        echo "group '$g' already exists"
    else
        api POST "/realms/$REALM/groups" -d "{\"name\":\"$g\"}" >/dev/null
        echo "created group '$g'"
    fi
done

# ---- demo users -------------------------------------------------------------

if [[ $DEMO_USERS -eq 1 ]]; then
    add_user() { # add_user NAME GROUP-OR-EMPTY PASSWORD
        local name="$1" group="$2" pass="$3" body
        body=$(python3 - "$name" "$group" "$pass" <<'PY'
import json, sys
name, group, pw = sys.argv[1], sys.argv[2], sys.argv[3]
u = {"username": name, "enabled": True, "emailVerified": True,
     "email": f"{name}@example.test", "firstName": name.capitalize(),
     "lastName": "Demo",
     "credentials": [{"type": "password", "value": pw, "temporary": False}]}
if group:
    u["groups"] = ["/" + group]
print(json.dumps(u))
PY
)
        if api GET "/realms/$REALM/users?username=$name&exact=true" \
            | python3 -c 'import sys,json; sys.exit(0 if json.load(sys.stdin) else 1)'; then
            echo "user '$name' already exists"
        else
            api POST "/realms/$REALM/users" -d "$body" >/dev/null
            echo "created user '$name'${group:+ in $group}"
        fi
    }
    add_user alice vquasar-admins    alice
    add_user bob   vquasar-viewers   bob
    add_user carol ""                carol   # deliberately in no group
    echo
    echo "Demo users created with passwords equal to their usernames."
    echo "They are for testing an install — delete them before going live."
fi

# ---- what to configure ------------------------------------------------------

cat <<EOF

Done. Configure the control plane with:

  [auth]
  issuer          = "$KC_URL/realms/$REALM"
  client_id       = "$CLIENT"
  audience        = "$CLIENT"
  groups_claim    = "groups"
  bootstrap_admin = "<username or email of your first admin>"
  # ca = "/etc/vquasar/certs/ca.crt"   # if Keycloak uses a private CA

then map each Keycloak group to a vquasar role (as the bootstrap admin):

  POST /api/v1/iam/group-roles  {"group":"vquasar-admins",    "role_id":"<admin>"}
  POST /api/v1/iam/group-roles  {"group":"vquasar-operators", "role_id":"<operator>"}
  POST /api/v1/iam/group-roles  {"group":"vquasar-viewers",   "role_id":"<viewer>"}

Role ids come from GET /api/v1/iam/roles.
EOF
