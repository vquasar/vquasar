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

**Every address the console answers on must be registered with the identity
provider.** The console is served by *every* instance, so it can be reached at
the VIP and at each node — but the OIDC client only permits the redirect URIs
and web origins it was told about, and a login anywhere else fails with
`redirect_uri`. Pass `--ui-origin` once per address to
[`keycloak-setup.sh`](../scripts/keycloak-setup.sh), or add them to the client
afterwards. Registering the per-node addresses as well as the VIP is
deliberate: reaching a node directly is exactly what you want when the VIP is
somewhere else.

**That address must be in every certificate's SAN.** The VIP is the name
clients connect to, so each instance's certificate needs it alongside its own
host — otherwise the connection fails verification exactly when the VIP moves.
Easy to miss, because it works until the first failover.

**`[enrollment] control_url` must be the VIP, before you create any VMs.** It is
rendered into cloud-init at seed time, so a guest keeps whatever address it was
built with. Changing it later does not move existing guests; they have to be
re-seeded or recreated.

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

In practice, the things that are easy to forget:

1. **The whole config, not just the database URL.** The encryption key and key
   id especially: an instance with a different key cannot decrypt sealed
   cloud-init, so every VM it reconciles fails. The issuing CA (`[tls]
   issuer_cert` / `issuer_key`) too, or that instance cannot sign enrolments.
   The PostgreSQL CA if you use `verify-full` — the startup check names the
   missing path, which is the intended way to find this out.
2. **Shared storage.** Control runs `qemu-img` on it for image and volume work,
   so every instance needs the same mount at the same path.
3. **Verify the binary you deployed.** Compare checksums with the build host
   rather than trusting that a copy succeeded — a copy that raced a rebuild
   produces a plausible-looking file of the same size, and the symptom is a
   behaviour that "should work" mysteriously not working.

## A VIP with keepalived

One package, no cluster manager. The health check is what matters:

```
vrrp_script check_control {
    script "/usr/bin/curl -sk -f -m 2 https://127.0.0.1:8080/healthz"
    interval 2
    rise 2
    fall 2
    weight 0
}

vrrp_instance vquasar_api {
    state MASTER           # BACKUP on the others
    interface br0
    virtual_router_id 85
    priority 110           # lower on the others
    advert_int 1
    authentication { auth_type PASS
                     auth_pass <something> }
    virtual_ipaddress { 10.0.0.85/24 }
    track_script { check_control }
}
```

**The VIP follows a serving instance, not the leader.** The API is
active/active — every instance answers every request, and only the background
loops are single-leader. Tying the address to leadership would move it for no
reason and route around a perfectly healthy node.

`/healthz` rather than `systemctl is-active`: it proves the process is answering
HTTPS, and an instance that cannot reach PostgreSQL fails it while systemd still
calls the unit active.

Two things that will silently stop this working, both found the hard way:

* **SELinux.** `keepalived_connect_any` is off by default on RHEL-family
  systems, so the daemon cannot connect to the API it is checking. The node sits
  in `FAULT` and refuses the VIP while looking healthy, and running the same
  curl by hand works — which is what makes it confusing.
  `setsebool -P keepalived_connect_any on`.
* **The firewall.** VRRP is IP protocol 112, not a TCP port:
  `firewall-cmd --add-protocol=vrrp --permanent`. Use `--permanent` — a reload
  drops anything that was only added at runtime.

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

## What it measures out at

From a two-node lab (one VIP, three hypervisor hosts, three running VMs):

| Event | Time to recover |
| --- | --- |
| Leader stopped cleanly (`systemctl stop`) | **~4s** — the lease is handed back immediately, and the standby acquires on its next 5s tick |
| Leader lost without warning (crash, power) | **~15s** — the full lease TTL, which is what the TTL is for |
| VIP move when an instance stops serving | **~5s** — two failed health checks at 2s intervals |

Nothing was disturbed on the fleet during any of it: hosts stayed polled within
a second, and no VM changed state. That is the point of the design — the loops
are idempotent, so a gap in leadership is a gap in *progress*, not in service.

If a clean stop is taking the full TTL, the process is not receiving SIGTERM or
is being killed outright. Check for `vquasar-control stopped` in the journal: if
it is absent, the graceful path did not run.

## Limits

* **The database is still single.** Use Patroni or a managed PostgreSQL if you
  need that covered too.
* **One leader runs every controller.** They are not split individually, so a
  controller wedged on something slow delays the others.
* **Agent-side fencing is not implemented.** A leader paused inside its safety
  margin and resuming outside it could, in principle, issue one stale migration
  call. Closing that needs the agent to reject stale callers by epoch, which is
  a protocol change and a separate milestone; the epoch is already recorded.
