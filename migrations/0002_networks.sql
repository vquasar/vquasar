-- Virtual networks (design document, section 18).
-- The MVP models a network as an optional 802.1Q VLAN on each host's
-- integration bridge (br-int); NULL vlan is a flat/untagged provider network.

CREATE TABLE networks (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL,
    vlan        INTEGER,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);
