-- Per-network default security group (design §18, ADR-017).
--
-- A NIC with no security groups was unfiltered: control sent filtered=false and
-- the agent cleared every flow for that TAP. Absence of configuration meant
-- absence of enforcement. Effective policy now becomes
--
--     network.default_security_group  ∪  nic.security_groups
--
-- so an empty NIC group set means "the network's default applies", never
-- "unfiltered". "Open" becomes an explicit allow-any rule in a real security
-- group — an object that can be read, audited and tightened — instead of the
-- absence of one.
--
-- Every existing network is seeded a *permissive* default here, so reachability
-- on a running cluster is byte-for-byte what it was. Tightening is then a
-- visible, reversible, per-network operator action rather than a flag day.

ALTER TABLE security_groups
    ADD COLUMN managed BOOLEAN NOT NULL DEFAULT false;

ALTER TABLE networks
    ADD COLUMN default_security_group_id UUID REFERENCES security_groups(id);

-- Seed one managed group per existing network, keyed on network_id (not name —
-- networks.name has no uniqueness constraint).
CREATE TEMP TABLE seeded_defaults ON COMMIT DROP AS
SELECT n.id AS network_id, gen_random_uuid() AS sg_id, n.name AS network_name
FROM networks n;

INSERT INTO security_groups (id, name, description, managed, created_at, updated_at)
SELECT sg_id,
       'default-' || network_name,
       'Network default policy. Seeded permissive on upgrade to preserve the '
       || 'behaviour of NICs that had no security group; tighten before relying on it.',
       true,
       now(),
       now()
FROM seeded_defaults;

-- Allow-any ingress, both ethertypes: exactly "unfiltered", stated explicitly.
INSERT INTO security_group_rules
    (id, security_group_id, direction, ethertype, protocol, remote_cidr, created_at)
SELECT gen_random_uuid(), s.sg_id, 'ingress', e.ethertype, 'any', NULL, now()
FROM seeded_defaults s
CROSS JOIN (VALUES ('IPv4'), ('IPv6')) AS e(ethertype);

UPDATE networks n
   SET default_security_group_id = s.sg_id
  FROM seeded_defaults s
 WHERE n.id = s.network_id;
