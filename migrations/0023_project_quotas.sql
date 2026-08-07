-- Per-project quotas (design §47, ADR-019).
--
-- One row per project that has limits. No row means no quota, which is what
-- every existing project gets: this migration cannot make a running cluster
-- start refusing work.
--
-- A NULL column is "unlimited in that dimension", so a project can be capped on
-- memory without also having to declare a VM count it does not care about.
--
-- There are deliberately no usage counters here. Usage is aggregated from the
-- owning tables inside the admission transaction (ADR-019): a stored counter is
-- a second source of truth that drifts on any crash between the insert and the
-- increment, and needs a repair pass nobody remembers to run.

CREATE TABLE project_quotas (
    project_id        UUID PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    max_vms           INTEGER,
    max_vcpus         INTEGER,
    max_memory_mib    BIGINT,
    max_volumes       INTEGER,
    max_storage_bytes BIGINT,
    created_at        TIMESTAMPTZ NOT NULL,
    updated_at        TIMESTAMPTZ NOT NULL,

    -- A negative limit is not a limit, and 0 is a meaningful one (freeze the
    -- project). Catching it here means an operator typo cannot become a quota
    -- that silently never applies.
    CONSTRAINT project_quotas_non_negative CHECK (
        COALESCE(max_vms, 0) >= 0 AND COALESCE(max_vcpus, 0) >= 0
        AND COALESCE(max_memory_mib, 0) >= 0 AND COALESCE(max_volumes, 0) >= 0
        AND COALESCE(max_storage_bytes, 0) >= 0
    )
);

-- A volume is provisioned by expensive external work (qemu-img convert on
-- shared storage) that used to run *before* anything was persisted. Under a
-- quota that ordering is unusable: the work would happen and only then be
-- refused, and two concurrent creates would both do the work before either was
-- counted. So the row is now committed inside the admission transaction and
-- the file is built afterwards, which needs a state to sit in meanwhile
-- (ADR-019).
--
-- Existing volumes are 'ready': they have their file already.
ALTER TABLE volumes ADD COLUMN status TEXT NOT NULL DEFAULT 'ready';
