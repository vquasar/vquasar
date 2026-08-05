# Authentication and RBAC with Keycloak

vquasar does not store passwords. Identity comes from an external OIDC provider;
the control plane is an OAuth2 resource server that validates the bearer access
token on every request. **Authorization is vquasar's own** — roles and
permissions over vquasar resources live in the control plane, and the token only
supplies identity and group membership.

Keycloak is the reference provider. Any OIDC provider works if it can issue an
access token carrying the right audience and a groups claim.

## Set up the provider

[`scripts/keycloak-setup.sh`](../scripts/keycloak-setup.sh) creates everything
needed, idempotently:

```bash
KEYCLOAK_ADMIN_PASSWORD=... scripts/keycloak-setup.sh \
  --url https://keycloak.lab:8443 \
  --ui-origin https://control.lab:8080
```

It creates a realm (`vquasar`), a public client for the web UI (Authorization
Code + PKCE, no client secret in the browser), the groups
`vquasar-admins` / `vquasar-operators` / `vquasar-viewers`, and two protocol
mappers. Add `--demo-users` on a test install to also get alice (admins), bob
(viewers) and carol (no group); `--insecure` skips certificate verification when
Keycloak has a self-signed development certificate.

**Both mappers are load-bearing**, and each is a silent failure if missing:

* **audience** — vquasar checks the token's `aud` against `[auth] audience`.
  Keycloak does not put the client id in `aud` by default, so without this
  mapper every token is rejected as an invalid audience.
* **groups** — vquasar maps groups to roles. Without this mapper the token
  carries no groups, every group mapping is inert, and users get nothing but
  whatever roles are assigned to them directly.

## Configure the control plane

```toml
[auth]
issuer          = "https://keycloak.lab:8443/realms/vquasar"
client_id       = "vquasar"
audience        = "vquasar"
groups_claim    = "groups"
bootstrap_admin = "alice"
ca              = "/etc/vquasar/certs/ca.crt"   # only for a private-CA IdP
```

Or at install time:

```bash
scripts/install.sh control \
  --oidc-issuer https://keycloak.lab:8443/realms/vquasar \
  --oidc-client-id vquasar --oidc-audience vquasar \
  --oidc-ca /etc/vquasar/certs/ca.crt \
  --bootstrap-admin alice
```

`bootstrap_admin` solves the chicken-and-egg: the first identity to log in with
that username, email, or subject is granted the `admin` role, so someone can
create the rest. It stays in effect, so set it to a real, controlled identity.

The control plane discovers the provider's JWKS at startup and caches the keys,
refreshing on an unknown key id so provider key rotation needs no restart. If
discovery fails, the control plane **refuses to start** rather than falling back
to an open API.

Auth is off when `issuer` is empty, and then every caller is a dev superuser.
That is the escape hatch for a fresh lab; the startup log says so explicitly:

```
INFO authentication DISABLED (dev mode) — set [auth] issuer to enforce
```

## Map groups to roles

Built-in roles are `admin` (everything), `operator` (workloads and their
resources, but not identity or host management), and `viewer` (read-only plus
VM console). Custom roles can grant any subset of the permission catalog
(`GET /api/v1/permissions`).

As the bootstrap admin:

```bash
TOKEN=...   # an access token for the bootstrap admin
API=https://control.lab:8080/api/v1

VIEWER=$(curl -s -H "Authorization: Bearer $TOKEN" $API/roles \
  | python3 -c 'import sys,json;print([r["id"] for r in json.load(sys.stdin) if r["name"]=="viewer"][0])')

curl -X POST $API/group-mappings -H "Authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d "{\"group\":\"vquasar-viewers\",\"role_id\":\"$VIEWER\"}"
```

Or use the Access page in the web UI. A caller's effective permissions are the
union of the roles assigned to them directly and the roles mapped from their
token's groups.

## Verifying an install

The checks below were run against Keycloak 26 and all pass. Get tokens without a
browser using the direct access grant (which the setup script leaves enabled):

```bash
tok() { curl -sk -X POST \
  https://keycloak.lab:8443/realms/vquasar/protocol/openid-connect/token \
  -d client_id=vquasar -d grant_type=password -d "username=$1" -d "password=$2" \
  | python3 -c 'import sys,json;print(json.load(sys.stdin)["access_token"])'; }
```

| Check                                                | Expected |
| ---------------------------------------------------- | -------- |
| `GET /vms` with no token                             | 401      |
| `GET /vms` with a malformed token                    | 401      |
| `GET /vms` with a token from a different realm       | 401      |
| `GET /vms` as the bootstrap admin                    | 200      |
| `GET /vms` as a user in no group                     | 403      |
| `GET /vms` as a user whose group is not yet mapped   | 403      |
| `GET /vms` after mapping that group to `viewer`      | 200      |
| `POST /vms` as that viewer                           | 403      |
| `GET /roles` as that viewer                          | 403      |
| `POST /hosts` as that viewer                         | 403      |
| `GET /vms/{id}/console` (WS) with no `access_token`  | 401      |
| `GET /vms/{id}/console` (WS) with an invalid token   | 401      |

Decode a token to confirm the mappers are doing their job — `aud` must contain
the client id and `groups` must list the user's groups:

```bash
python3 -c '
import base64,json,sys
p = sys.argv[1].split(".")[1]
c = json.loads(base64.urlsafe_b64decode(p + "=" * (-len(p) % 4)))
print("aud:", c.get("aud"), "groups:", c.get("groups"), "iss:", c.get("iss"))' "$(tok alice alice)"
```

Turn the direct access grant off on the client once you are done testing — the
browser flow does not need it.

## Notes and limitations

* **The browser flow** (Authorization Code + PKCE from the SPA) is implemented
  and the UI reads `GET /api/v1/auth-config` for the issuer and client id, but
  the checks above exercise the API, not the browser redirect. If the IdP uses a
  private CA, the browser must trust it too — open the Keycloak URL once and
  accept the certificate, or install the CA in the browser's trust store.
* **The console WebSocket** takes its token as a query parameter, because
  browsers cannot set headers on a WebSocket handshake. Access tokens therefore
  appear in the control plane's request logs at that endpoint; keep console
  token lifetimes short.
* **Group names are matched literally** after stripping Keycloak's leading `/`.
  Nested groups arrive as the leaf name unless the mapper is configured for full
  paths.
* vquasar reads **groups only** — Keycloak realm or client roles are ignored.
