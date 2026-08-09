# Storage pools

A **storage pool** is a named place to put bytes. Every volume belongs to
exactly one, and a VM's disks record the pool they were placed in.

The part worth understanding before anything else: **a pool does not say which
hosts can reach it — the hosts do.** You configure a name, a kind and a path;
each agent then reports, every few seconds, whether it can actually use that
pool and how much room it has. Nothing you type makes a pool usable, and
nothing keeps it usable once the hosts stop saying so.

That is not pedantry. Before pools, storage was one directory in
`control.toml` that every host was *assumed* to have mounted at the same path.
Nothing recorded the assumption and nothing checked it, so a host without the
mount accepted a VM and then failed to launch it with a path error — and live
migration depended on the same assumption silently.

## Pending and ready

A pool is `pending` until some host reports it, and `ready` while at least one
does. A brand-new pool is `pending`; so is a correctly-configured one whose NFS
server is down.

Open a pool in the console (**Storage pools** → click a row) to see every
host's report. A host that cannot use the pool says why:

```
hv-3    not usable    /srv/fast does not exist here — the pool is probably not
                      mounted on this host
```

The agent proves this rather than assuming it. For a `shared_dir` pool it
checks that the directory exists, that it is a directory, and that it is
genuinely writable — by writing a probe file, because permission bits say
nothing about a read-only remount and under NFS `root_squash` they say the
opposite of the truth. Capacity comes from `statvfs`.

**The agent never creates the directory.** A host missing the mount would
otherwise get an empty directory on its own root filesystem and then report the
pool as usable — writing what it believed was shared storage somewhere no other
host can see. Create the mount point yourself, on the hosts that should have it.

## What a pool means for placement

The scheduler refuses a host that does not report **every** pool a VM's disks
are in. A VM waiting for that says so on its task:

```
no schedulable host reports the storage pool this VM's disks are in
```

which is deliberately not the message you get when the fleet is merely full.
The same check guards an explicit migration target and a drain — a live
migration onto a host that cannot see the guest's disks is refused up front
rather than discovered when the guest fails to start on the far side.

A disk pointed at a raw path you supplied yourself carries no pool and
constrains placement not at all. The platform does not know which pool such a
file is in, and guessing from the path would be a claim it cannot back. Those
paths are governed by `[storage] allowed_paths` instead.

## Creating one

Console: **Storage pools** → **Add pool**. Or:

```bash
curl -X POST https://vquasar/api/v1/storage-pools \
  -H 'content-type: application/json' \
  -d '{"name":"fast","kind":"shared_dir","path":"/srv/fast"}'
```

`shared_dir` is the only kind today. The path must sit under one of
`[storage] allowed_paths`: the agent opens files there with privilege, so a
pool is a caller-supplied host path like any other.

Two things are fixed once a pool exists. Its **kind and path cannot be
changed** — a pool's identity is where its bytes are, and repointing it would
strand every volume in it while the row still looked correct. And **one
directory is one pool**: a second pool over the same path is refused, because
it would double-count the same disk's capacity and split its volumes across two
namespaces.

Deleting a pool that still holds volumes is refused, with a count. Move or
delete them first.

## Placing a volume

Volumes go in `default` unless you say otherwise:

```bash
curl -X POST https://vquasar/api/v1/volumes \
  -d '{"name":"data","size_bytes":10737418240,"pool":"fast"}'
```

`pool` takes an id or a name. In the console the volume dialog grows a pool
selector once more than one pool exists.

A VM's own system disk goes in `default`. Choosing a pool per VM needs per-VM
storage policy, which does not exist yet.

## Upgrading an existing cluster

Nothing moves. The first time a control plane starts after the upgrade it
creates a `default` pool from the `[storage] shared_volumes_dir` you already
had, and adopts every existing volume into it. Paths are unchanged — a volume
is still `<pool>/vol-<uuid>.<ext>`, and the default pool's root *is* the old
directory.

`shared_volumes_dir` is now only the seed for that one row. It is not read
again: if you rename or repoint the `default` pool through the API, that wins,
and config will not overrule it on the next restart.

## Reclaiming files nobody owns

Deleting a VM leaves nothing behind today, but clusters that ran older versions
have cloud-init seeds and system disks on shared storage whose VM is long gone.
A periodic sweep finds them.

By default it **reports and deletes nothing**, recording a `storage.orphans`
event:

```
14 orphaned file(s) holding 213 MiB; set [storage] orphan_reclaim = "delete"
to reclaim them
```

To act on it:

```toml
[storage]
orphan_reclaim = "delete"   # "report" (default) | "delete" | "off"
orphan_sweep_secs = 3600
orphan_min_age_secs = 3600
```

Only files vquasar itself writes are ever candidates — `vol-<uuid>`, `<uuid>`,
`<uuid>-disk<n>`, `<uuid>-d<n>`, and seeds under `<pool>/seeds/`. Your own
files in a pool are never touched, whatever they contain, and a file must have
gone untouched for `orphan_min_age_secs` before it is considered at all.

The sweep runs in the control plane rather than on the agents, and has to: a
file on shared storage may belong to a VM on any host, so an agent deleting
what it does not recognise would be deleting another host's work.

## Permissions

| Permission | Held by | What it allows |
| --- | --- | --- |
| `storagepool:read` | `viewer`, `operator`, `admin` | See pools, their state, and per-host reports |
| `storagepool:manage` | `admin` | Create, rename, delete |

Pools are platform resources, like hosts: any project may place a volume in any
pool. Restricting pools per project belongs with quotas and does not exist yet.

## Planned kinds

`lvm_thin`, `nfs` and `rbd` are the shapes the model was built to accept. Each
is one more variant; nothing that reads a pool changes when they arrive.

See [ADR-023](../DESIGN.md) for the reasoning, and §20 for where pools sit in
the architecture.
