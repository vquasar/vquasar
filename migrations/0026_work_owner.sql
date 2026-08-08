-- Which instance owns a piece of in-flight work (design §48, ADR-021).
--
-- Importing an image and provisioning a volume run in a detached task after the
-- row is persisted, and the row is reclaimed at startup because a process that
-- has just started owns no detached tasks (0023, `recovery.rs`).
--
-- That reasoning holds for exactly one control plane. With several, a
-- restarting instance would reclaim work another instance is still doing — a
-- download killed at 90%, or worse, a volume row deleted from under a running
-- `qemu-img convert`. The row has to say whose it is.
--
-- NULL means a row created before this column existed. Reclaiming those is
-- right: they can only have been written by a binary that predates HA, which by
-- definition is not one of the instances now running.

ALTER TABLE images  ADD COLUMN owner TEXT;
ALTER TABLE volumes ADD COLUMN owner TEXT;

-- The sweep filters on (status, owner); both are small, so one index each on
-- the transitional rows is enough and costs nothing on the ready ones.
CREATE INDEX images_importing_idx  ON images (owner) WHERE status = 'importing';
CREATE INDEX volumes_provisioning_idx ON volumes (owner) WHERE status = 'provisioning';
