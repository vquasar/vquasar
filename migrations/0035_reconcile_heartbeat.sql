-- When the leader last finished a reconcile pass.
--
-- A reconcile loop that has stopped is invisible: every VM stays as it was,
-- which looks exactly like a fleet with nothing to do. The lease says an
-- instance is alive — it is renewed by its own task — and says nothing about
-- whether the loop that instance is supposed to be running still turns.
ALTER TABLE controller_lease ADD COLUMN last_pass_at TIMESTAMPTZ;
