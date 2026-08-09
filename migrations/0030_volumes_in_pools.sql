-- A volume belongs to exactly one storage pool (design §20, ADR-023).
--
-- Nullable, and grandfathered rather than backfilled with a constant: the
-- `default` pool's id is generated the first time a control plane starts, so
-- this migration cannot name it. Existing rows are adopted here when that pool
-- is already present, and startup adopts whatever is left — including on a
-- cluster jumping straight from pre-pool to here, where the pool row does not
-- exist yet when this runs.
--
-- Same shape as the grandfathered `networks.segment_key` (ADR-016): NULL means
-- "predates the model", every new row gets a value, and nothing has to invent
-- one to make the schema happy.
--
-- ON DELETE RESTRICT: a pool holding volumes cannot be deleted out from under
-- them. Losing the record of where bytes are is worse than a refusal.

ALTER TABLE volumes
    ADD COLUMN pool_id UUID REFERENCES storage_pools(id) ON DELETE RESTRICT;

UPDATE volumes
   SET pool_id = (SELECT id FROM storage_pools WHERE name = 'default' LIMIT 1)
 WHERE pool_id IS NULL;

CREATE INDEX volumes_pool_idx ON volumes (pool_id);
