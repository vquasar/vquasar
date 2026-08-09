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

## Kinds

### `shared_dir`

A directory you have already mounted on the hosts, by whatever means your fleet
already uses. vquasar does not mount it and **never creates it** — a host
missing the mount would otherwise get an empty directory on its own root
filesystem and then report the pool as usable.

### `nfs`

The pool names the export, and the agents mount it:

```bash
curl -X POST https://vquasar/api/v1/storage-pools \
  -H 'content-type: application/json' \
  -d '{"name":"shelf","kind":"nfs","server":"10.0.0.5","export":"/exports/vms",
       "mount_point":"/var/lib/vquasar/nfs/shelf","options":"vers=4.2,hard"}'
```

The difference from a `shared_dir` that happens to be NFS is *who is
responsible for the mount*. With `shared_dir`, every host needs the same mount
arranged out of band and nothing records that you did it. With `nfs`, the pool
is the record, and a host that lacks the mount gets it on the next reconcile
tick.

`server` is an address on its own; the export is a separate field. Writing
`10.0.0.5:/exports` into `server` is refused, because it would build
`10.0.0.5:/exports:/exports`.

The mount point *is* the pool's host path: volumes live under it, placement and
the orphan sweep treat it exactly as they treat a shared directory, and it must
sit under `[storage] allowed_paths` like any other.

Three things worth knowing:

* **Agents create the mount point but not the pool.** That is safe here and not
  for `shared_dir`, because an `nfs` pool is usable only once `/proc/mounts`
  shows that export mounted there. A failed mount leaves a bare directory that
  still reports unusable.
* **The source is checked, not just the path.** A directory that is a mount of
  some *other* export is not this pool, and accepting it would put the pool's
  volumes on a stranger's filesystem.
* **Nothing is ever unmounted.** Deleting a pool leaves its mount in place: a
  guest may have a disk open on it, and tidying up is not worth taking a VM's
  storage away. Unmount by hand once you are sure.

### Not yet: LVM thin, RBD, iSCSI, NVMe-oF

These are the kinds that are not directories, and they need two things the
platform does not have yet. Volume provisioning would have to move into the
agent — an LV is not a file the control plane can `qemu-img create` — and a
pool would have to declare whether its bytes are **shared between hosts or local
to one**. Everything about placement currently rests on "a host reporting a pool
can see that pool's data", which is false for local storage: a VM there is
pinned to its host, and a live migration to another host has to be refused
rather than attempted. Adding a block kind before that would break the
assumption quietly, which is the failure mode this whole area exists to remove.

## Creating one

Console: **Storage pools** → **Add pool**. Or:

```bash
curl -X POST https://vquasar/api/v1/storage-pools \
  -H 'content-type: application/json' \
  -d '{"name":"fast","kind":"shared_dir","path":"/srv/fast"}'
```

The path (or, for `nfs`, the mount point) must sit under one of
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

See [ADR-023](../DESIGN.md) for the reasoning, and §20 for where pools sit in
the architecture.
