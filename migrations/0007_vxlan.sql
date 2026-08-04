-- VXLAN overlay networks (design M13b).
--
-- A network with a `vni` is a VXLAN overlay: an isolated L2 segment that spans
-- hosts over the management underlay, with no dependency on a physical-switch
-- VLAN trunk. `vni` is mutually exclusive with the 802.1Q `vlan` column (that is
-- the flat/VLAN mode). NULL vni keeps the existing flat/VLAN behaviour.
ALTER TABLE networks ADD COLUMN vni INTEGER;

-- VNIs are globally unique across the deployment (they key the wire encap).
CREATE UNIQUE INDEX networks_vni_uidx ON networks (vni) WHERE vni IS NOT NULL;
