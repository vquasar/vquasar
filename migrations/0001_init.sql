-- ch-orchestrator control-plane schema (design document, section 31).
-- Explicit tables; no ORM. State transitions run inside transactions and use
-- the generation column for optimistic concurrency.

CREATE TABLE hosts (
    id                        UUID PRIMARY KEY,
    name                      TEXT NOT NULL,
    -- Agent gRPC endpoint the control plane dials (e.g. http://10.0.0.11:9500).
    endpoint                  TEXT NOT NULL,
    schedulable               BOOLEAN NOT NULL DEFAULT TRUE,
    state                     TEXT NOT NULL DEFAULT 'NotReady',
    hostname                  TEXT,
    architecture              TEXT,
    kernel_version            TEXT,
    cloud_hypervisor_version  TEXT,
    logical_cpus              INTEGER,
    cpu_model                 TEXT,
    total_memory_bytes        BIGINT,
    available_memory_bytes    BIGINT,
    vm_count                  INTEGER NOT NULL DEFAULT 0,
    last_heartbeat            TIMESTAMPTZ,
    created_at                TIMESTAMPTZ NOT NULL,
    updated_at                TIMESTAMPTZ NOT NULL,
    generation                BIGINT NOT NULL DEFAULT 1
);

CREATE TABLE virtual_machines (
    id                    UUID PRIMARY KEY,
    name                  TEXT NOT NULL,
    -- Desired state: the orchestration VirtualMachineSpec (ADR-013 keeps this
    -- independent of Cloud Hypervisor's own config format).
    spec                  JSONB NOT NULL,
    -- Observed state.
    phase                 TEXT NOT NULL DEFAULT 'Pending',
    host_id               UUID REFERENCES hosts(id) ON DELETE SET NULL,
    observed_generation   BIGINT NOT NULL DEFAULT 0,
    message               TEXT,
    ip_address            TEXT,
    created_at            TIMESTAMPTZ NOT NULL,
    updated_at            TIMESTAMPTZ NOT NULL,
    generation            BIGINT NOT NULL DEFAULT 1
);

CREATE INDEX virtual_machines_host_id_idx ON virtual_machines(host_id);
CREATE INDEX virtual_machines_phase_idx ON virtual_machines(phase);

CREATE TABLE tasks (
    id          UUID PRIMARY KEY,
    task_type   TEXT NOT NULL,
    state       TEXT NOT NULL DEFAULT 'Pending',
    progress    INTEGER NOT NULL DEFAULT 0,
    vm_id       UUID,
    message     TEXT,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

CREATE INDEX tasks_state_idx ON tasks(state);

CREATE TABLE events (
    id             UUID PRIMARY KEY,
    ts             TIMESTAMPTZ NOT NULL,
    resource_type  TEXT NOT NULL,
    resource_id    UUID,
    event_type     TEXT NOT NULL,
    severity       TEXT NOT NULL DEFAULT 'info',
    message        TEXT NOT NULL,
    metadata       JSONB
);

CREATE INDEX events_ts_idx ON events(ts DESC);
