-- Per-project role bindings (design §47, ADR-018/020).
--
-- A binding gains the project it applies in. NULL keeps exactly today's
-- meaning — a platform-wide grant — so every existing row, the first-admin
-- bootstrap and the OIDC group mapping are unchanged by this migration.
--
-- This is what makes tenancy enforceable. Until now the request's project came
-- from a header and was believed: any caller holding a global permission could
-- name any project and act in it. With bindings scoped, a caller's permissions
-- are resolved *in* the requested project, and a caller with no binding there
-- resolves to the empty set — which fails every guard without needing a
-- separate membership check that could be forgotten.

ALTER TABLE user_roles  ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE CASCADE;
ALTER TABLE group_roles ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE CASCADE;

-- The same (user, role) pair can now be granted platform-wide *and* in a
-- project, so project_id joins the key. NULLS NOT DISTINCT is what keeps the
-- platform-wide row unique: without it PostgreSQL treats every NULL as
-- different and the same platform grant could be inserted repeatedly.
ALTER TABLE user_roles  DROP CONSTRAINT user_roles_pkey;
ALTER TABLE group_roles DROP CONSTRAINT group_roles_pkey;
CREATE UNIQUE INDEX user_roles_binding_uidx
    ON user_roles (user_id, role_id, project_id) NULLS NOT DISTINCT;
CREATE UNIQUE INDEX group_roles_binding_uidx
    ON group_roles ("group", role_id, project_id) NULLS NOT DISTINCT;

-- Permission resolution filters on the project every request, so index it.
CREATE INDEX user_roles_project_idx  ON user_roles (project_id);
CREATE INDEX group_roles_project_idx ON group_roles (project_id);
