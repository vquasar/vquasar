-- Per-VM secret for the cloud-init phone_home callback (design M13e).
--
-- The endpoint was unauthenticated: anyone who could reach the API and knew a
-- VM's id could set that VM's recorded address, and VM ids are not secret —
-- the task and event streams hand them out. The guest cannot hold an operator
-- credential, so it gets a secret of its own, injected into its seed and known
-- only to the control plane and that guest.
ALTER TABLE virtual_machines ADD COLUMN phone_home_token TEXT;
