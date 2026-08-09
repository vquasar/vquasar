-- The two halves of per-tenant network policy that were still missing
-- (design §18, ADR-017 follow-ups).
--
-- A rule may name a *group* as its remote instead of a CIDR. The control plane
-- expands it to the addresses of that group's members every reconcile tick, so
-- "the web tier may reach the database tier" survives a VM being replaced —
-- which is the thing a CIDR cannot express without an operator maintaining it
-- by hand.
--
-- Exactly one of remote_cidr / remote_group_id may be set. Both would be two
-- different answers to "who is the remote", and the rule would silently mean
-- whichever one the resolver happened to read.
ALTER TABLE security_group_rules
    ADD COLUMN remote_group_id UUID REFERENCES security_groups(id) ON DELETE CASCADE,
    ADD CONSTRAINT security_group_rules_one_remote
        CHECK (remote_cidr IS NULL OR remote_group_id IS NULL);

CREATE INDEX security_group_rules_remote_group_idx
    ON security_group_rules (remote_group_id) WHERE remote_group_id IS NOT NULL;

-- And a project's own default group, unioned into every NIC its VMs have.
--
-- A network's default (ADR-017) is the right tool for a tenant network, which
-- belongs to one project. It is the wrong one for a provider or VLAN network,
-- which is platform-shared: a rule an operator adds there applies to every
-- tenant on it. This is where a tenant's baseline goes instead.
--
-- Nullable: a project created before this column keeps working, and startup
-- gives it one.
ALTER TABLE projects ADD COLUMN default_security_group_id UUID REFERENCES security_groups(id);
