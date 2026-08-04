-- Volume snapshots (design M14c): point-in-time snapshots of a qcow2 volume.
--
-- The snapshot data lives inside the volume's qcow2 file (an internal snapshot,
-- tagged with this row's id); this table is the catalog. Raw volumes have no
-- snapshot support.

CREATE TABLE volume_snapshots (
    id         UUID PRIMARY KEY,
    volume_id  UUID NOT NULL REFERENCES volumes(id) ON DELETE CASCADE,
    name       TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX volume_snapshots_volume_idx ON volume_snapshots (volume_id);
