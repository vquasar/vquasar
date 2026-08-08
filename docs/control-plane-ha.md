# Running more than one control plane

Several `vquasar-control` instances share one PostgreSQL. All of them serve the
API; exactly one runs the controllers — the reconcile loop, the migration
controller and the sweeps — and holds a lease to say so.

This is availability for the *control plane*, not for the database. If
PostgreSQL is down, every instance is down with it.

## What you need before adding a second instance

**One certificate CN across all of them.** The agent pins the control plane's
certificate common name, so an instance presenting a different CN is rejected by
every agent — quietly, as an unreachable host. Issue each instance its own
certificate with the *same* CN, or share one.

**A stable address in front.** Agents never need to know who the leader is —
control polls them, not the other way round. But two things go the other way and
need an address that outlives any one instance: `POST /phone-home/{vm}` from
guests, and agent enrollment. A VIP or a DNS name resolving to all instances is
enough; there is nothing to make sticky, because any instance can answer.

**Distinct instance ids if you run two on one host.** The identity defaults to
the hostname and is deliberately stable across restarts. Two processes sharing a
hostname would each believe the other's lease and in-flight work were their own.

```toml
[server]
instance_id = "control-a"     # defaults to the hostname
```

## Adding one

Install the same binary and the same config, pointed at the same database, with
its own certificate (same CN) and its own listen address. Start it. That is the
whole procedure — there is no cluster to join and no membership to configure,
because the database *is* the membership.

Migrations run at startup on whichever instance starts first; the others find
them already applied.

## Checking it

```bash
curl -sk https://control-a:8080/api/v1/leader -H "Authorization: Bearer $TOKEN"
```

```json
{
  "instance": "control-a",
  "leader": {
    "holder": "control-b",
    "epoch": 7,
    "acquired_at": "2026-08-08T09:14:02Z",
    "expires_at": "2026-08-08T09:14:17Z",
    "valid": true
  },
  "is_self": false
}
```

Ask any instance and you get the same `leader`, because the answer comes from
the database rather than from the instance you reached. `is_self` tells you
which instance the load balancer sent you to.

Each instance also exports `vquasar_controller_is_leader` — `1` on the holder,
`0` on the standbys. Exactly one should ever be `1`; alert on the sum being
anything other than 1 for more than a few seconds.

`epoch` counts terms, not renewals: it increases when leadership *moves*. An
epoch climbing steadily means instances are taking the lease from each other,
which is usually a database that is slow enough to make renewals miss.

## What happens when the leader goes away

**Stopped cleanly** — it hands the lease back on shutdown, and another instance
picks it up within a renewal interval (5s). This needs the process to actually
receive the stop: `systemctl stop` and `systemctl restart` send SIGTERM, which
is handled. A `kill -9` is a crash, and behaves as one.

**Killed, or partitioned from the database** — the lease expires on its own after
15s, and another instance takes it. Nothing is lost: every controller pass reads
its work from the database at the start of the tick, so the new leader picks up
where the old one was. A migration mid-flight resumes from its persisted state.

**Paused and resumed** — a leader frozen long enough to lose its lease notices
before acting again: the controllers only run while more than half the lease
remains. The one operation where an overlap would corrupt rather than converge
is migration, and the migration controller re-checks the lease against the
database immediately before each step.

## Limits

* **The database is still single.** Use Patroni or a managed PostgreSQL if you
  need that covered too.
* **One leader runs every controller.** They are not split individually, so a
  controller wedged on something slow delays the others.
* **Agent-side fencing is not implemented.** A leader paused inside its safety
  margin and resuming outside it could, in principle, issue one stale migration
  call. Closing that needs the agent to reject stale callers by epoch, which is
  a protocol change and a separate milestone; the epoch is already recorded.
