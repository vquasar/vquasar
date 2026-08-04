-- First-class volumes (design M14a): block devices managed independently of any
-- VM. A volume is provisioned as a disk image on shared storage and can be
-- attached to / detached from a VM; it survives VM deletion.

CREATE TABLE volumes (
    id              UUID PRIMARY KEY,
    name            TEXT NOT NULL,
    size_bytes      BIGINT NOT NULL,
    format          TEXT NOT NULL DEFAULT 'qcow2',   -- raw | qcow2
    attached_vm_id  UUID,                            -- NULL ⇒ available
    attached_serial INTEGER,                         -- disk index on the VM
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL
);

CREATE INDEX volumes_attached_vm_idx ON volumes (attached_vm_id);
