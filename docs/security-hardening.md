# Security hardening

Notes on three defects found in a review of the pre-tenancy codebase, what they
allowed, and what changed. All three were exploitable in a single-tenant
install, not only under the multi-tenancy work that prompted the review.

## An agent certificate was also a control credential

`scripts/gen-certs.sh` and the enrollment endpoint both issued agent leaves with
`extendedKeyUsage = serverAuth, clientAuth`, and the agent's gRPC server
required only that the peer certificate chain to the internal CA. Every agent
certificate chains to that CA.

So an attacker who compromised one host and read `/etc/vquasar/tls/agent.key`
could dial **every other agent** and call `EnsureVm`, `DeleteVm` or `VmConsole`
against VMs on those hosts, without ever touching the control plane. That is a
direct violation of the rule the whole split rests on (design §30): a host
compromise must not become a control-plane compromise.

Two changes:

* Agent certificates are issued `serverAuth` only. They identify a server; they
  are not a credential for talking to one. The control certificate keeps
  `clientAuth`, because it is the party that dials agents.
* The agent verifies the peer's **identity**, not just its issuer. The control
  plane's client certificate must carry the Common Name in `[tls] control_cn`,
  which defaults to `control` — what `gen-certs.sh` issues.

```toml
[tls]
ca   = "/etc/vquasar/tls/ca.crt"
cert = "/etc/vquasar/tls/agent.crt"
key  = "/etc/vquasar/tls/agent.key"
control_cn = "control"
```

or `scripts/install.sh agent --tls-control-cn control`.

**Upgrading**: if your control certificate was issued with a different Common
Name, agents will refuse it after this change and log the mismatch:

```
WARN rejected gRPC peer: not the control plane (set [tls] control_cn if this is wrong)
     peer_cn=my-control expected=control
```

Set `control_cn` to match. Existing agent certificates keep working — the fix
that matters is the peer check, not the EKU; re-issuing agent certificates
without `clientAuth` is defence in depth for when the check is misconfigured.

## `vm:read` returned decrypted cloud-init secrets

Cloud-init passwords, raw user-data and SSH keys are sealed at rest with
AES-256-GCM and unsealed at the store boundary, so the reconcile loop can hand
plaintext to the agent for the seed ISO. `GET /vms` and `GET /vms/{id}`
serialized that same unsealed spec straight back to the caller — as did
`GET /templates`.

The built-in `viewer` role holds `vm:read`. The most restricted role in the
system could therefore read every VM's guest password. Encryption at rest was
doing its job; it just was not the boundary anyone assumed it was.

Responses now carry `__redacted__` in place of each secret. The marker is
deliberately visible rather than dropped, so a caller can still see *that* a
password is set. Because template updates submit a whole cloud-init block, an
update that echoes the marker back resolves it against the stored value instead
of overwriting the secret with the marker — so editing a template in the UI
without retyping the password no longer destroys it.

Secrets still reach the agent in full over mutual TLS. Nothing about the seed
ISO changes.

## A VM spec could name any file on the host

`validate()` rejected only *empty* disk, kernel and firmware paths. Nothing
confined them to the platform's own storage, and the agent opens whatever the
control plane tells it to.

`vm:create` was therefore a read primitive over the host filesystem: attach
`/etc/vquasar/tls/agent.key` as a raw disk and read it from inside your own
guest. `POST /images` had the same shape through its free-form `source_path`.

Caller-supplied paths must now sit under an allow-listed root:

```toml
[storage]
allowed_paths = ["/var/lib/vquasar"]
```

Relative paths and any `.`/`..` component are rejected outright; prefixes are
compared component-wise, so `/var/lib/vquasar` does not admit
`/var/lib/vquasar-evil`. Add roots with repeated
`scripts/install.sh control --allowed-path DIR` when storage lives elsewhere.

**Upgrading**: if your images, firmware or volumes live outside
`/var/lib/vquasar`, add those roots before upgrading or VM creation will start
failing with `must be under one of the permitted storage roots`. Existing VMs
are unaffected — the check runs at admission, not on reconcile.

Symlinks are not resolved. The roots hold platform-managed storage, and a
symlink planted inside one already implies host access.

## Also changed

Raw `sqlx` errors are no longer formatted into the API response. They carried
table, column and constraint names, and sometimes values — a unique-violation
message is an existence oracle for a row the caller may not be entitled to know
about. The detail is logged with the request id; the caller gets a constant
message.

## Not addressed here

The review raised more than this. Still open, roughly in severity order:

* The console WebSocket authorizes the *permission* but never checks that the
  caller has any relationship to the VM, and `vm:console` is granted to
  `viewer`. Interactive serial access is not read-only.
* `POST /phone-home/{vm_id}` is unauthenticated and writes VM state.
* NICs with no security group are unfiltered, there is no anti-spoofing on any
  TAP, and `NetworkInterfaceSpec.mac` is honoured verbatim from the request.
* All flat networks share one untagged L2 domain on `br-int`, and callers may
  choose their own VLAN tag or VXLAN VNI.
* Task and event streams are global and name resources in free text.
* No upper bound on requested vCPU, memory, disk size or image-import size.

The network items are the substance of "per-tenant network isolation" and are
prerequisites for multi-tenancy meaning anything at the dataplane.
