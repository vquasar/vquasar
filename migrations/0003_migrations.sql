-- Live-migration state machine (design document, section 28).
-- A migration is a persisted record advanced one step per reconcile tick, so it
-- survives a control-plane restart rather than living inside one RPC handler.

CREATE TABLE migrations (
    id              UUID PRIMARY KEY,
    vm_id           UUID NOT NULL,
    source_host_id  UUID,
    target_host_id  UUID NOT NULL,
    -- Pending -> Sending -> Finalizing -> Completed | Failed
    state           TEXT NOT NULL DEFAULT 'Pending',
    migration_url   TEXT,
    task_id         UUID,
    message         TEXT,
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL
);

CREATE INDEX migrations_state_idx ON migrations(state);
CREATE INDEX migrations_vm_id_idx ON migrations(vm_id);
