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

**In the console**, the project sits in the top bar and applies to everything on
screen — one selection, sent on every request, so you never see one project's
VMs beside another's networks. It survives a reload, and is dropped
automatically if the project disappears or your binding is revoked. "All
projects" appears only if you hold a platform-wide binding.

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

## Quotas

A project can be capped on five dimensions. An unset one is unlimited, and a
project with no quota at all is unlimited everywhere — which is what every
project gets on upgrade.

```bash
curl -X PUT $API/projects/$P/quota -H "Authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"max_vms": 20, "max_vcpus": 80, "max_memory_mib": 262144,
       "max_storage_bytes": 5497558138880}'
```

This is a whole-object write: a field you leave out becomes unlimited. That is
deliberate — there is no way to leave a stale limit in place by forgetting to
mention it.

`GET /projects/:id/quota` returns the limits, current usage and an `over_quota`
flag together, because a limit without the usage beside it does not answer the
question you actually have.

```json
{"limits": {"max_vms": 20, "max_vcpus": 80},
 "usage":  {"vms": 7, "vcpus": 22, "memory_mib": 45056,
            "volumes": 3, "storage_bytes": 274877906944},
 "over_quota": false}
```

`DELETE` returns the project to unlimited.

**What counts.** A resource consumes quota from the moment its row exists until
the row is gone — including while it is `Pending`, `Failed` or `Deleting`. The
quota asks what the project has committed to, not what the fleet has managed to
build.

* `vcpus` and `memory_mib` count the hot-plug **ceiling** (`max_vcpus`,
  `memory_max_mib`), because that is what was committed to, whatever the VM
  boots with.
* `storage_bytes` counts volumes **and** disks a VM spec asks the agent to
  provision. Counting only volumes would leave the cap bypassable by asking for
  the space as a VM disk.

**When it is checked.** Only at API admission, in the same transaction that
persists the intent. The reconcile loop never refuses work for quota reasons: a
request accepted and then stranded is worse than one refused up front. Every
write that changes a counted quantity passes admission, including an in-place
`PATCH /vms/:id`, checked on the *difference* it makes — which is why shrinking
a VM is always allowed.

A refusal is `409` with code `QUOTA_EXCEEDED` and a message carrying the
arithmetic:

```
project quota exceeded: memory_mib — limit 16384, 15360 in use,
this request asks for 2048
```

**Lowering a limit below current usage is allowed** and destroys nothing. It
blocks new commitments and reports `over_quota: true`. Refusing the change would
leave you no way to stop a runaway project short of deleting its work, and
shrinking stays admissible so a project is never trapped above its cap.

**Volumes are reserved before they are built.** Creating one commits the row
inside the admission transaction, then `qemu-img` runs, then the row is
finalised — so it is `provisioning` for a while and cannot be attached or
snapshotted until it is `ready`. The old order did the conversion first, which
under a quota means doing the expensive work and only then refusing it.

## Limits

* **Projects are created and quotas set through the API only.** The console
  switches between projects but does not create, rename or cap them; a platform
  admin does that with `POST /projects` and `PUT /projects/:id/quota`.
* **Projects are flat.** `parent_id` exists in the schema and is unenforced —
  recursive permission inheritance and quota rollup are load-bearing decisions
  not worth guessing at in advance.
* **Deleting a project is refused while it owns anything.** Cascading would mean
  deleting VMs, which is long, agent-touching and restartable — not something
  that belongs behind a `DELETE`.
