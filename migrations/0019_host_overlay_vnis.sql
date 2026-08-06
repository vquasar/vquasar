-- VNIs each host currently carries an overlay bridge for (design §18).
--
-- A released VXLAN segment is quarantined rather than reused immediately,
-- because a host that has not yet torn down `vxbr<vni>` still has a live tunnel
-- mesh for it — and a new network handed that VNI would silently adopt the
-- mesh. Time alone is a guess at when teardown finished; this records what the
-- hosts actually report, so the quarantine can end on evidence instead.
ALTER TABLE hosts ADD COLUMN overlay_vnis INTEGER[] NOT NULL DEFAULT '{}';
