# Registering hosts

Installing an agent does not put its host in the fleet. The control plane has to
be told the host exists and where to reach it — until then the agent is running
and nothing is talking to it.

The direction matters and explains most first-time confusion: **the control
plane dials the agent.** An agent never calls in, never announces itself, and
never registers itself. So the endpoint you give must be one the *control plane*
can reach, and the agent's port must be open to it. (Nothing dials the control
plane over gRPC — it has no gRPC port at all.)

There are two ways to do it. Use enrolment unless you already have a
certificate workflow.

## Enrolment (recommended)

One call on the control plane, one flag on the agent. The agent generates its
own private key, which never leaves the host, and gets a certificate signed for
it.

**1. Create the host and get a one-time token** — needs `host:manage`:

```bash
curl -sS -X POST https://control.example:8080/api/v1/hosts/enroll \
  -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"hv-01","endpoint":"https://10.0.0.11:9500"}'
```

```json
{
  "host_id": "…",
  "token": "…",          // shown once
  "bootstrap_url": "https://control.example:8080/api/v1/enroll/sign",
  "ca_cert": "-----BEGIN CERTIFICATE-----…",
  "expires_in_secs": 3600
}
```

The token is shown once and expires. It authorises exactly one certificate, for
exactly this host.

**2. Install the agent with it:**

```bash
curl -fsSL https://raw.githubusercontent.com/vquasar/vquasar/main/scripts/install.sh \
  | sudo sh -s -- agent \
      --name hv-01 \
      --advertise-host 10.0.0.11 \
      --bootstrap-token "$TOKEN" \
      --bootstrap-url https://control.example:8080/api/v1/enroll/sign \
      --bootstrap-ca /tmp/control-ca.pem
```

The agent generates a key and a CSR, posts the CSR with the token, and installs
the certificate it gets back. `--bootstrap-ca` is the CA the agent must trust to
reach the control plane *during* bootstrap — before it has a certificate of its
own there is nothing else to anchor that first call to, and skipping it would
mean handing a token to whoever answered.

This requires an issuing CA on the control plane (`--tls-issuer-cert` /
`--tls-issuer-key` at install time). Without one, `/hosts/enroll` is not
mounted and you want the manual path.

## Manual registration

When certificates come from somewhere else — your own PKI, or
[`scripts/gen-certs.sh`](../scripts/README.md) for a lab.

```bash
curl -sS -X POST https://control.example:8080/api/v1/hosts \
  -H "authorization: Bearer $TOKEN" \
  -H 'content-type: application/json' \
  -d '{"name":"hv-01","endpoint":"https://10.0.0.11:9500"}'
```

Then install the agent with the certificate you issued:

```bash
sudo sh install.sh agent \
  --name hv-01 --advertise-host 10.0.0.11 \
  --tls-ca /etc/vquasar/tls/ca.pem \
  --tls-cert /etc/vquasar/tls/hv-01.pem \
  --tls-key /etc/vquasar/tls/hv-01-key.pem \
  --tls-control-cn control
```

`--tls-control-cn` is the Common Name the control plane's client certificate
must carry. Chaining to your CA is *not* identity: without pinning the CN, any
certificate your CA ever issued — including one issued to a guest — could drive
the agent. See [design §30](../DESIGN.md).

The console can do the same thing: **Hosts → Register**.

## What happens next

The control plane dials every registered host on each reconcile tick (5s by
default). A host starts `NotReady` and becomes `Ready` on the first successful
call, which also collects its inventory: CPU model and features, memory, kernel
and Cloud Hypervisor versions, the agent's own build, and which
[storage pools](storage-pools.md) it can actually use.

```bash
curl -sS https://control.example:8080/api/v1/hosts -H "authorization: Bearer $TOKEN"
```

If it stays `NotReady`, the control plane records a `host.unreachable` event
with the reason. In order of likelihood:

* the endpoint is not reachable **from the control plane** — a hostname it
  cannot resolve, or an address that is only routable from somewhere else;
* the agent's port (9500 by default) is closed between the two;
* the certificates do not chain, or the CN pinning rejects the caller;
* the agent is not running: `systemctl status vquasar-agent`.

A `Ready` host is not yet a *usable* one. It also has to report the storage
pools a VM's disks need, or the scheduler will refuse to place onto it — with a
reason saying so rather than a capacity error.

## Removing a host

Cordon it first so nothing new lands there, then drain it to move its VMs:

```bash
curl -sS -X PATCH .../api/v1/hosts/$ID -d '{"schedulable":false}'
curl -sS -X POST  .../api/v1/hosts/$ID/drain
```

Drain reports each VM it could not move and why — no compatible CPU, no host
that reports its storage, or pinned to this host by local storage or an explicit
placement. Those are not the same problem and it does not pretend they are.

VMs are **never** moved off an unreachable host automatically. Restarting a
guest elsewhere while it may still be running on a host you cannot see is how
two copies end up writing to one disk; doing it safely needs fencing, which
vquasar does not have yet ([ADR-014](../DESIGN.md)).
