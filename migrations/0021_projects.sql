-- Projects: the unit of tenancy (design §47, ADR-018).
--
-- One migration for the whole ownership axis. Projects, quotas and per-tenant
-- isolation all want the same columns, and doing them separately would migrate
-- the same tables three times, each time rewriting the previous one's queries.
--
-- Nothing about this changes behaviour on its own. Every existing row is
-- assigned to a `default` project, the shareable catalogues stay shared, and
-- the behaviour is gated behind `[tenancy] enabled` — which is off.

CREATE TABLE projects (
    id          UUID PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE,
    description TEXT,
    -- The project every pre-tenancy row belongs to. Deletion-protected: it is
    -- the fallback for any caller without project context.
    is_default  BOOLEAN NOT NULL DEFAULT false,
    -- Reserved, unenforced. A hierarchy means recursive permission inheritance
    -- and recursive quota rollup, both load-bearing and both wrong to guess at
    -- now. Keeping the column costs nothing and keeps the option open; adding a
    -- level later is a migration on one table, removing a wrong one is not.
    parent_id   UUID REFERENCES projects(id),
    created_at  TIMESTAMPTZ NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL
);

-- A well-known id so the backfill below, and any operator query, can rely on it.
INSERT INTO projects (id, name, description, is_default, created_at, updated_at)
VALUES ('00000000-0000-0000-0000-000000000001', 'default',
        'Everything that existed before projects did.', true, now(), now());

CREATE UNIQUE INDEX projects_single_default_uidx ON projects (is_default) WHERE is_default;

-- Project-owned resources. The DEFAULT is kept deliberately: it makes the
-- backfill free, and it means an older binary that omits project_id on INSERT
-- still works — which is the rollback story for a single-node control plane
-- that applies migrations at startup.
ALTER TABLE virtual_machines ADD COLUMN project_id UUID NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES projects(id) ON DELETE RESTRICT;
ALTER TABLE volumes ADD COLUMN project_id UUID NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES projects(id) ON DELETE RESTRICT;
ALTER TABLE security_groups ADD COLUMN project_id UUID NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES projects(id) ON DELETE RESTRICT;
ALTER TABLE templates ADD COLUMN project_id UUID NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES projects(id) ON DELETE RESTRICT;
ALTER TABLE tasks ADD COLUMN project_id UUID NOT NULL
    DEFAULT '00000000-0000-0000-0000-000000000001' REFERENCES projects(id) ON DELETE RESTRICT;

-- Shareable catalogues. NULL means platform-shared and usable by every project.
-- Backfilling these into `default` instead would make the lab's images and its
-- provider network invisible the moment a second project existed — so they are
-- left alone, and NULL preserves exactly today's behaviour.
ALTER TABLE images ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE RESTRICT;
ALTER TABLE networks ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE RESTRICT;

-- Events describe both resources and the platform itself. A platform event
-- (host.ready) belongs to no project, so this one is nullable by nature.
ALTER TABLE events ADD COLUMN project_id UUID REFERENCES projects(id) ON DELETE SET NULL;

CREATE INDEX virtual_machines_project_idx ON virtual_machines (project_id, created_at);
CREATE INDEX volumes_project_idx          ON volumes (project_id);
CREATE INDEX security_groups_project_idx  ON security_groups (project_id);
CREATE INDEX templates_project_idx        ON templates (project_id);
CREATE INDEX tasks_project_idx            ON tasks (project_id, created_at);
CREATE INDEX events_project_idx           ON events (project_id, ts);
