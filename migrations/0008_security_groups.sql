-- Security groups: stateful per-NIC L3/L4 filtering (design M13c).
--
-- A NIC references security groups by id in its spec. A NIC with no group is
-- unfiltered (opt-in). A NIC with ≥1 group is default-deny ingress + allow
-- egress + stateful (established/related always allowed); ingress rules are the
-- allow-list. Rules keep a `direction` column for future egress enforcement,
-- but v1 enforces ingress only (egress is allowed).

CREATE TABLE security_groups (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    description TEXT,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

CREATE TABLE security_group_rules (
    id                UUID PRIMARY KEY,
    security_group_id UUID NOT NULL REFERENCES security_groups(id) ON DELETE CASCADE,
    direction         TEXT NOT NULL DEFAULT 'ingress',  -- ingress | egress
    ethertype         TEXT NOT NULL DEFAULT 'IPv4',      -- IPv4 | IPv6
    protocol          TEXT NOT NULL DEFAULT 'any',       -- tcp | udp | icmp | any
    port_min          INTEGER,                           -- tcp/udp only
    port_max          INTEGER,
    remote_cidr       TEXT,                              -- NULL ⇒ any
    created_at        TIMESTAMPTZ NOT NULL
);

CREATE INDEX security_group_rules_sg_idx ON security_group_rules (security_group_id);
