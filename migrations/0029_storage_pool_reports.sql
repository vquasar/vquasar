-- What a host reports about a pool, including "I cannot use this, and here is
-- why" (design §20, ADR-023).
--
-- 0028 encoded reachability as presence: a row meant a host could use the pool.
-- That is a fine invariant and a poor operator experience — the question after
-- a placement refusal is never *whether* a host can see the storage, it is
-- *why not*. So an observation is now always recorded, and `usable` says which
-- kind it is. Everything that asks "is this pool reachable" filters on it.
--
-- The default is `true` so the rows 0028 could already hold keep their old
-- meaning.

ALTER TABLE storage_pool_reachability
    ADD COLUMN usable  BOOLEAN NOT NULL DEFAULT true,
    -- Why not, in the agent's words. NULL when usable.
    ADD COLUMN message TEXT;
