-- Which host holds a volume whose pool is local to one machine (ADR-025).
--
-- NULL means the volume is on shared storage and no single host owns it, which
-- is every volume that existed before local pools did. Not a default: "on
-- shared storage" and "on host X" are different facts, and only one of them
-- needs a host named.
--
-- ON DELETE RESTRICT: a host holding volumes cannot be deleted out from under
-- them. The volume's bytes are on that machine, and forgetting which one is
-- worse than a refusal.
ALTER TABLE volumes
    ADD COLUMN host_id UUID REFERENCES hosts(id) ON DELETE RESTRICT;

CREATE INDEX volumes_host_idx ON volumes (host_id) WHERE host_id IS NOT NULL;
