-- Storage pools (design §20, ADR-023): a named place to put bytes, and a
-- record of which hosts can actually reach it.
--
-- Both halves are here because they are one resource. `storage_pools` is
-- desired state — an operator says a pool exists and what kind it is.
-- `storage_pool_reachability` is observed state (§7): a row means *this host
-- reported it can use this pool*, and its absence means nothing has said so.
-- Reachability is never written by an operator, because a declared mount is an
-- intention the filesystem is free to contradict — which is the failure this
-- exists to remove.

CREATE TABLE storage_pools (
    id          UUID PRIMARY KEY,
    -- Operators name pools and volumes will refer to them by name, so one name
    -- must mean one pool.
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    -- shared_dir today; lvm_thin | nfs | rbd are the shapes it is built for.
    kind        TEXT NOT NULL,
    -- Kind-specific parameters, internally tagged (see model::storage). The
    -- CHECK is what makes the duplication safe: the column exists for queries
    -- and indexes, the blob names its own shape, and they cannot drift.
    params      JSONB NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL,
    CONSTRAINT storage_pools_kind_matches_params CHECK (params->>'kind' = kind)
);

-- Two pools over one directory would double-count its capacity and split its
-- volumes across two namespaces for no gain. Same move as networks.segment_key:
-- make the collision impossible rather than documenting it.
CREATE UNIQUE INDEX storage_pools_shared_dir_path_uidx
    ON storage_pools ((params->>'path')) WHERE kind = 'shared_dir';

-- One row per (pool, host) that the host currently reports. Capacity is
-- observed for the same reason reachability is: a number an operator typed is
-- a number that goes stale.
CREATE TABLE storage_pool_reachability (
    pool_id         UUID NOT NULL REFERENCES storage_pools(id) ON DELETE CASCADE,
    host_id         UUID NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    capacity_bytes  BIGINT,
    available_bytes BIGINT,
    reported_at     TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (pool_id, host_id)
);

CREATE INDEX storage_pool_reachability_host_idx ON storage_pool_reachability (host_id);
