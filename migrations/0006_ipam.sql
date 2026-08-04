-- Control-plane IPAM: static IP assignment (design M13a).
--
-- A network becomes "managed" (control-plane IPAM instead of external DHCP) as
-- soon as it carries a subnet for a family. Both families are optional and
-- independent, so a network can be v4-only, v6-only, dual-stack, or (all NULL)
-- an unmanaged DHCP/flat network exactly as before — fully backward compatible.

ALTER TABLE networks
    ADD COLUMN cidr_v4      TEXT,          -- e.g. 172.16.56.0/24
    ADD COLUMN gateway_v4   TEXT,
    ADD COLUMN cidr_v6      TEXT,          -- e.g. fd00:56::/64
    ADD COLUMN gateway_v6   TEXT,
    ADD COLUMN dns          TEXT[] NOT NULL DEFAULT '{}',  -- resolvers, any family
    ADD COLUMN pool_v4_start TEXT,         -- optional; default = usable range
    ADD COLUMN pool_v4_end   TEXT,
    ADD COLUMN pool_v6_start TEXT,
    ADD COLUMN pool_v6_end   TEXT;

-- One row per assigned address. A dual-stack NIC holds two rows (v4 + v6). The
-- unique (network_id, ip) prevents double-assignment; index on vm_id makes
-- release-on-delete cheap.
CREATE TABLE ip_allocations (
    id           UUID PRIMARY KEY,
    network_id   UUID NOT NULL REFERENCES networks(id) ON DELETE CASCADE,
    ip           TEXT NOT NULL,
    family       SMALLINT NOT NULL,        -- 4 or 6
    vm_id        UUID,                     -- NULL = statically reserved, no VM
    nic_index    INTEGER NOT NULL,
    mac          TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    UNIQUE (network_id, ip)
);

CREATE INDEX ip_allocations_vm_idx ON ip_allocations (vm_id);
CREATE INDEX ip_allocations_network_idx ON ip_allocations (network_id);
