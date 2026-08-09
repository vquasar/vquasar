-- More than one pool kind (ADR-023), and the uniqueness that has to survive it.
--
-- 0028 made "one directory is one pool" true for `shared_dir` with a partial
-- index on `params->>'path'`. With `nfs` naming its mount point in a different
-- field, that index stops covering the invariant — and worse, it stops covering
-- it *silently*: a shared_dir at /mnt/vms and an nfs pool mounted at /mnt/vms
-- would both be accepted, double-counting one filesystem and splitting its
-- volumes across two namespaces.
--
-- So the index moves off the field and onto the thing it was always about: the
-- pool's host path, whichever key the kind stores it under. A future kind with
-- no host path stores NULL, and NULLs do not collide — which is right, because
-- two RBD pools on one cluster are two different pools.

DROP INDEX storage_pools_shared_dir_path_uidx;

CREATE UNIQUE INDEX storage_pools_host_path_uidx
    ON storage_pools ((COALESCE(params->>'path', params->>'mount_point')));

-- And the export itself: two pools mounting one export at two paths are the
-- same bytes twice, for the same reason and with the same consequences.
CREATE UNIQUE INDEX storage_pools_nfs_export_uidx
    ON storage_pools ((params->>'server'), (params->>'export'))
    WHERE kind = 'nfs';
