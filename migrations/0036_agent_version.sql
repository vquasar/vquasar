-- Which build of vquasar-agent each host is running.
--
-- Observed, like the rest of the inventory: written from what the agent
-- reports on a reconcile tick, never declared at registration. A host row
-- created before its agent has ever answered has NULL here, and so does a host
-- whose agent predates the field — both mean "not known", which is the honest
-- answer and not the same as "matches the control plane".
ALTER TABLE hosts ADD COLUMN agent_version TEXT;
