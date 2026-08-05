# Encrypting the VXLAN underlay

Tenant networks are VXLAN overlays carried over the management network. By
default those tunnels are **cleartext and unauthenticated**: anyone who can read
the underlay can read every overlay, and anyone who can send UDP/4789 to a host
can inject frames into any VNI. A `tenant` network's whole purpose is to be a
distinct, isolated L2 domain, and that claim does not hold until this is on.

Encryption is IPsec between host pairs, authenticated with the same internal CA
that already secures control ↔ agent gRPC.

## Roll it out in two steps

**Do not skip straight to `ipsec`.** The guest MTU has to shrink to make room
for ESP, and MTU is rendered into cloud-init *at seed time* — a running VM never
picks up a new value. Enable encryption first and every existing overlay VM is
34 bytes over, with a failure that is genuinely nasty to diagnose: ARP works,
ping works, the TCP handshake works, and then the first full-size segment
disappears. The ICMP that would normally signal it is addressed to the guest's
overlay IP and emitted by the host stack, which has no route onto the overlay
bridge — so the guest never learns, and it looks like an application hang.

### Step 1 — reserve the headroom

```toml
[network]
overlay_encryption = "reserve"
underlay_mtu = 1500          # raise for a jumbo underlay
```

Tunnels stay cleartext, but new seeds render MTU 1416 instead of 1450. Re-seed
or reboot the overlay VMs, then confirm from inside a guest:

```bash
ip link show eth0 | grep mtu           # expect 1416
ping -M do -s 1388 <peer-overlay-ip>   # 1388 + 28 = 1416, must succeed
ping -M do -s 1389 <peer-overlay-ip>   # must fail
```

If the second ping succeeds, the guest has not picked up the new MTU and step 2
will blackhole it.

### Step 2 — turn on IPsec

Install the OVS IPsec package on **every** host first — the agent configures
OVS, but the IKE daemon does the work:

```bash
# Debian/Ubuntu (pulls strongSwan)
apt install openvswitch-ipsec
# RHEL/Rocky/Alma (pulls libreswan)
dnf install openvswitch-ipsec
```

OVS supports both IKE daemons behind the same OVSDB configuration, which is why
vquasar drives it through OVS rather than talking to one daemon directly.

Then:

```toml
[network]
overlay_encryption = "ipsec"
```

The control plane tells each agent which peers to protect and what certificate
identity to expect from each. The agent creates a `br-vxipsec` bridge holding
one anchor tunnel per peer — **not** per VNI, because the IPsec traffic selector
is `(peer_ip, udp/4789)` and carries no VNI, so a single association protects
every overlay between two hosts.

## Verifying it is actually on

The control plane logs its state at every start, and warns while tunnels are
cleartext. That tells you what was *asked for*. To confirm what is *happening*,
check on a host — and check on the **receiving** side, since a sending host can
observe its own packets before encryption:

```bash
# OVS's own view; the tunnel sections must be populated
ovs-appctl -t ovs-monitor-ipsec tunnels/show

# Kernel policy: expect transport mode, proto esp, udp port 4789 in the selector
ip xfrm policy
ip -s xfrm state | grep -A3 'proto esp'    # counters must climb under load

# The decisive test, on the receiver
tcpdump -nni <underlay-nic> host <peer> and proto esp       # must show ESP
tcpdump -nni <underlay-nic> host <peer> and udp port 4789   # must show nothing
```

`nstat -az | grep -i Xfrm` shows the drop counters (`XfrmInNoPols`,
`XfrmInPolBlock`, `XfrmInStateInvalid`) when something is misconfigured.

## Certificates

Agent certificates are reused as IPsec identities, with two requirements that
are easy to get wrong:

* **The subject needs a second RDN after the CN** — `/O=vquasar/CN=agent-host1`,
  not `/CN=agent-host1`. `ovs-monitor-ipsec` extracts the CN from RFC2253 output
  with a regex requiring a trailing comma, and RFC2253 reverses RDN order, so a
  single-RDN subject never matches. It then reports every tunnel as missing its
  certificate, which points you at entirely the wrong problem.
* **The CN must also appear as a `DNS:` SAN**, because the peer identity is sent
  as a bare string that strongSwan treats as `ID_FQDN`, and an `ID_FQDN` can only
  match a `dNSName` SAN.

This failure has been reproduced against `openvswitch-ipsec` 3.3.4: the daemon
logs `No CN in the certificate subject`, marks the whole credential
configuration invalid, and then reports every tunnel as
`must set 'certificate' as local certificate` — pointing at a setting that is
in fact correct.

`gen-certs.sh` and the enrollment endpoint both do this. Enrollment additionally
records the CN on the host record and **rejects a CSR with a single-RDN subject**
rather than issuing a certificate that silently cannot be used for IPsec.

A host enrolled before this existed has no recorded CN. Its tunnels still get
IPsec, but cannot be identity-pinned — meaning any certificate the CA signed is
accepted for that peer, so one compromised host could impersonate another. The
agent warns for each such peer. Re-enroll those hosts.

**Rotation**: overwriting a certificate in place does not reliably reload the
strongSwan path. Write the renewal to a new path and update the config, or
restart `openvswitch-ipsec`. A silent expiry takes down every cross-host overlay
at once, so tie renewal to the same flow that renews the gRPC identity.

## What this does not solve

* **Injection.** A VXLAN packet from an *unconfigured* source IP matches no
  IPsec policy and is delivered normally. Encryption protects the pairs you have
  configured; it does not stop a stranger. That needs a host ingress filter
  accepting only ESP-protected traffic on UDP/4789 — tracked as M18c, and the
  one piece that differs between distributions.
* **Performance.** Expect roughly 2–6 Gbit/s per flow on AES-NI x86. Because one
  association carries all VNIs between a host pair, a single hot VM-to-VM flow
  across two hosts is single-SA and effectively single-core on the crypto path.
* **A half-converted fleet.** A peer without IPsec configured does not fall back
  to cleartext — it fails closed. Convert every host before relying on it.
