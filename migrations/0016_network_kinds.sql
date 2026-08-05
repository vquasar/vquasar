-- Network types and segment identity (design §18, ADR-016).
--
-- Before this, a network was an IPAM record: two networks with no VLAN and no
-- VNI were the *same* untagged L2 domain on the shared integration bridge, and
-- any caller with network:create could pick a VLAN tag or VXLAN VNI — landing
-- on whatever provider segment the trunk carried, or colliding with an existing
-- overlay.
--
-- A network now declares a `kind`, which declares what it isolates, and one
-- network corresponds to exactly one L2 segment (`segment_key`, uniquely
-- indexed).
--
-- Networks that predate this migration are grandfathered rather than migrated:
-- they keep `segment_key = NULL` (excluded from the partial unique index, so
-- this migration cannot fail on a cluster whose flat networks already overlap)
-- and are flagged `legacy_segment` so the API and UI can say so. Consolidating
-- them is an operator decision made through the API — a migration must not
-- rewrite a running workload's connectivity.

ALTER TABLE networks
    ADD COLUMN kind             TEXT    NOT NULL DEFAULT 'provider',
    ADD COLUMN physical_network TEXT,
    ADD COLUMN segment_key      TEXT,
    ADD COLUMN legacy_segment   BOOLEAN NOT NULL DEFAULT false;

-- Existing rows: provider kind on the default uplink, no segment key, flagged.
-- An existing overlay (vni IS NOT NULL) becomes a tenant network and *can* be
-- given its segment key, because VNIs were already unique.
UPDATE networks
   SET kind             = CASE WHEN vni IS NOT NULL THEN 'tenant' ELSE 'provider' END,
       physical_network = CASE WHEN vni IS NULL THEN 'default' END,
       segment_key      = CASE WHEN vni IS NOT NULL THEN 'vxlan:' || vni END,
       legacy_segment   = (vni IS NULL);

ALTER TABLE networks
    ADD CONSTRAINT networks_kind_check CHECK (kind IN ('provider', 'vlan', 'tenant'));

-- One row = one L2 domain. Partial, so grandfathered rows (NULL) are exempt.
CREATE UNIQUE INDEX networks_segment_key_uidx
    ON networks (segment_key)
    WHERE segment_key IS NOT NULL;

-- Segment allocation (design §18). VNIs are allocated by the control plane and
-- are never caller-selectable. A released segment is quarantined rather than
-- reused immediately: a host that returns with a stale vxbr<vni> and a live
-- tunnel mesh must never be re-adopted by a different network.
CREATE TABLE network_segments (
    segment_key TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,              -- vxlan | vlan
    value       INTEGER NOT NULL,
    state       TEXT NOT NULL,              -- allocated | quarantined | free
    network_id  UUID REFERENCES networks(id) ON DELETE SET NULL,
    released_at TIMESTAMPTZ,
    created_at  TIMESTAMPTZ NOT NULL,
    UNIQUE (kind, value),
    CONSTRAINT network_segments_state_check CHECK (state IN ('allocated', 'quarantined', 'free'))
);

CREATE INDEX network_segments_state_idx ON network_segments (state, released_at);

-- Adopt the VNIs already in use so the allocator never hands one out twice.
INSERT INTO network_segments (segment_key, kind, value, state, network_id, created_at)
SELECT 'vxlan:' || vni, 'vxlan', vni, 'allocated', id, now()
FROM networks
WHERE vni IS NOT NULL;
