# Projects and multi-tenancy

A **project** is the unit of tenancy. VMs, volumes, templates, security groups
and tasks belong to exactly one. Images and networks are *shareable* — one with
no owner is platform-shared and usable from everywhere. Hosts, users, roles and
the CA are platform resources and are never project-scoped: a host inventory is
a leak to a tenant with no tenant benefit.

The feature is **off by default**. With it off, every caller acts platform-wide
and nothing about a running cluster changes.

```toml
[tenancy]
enabled = true
```

## What a request acts in

Every request resolves to one project:

1. the `X-Vquasar-Project` header,
2. or `?project=` (a WebSocket handshake cannot set headers — the same
   constraint that put the access token in the query string),
3. or, absent both, the **default** project.

Never "everything" by omission. Absence of a selection must not widen what a
caller can see.

`X-Vquasar-Project: *` selects the platform view — every project at once. It is
not a privilege by itself; see below.

The project is a header rather than a path prefix because prefixing would double
every route and break every existing client.

## Permissions are held *in* a project

A role binding — a user's direct grant, or a mapping from an OIDC group — names
the project it applies in. **No project means platform-wide**, which is what
every binding meant before projects existed, so nothing you have already set up
changes.

A caller's effective permissions for a request are:

> their platform-wide bindings **∪** their bindings in the project the request
> names

A caller with no binding in that project has no permissions there, and every
endpoint refuses them. This is why the header can be set to anything and buys
nothing: it selects which bindings count, it does not assert anything.

That also means `*` is safe to offer. Permissions resolve against the platform
view the same way, so a caller holding only project bindings can do nothing
there.

### Making bindings

A binding is created in the scope the request is acting in — the same scope the
caller's own `iam:manage` was resolved in.

```bash
API=https://control.lab:8080/api/v1

# Platform-wide: applies in every project. Needs platform-wide iam:manage.
curl -X POST $API/group-mappings -H "Authorization: Bearer $TOKEN" \
  -H 'X-Vquasar-Project: *' -H 'content-type: application/json' \
  -d "{\"group\":\"vquasar-admins\",\"role_id\":\"$ADMIN\"}"

# Inside one project: applies only there.
curl -X POST $API/group-mappings -H "Authorization: Bearer $TOKEN" \
  -H 'X-Vquasar-Project: team-blue' -H 'content-type: application/json' \
  -d "{\"group\":\"blue-devs\",\"role_id\":\"$OPERATOR\"}"
```

This is what prevents privilege escalation: a project administrator cannot mint
a platform-wide grant, because that means acting in platform scope, where their
own permissions resolve to nothing.

**Watch the quiet case.** With tenancy on and no project named, an IAM write
lands in the *default* project, not platform-wide. Where authorization is
concerned the silent default is the narrower one. If you mean platform-wide, say
`*`.

## What ownership means for writes

Reading a shared resource and writing one are not symmetric:

| | shared (no owner) | owned by another project |
| --- | --- | --- |
| read | yes | no — `404` |
| write / delete | no — `404` | no — `404` |

Sharing an image must not hand every project the power to delete it out from
under the others.

Cross-project answers are always `404`, never `403`. A caller learns nothing
about what exists elsewhere, and an unknown id is indistinguishable from someone
else's.

## What a new resource belongs to

* VMs, volumes, templates, security groups — the caller's project.
* Images and networks — the caller's project when tenancy is on; unowned
  (shared) when it is off, since there is no project context to record.
* Tasks — the project of the VM they act on.
* Provider and VLAN networks are always platform-shared: they attach to physical
  infrastructure, which is not a tenant's to own.

## Checking it

`scripts/verify-oidc.sh` runs the whole authorization matrix, including the
per-project checks, against a real provider. CI runs the same script with
tenancy on, so what is verified automatically and what you can run against your
own install stay identical.

By hand, `GET /api/v1/me` reports the permissions you hold *and* the project
they are about:

```json
{"authenticated": true, "username": "bob",
 "permissions": ["vm:read", "..."],
 "project": "0f1e...": }
```

If a call is refused where you expect it to work, check that field first — most
surprises are a request acting in a different project than you meant.

## Limits

* **No quotas yet.** A project is an isolation boundary, not a consumption
  boundary: one project can still exhaust the fleet. See ADR-019.
* **The web UI has no project selector.** It sends no header, so it acts in the
  default project.
* **Projects are flat.** `parent_id` exists in the schema and is unenforced —
  recursive permission inheritance and quota rollup are load-bearing decisions
  not worth guessing at in advance.
* **Deleting a project is refused while it owns anything.** Cascading would mean
  deleting VMs, which is long, agent-touching and restartable — not something
  that belongs behind a `DELETE`.
