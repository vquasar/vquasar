# Encrypting the connection to PostgreSQL

The control plane holds every secret the platform has: cloud-init passwords and
SSH keys (sealed, but the keyring lives in the same process), enrollment tokens,
the whole desired state of the fleet. All of it travels over the PostgreSQL
connection. That link is the one hop in vquasar that is not encrypted by
default.

## Why the default is not enough

The PostgreSQL driver defaults to libpq's `prefer` mode: it tries TLS, and if
the server does not offer it, it **connects in plaintext anyway and says
nothing**. A server that quietly loses its certificate, or an attacker who can
strip the TLS negotiation, downgrades you silently.

vquasar therefore logs the effective mode at startup, and warns when it is one
that permits plaintext:

```
WARN database connection is NOT required to be encrypted — set [database]
     ssl_mode = "verify-full" (and ca) to enforce TLS   ssl_mode=prefer
```

Setting a verifying mode turns that into a hard requirement — a server without
TLS, or with a certificate you do not trust, becomes a startup failure instead
of a silent downgrade.

## Configuration

```toml
[database]
url      = "postgres://ch:secret@db.lab:5432/vquasar"
ssl_mode = "verify-full"
ca       = "/etc/vquasar/certs/pg-ca.crt"
```

| Key        | Meaning                                                          |
| ---------- | ---------------------------------------------------------------- |
| `ssl_mode` | `disable`, `allow`, `prefer`, `require`, `verify-ca`, `verify-full` |
| `ca`       | CA (PEM) that signed the PostgreSQL server certificate            |
| `cert`     | Client certificate (PEM), for PostgreSQL certificate auth         |
| `key`      | Client private key (PEM) matching `cert`; keep it `0600`          |

Or as environment variables — `VQUASAR_CONTROL_DATABASE__SSL_MODE`,
`__CA`, `__CERT`, `__KEY` — or at install time:

```bash
scripts/install.sh control \
  --db-url postgres://ch:secret@db.lab:5432/vquasar \
  --db-ssl-mode verify-full --db-ca /etc/vquasar/certs/pg-ca.crt
```

The modes, in increasing strictness:

* `disable` / `allow` / `prefer` — **can end up unencrypted**. `prefer` is the
  default when nothing is set.
* `require` — TLS is mandatory, but the certificate is only verified if a `ca`
  is configured. Encrypted, not authenticated: it stops passive sniffing, not an
  active man-in-the-middle.
* `verify-ca` — TLS, and the server certificate must chain to `ca`.
* `verify-full` — as above, **and** the hostname in `url` must match the
  certificate. This is the one to use. It is the only mode that defends against
  someone answering on the database's address with their own valid certificate.

`sslmode=` and `sslrootcert=` in the connection URL still work and are still
honoured; the `[database]` keys win where both are given. Certificate files are
read at startup, so a missing or unreadable one fails immediately with the path
in the message rather than surfacing later as a connection error.

## Setting the server side up

PostgreSQL needs a certificate whose subject alternative name matches the
hostname you put in `url` — `verify-full` checks it.

```bash
# A CA for the database (or reuse the internal CA from scripts/gen-certs.sh).
openssl req -x509 -newkey rsa:2048 -nodes -keyout pg-ca.key -out pg-ca.crt \
  -days 3650 -subj "/CN=vquasar-pg-ca"

# Server certificate for the name clients will connect to.
openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr \
  -subj "/CN=db.lab"
printf 'subjectAltName=DNS:db.lab\n' > san.ext
openssl x509 -req -in server.csr -CA pg-ca.crt -CAkey pg-ca.key -CAcreateserial \
  -out server.crt -days 365 -extfile san.ext

chmod 600 server.key && chown postgres:postgres server.key server.crt
```

Then in `postgresql.conf`:

```
ssl = on
ssl_cert_file = '/etc/postgresql/certs/server.crt'
ssl_key_file  = '/etc/postgresql/certs/server.key'
```

`ssl` is reloadable — `SELECT pg_reload_conf();` is enough, no restart.

Enabling TLS on the server does not by itself stop plaintext clients. To require
it, change the `host` lines in `pg_hba.conf` to `hostssl`.

## Checking it worked

From the control plane's own logs:

```
INFO database connection requires TLS   ssl_mode=verify-full
INFO connecting to PostgreSQL           database=postgres://***@db.lab:5432/vquasar
INFO migrations applied
```

From the server, which is the answer that actually counts:

```sql
SELECT a.usename, s.ssl, s.version, s.cipher
FROM pg_stat_ssl s JOIN pg_stat_activity a USING (pid)
WHERE a.datname = 'vquasar';
```

```
 usename | ssl | version |         cipher
---------+-----+---------+------------------------
 ch      | t   | TLSv1.3 | TLS_AES_256_GCM_SHA384
```

`ssl = f` on a connection you believe is encrypted means the mode is still
permissive somewhere — check the log line above, and remember a `sslmode=` in
the URL is overridden by `[database] ssl_mode`.

## What this does not cover

* **The database at rest.** Field encryption (`[encryption]`) protects
  cloud-init secrets in the rows; everything else — VM specs, host inventory,
  events — is stored in the clear. Disk-level encryption on the database host is
  an operational task outside vquasar.
* **Database credentials.** They live in the connection URL, in the systemd
  environment file (`0600`). Client-certificate authentication (`cert`/`key`
  with `pg_hba.conf` set to `cert`) avoids the password entirely and is the
  better answer where you can arrange it.
* **The agents.** They never talk to PostgreSQL — only to the control plane,
  over mutual TLS.
