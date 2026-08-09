# Installing vquasar

One command per machine. The control plane goes on one host; an agent goes on
every host that will run VMs.

```bash
# The control plane — API, console, scheduler, database.
curl -fsSL https://raw.githubusercontent.com/vquasar/vquasar/main/scripts/install.sh \
  | sudo sh -s -- control --allow-no-auth

# Each hypervisor host.
curl -fsSL https://raw.githubusercontent.com/vquasar/vquasar/main/scripts/install.sh \
  | sudo sh -s -- agent --advertise-host 10.0.0.11
```

That downloads the latest stable build, checks it, installs the binary to
`/usr/local/bin`, writes `/etc/vquasar/<role>.env`, generates a systemd unit,
and starts it. The control tarball carries the web console too, so a control
plane is never installed without the UI that matches it.

`--allow-no-auth` is a deliberate speed bump: without it the installer refuses
to bring up a control plane that anyone can talk to. It is right for a lab and
wrong for anything else — see [authentication and RBAC](oidc-keycloak.md).

## Channels

| Channel | What it is | How to ask for it |
| --- | --- | --- |
| `stable` | The current release. What you get by default. | `--channel stable` |
| `rc` | The newest release candidate. | `--channel rc` |
| `dev` | The tip of `main`, rebuilt on every merge. | `--channel dev` |
| — | One specific release. | `--version v0.2.0` |

`dev` is a moving tag: `--channel dev` always means the current tip, and the
artifact still names the commit it came from, so a build can be traced back to a
tree even though the tag moves.

## What the installer checks

Two things, and it tells you which of them actually ran:

* **Checksums**, always. `SHA256SUMS` is published with every release and the
  downloaded tarball is checked against it before anything is unpacked.
* **Provenance**, when [`gh`](https://cli.github.com) is installed. Every
  artifact carries a Sigstore attestation naming the workflow, repository and
  commit that produced it — a much stronger claim than a checksum, which only
  says the bytes match a list published alongside them.

To check by hand, before or after installing:

```bash
gh attestation verify vquasar-control-*.tar.gz --repo vquasar/vquasar
```

Each release also publishes a CycloneDX **SBOM**, itself attested. It answers
"what is in this build" without unpacking it.

If `gh` is not present the installer says so rather than implying both checks
passed. `--no-verify` skips the provenance step; nothing skips the checksum.

## Upgrading

The same command. The installer replaces the binary and restarts the unit;
`/etc/vquasar/<role>.env` is left alone unless you pass `--force-config`, so an
upgrade never quietly rewrites your configuration.

The control plane applies database migrations at start-up, so an upgrade needs
no separate step. Migrations are additive and backward-compatible by policy
(ADR-005): a newer schema keeps working with the binary that preceded it, which
is what makes a rollback possible.

## Building it yourself

```bash
make check     # everything CI runs: fmt, clippy, tests, console
make dist      # release tarballs and SHA256SUMS in ./dist
make install-control   # build and install on this machine
```

`make dist` produces the same layout the release workflow publishes, so a
locally built tarball installs by the same path as a downloaded one. What it
cannot produce is the attestation: that is signed by the workflow's own
identity, and a signature you can mint locally proves nothing about where a
file came from.

## What the installer does not do

* **PostgreSQL.** The control plane needs a database; point it at one with
  `--db-url`. See [encrypting the connection](postgres-tls.md).
* **Open vSwitch.** Agents need it for VM networking —
  [`scripts/setup-ovs.sh`](../scripts/README.md).
* **Cloud Hypervisor.** Agents need the binary at
  `/var/lib/vquasar/bin/cloud-hypervisor`, or pass `--ch-binary`.
* **Certificates.** Control ↔ agent traffic should be mutually authenticated;
  [`scripts/gen-certs.sh`](../scripts/README.md) makes an internal CA, and
  agents can enrol themselves afterwards (design M16).

Each is a decision about your environment rather than something an installer
should guess, and a wrong guess about any of them is worse than being asked.

## Supported platforms

x86-64 Linux with systemd and glibc 2.35 or newer (Ubuntu 22.04 and later,
Debian 12, RHEL 9). The published binaries are dynamically linked; on an older
distribution, build from source.
