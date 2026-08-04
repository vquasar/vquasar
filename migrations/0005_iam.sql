-- Identity & RBAC (design M12b). Authentication is delegated to an external
-- OIDC provider; authorization (roles -> permissions over our resources) lives
-- here. Roles are global for now; the schema leaves room for project scoping.

CREATE TABLE users (
    id            UUID PRIMARY KEY,
    -- OIDC subject ("sub"); the stable external identity.
    subject       TEXT NOT NULL UNIQUE,
    username      TEXT NOT NULL,
    email         TEXT,
    display_name  TEXT,
    is_active     BOOLEAN NOT NULL DEFAULT TRUE,
    last_login    TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL
);

CREATE TABLE roles (
    id           UUID PRIMARY KEY,
    name         TEXT NOT NULL UNIQUE,
    description  TEXT,
    -- Built-in roles are seeded and cannot be deleted; their permissions are
    -- kept in sync from code on startup.
    builtin      BOOLEAN NOT NULL DEFAULT FALSE,
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL
);

-- A permission is a `resource:action` string from the code-defined catalog.
CREATE TABLE role_permissions (
    role_id      UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    permission   TEXT NOT NULL,
    PRIMARY KEY (role_id, permission)
);

CREATE TABLE user_roles (
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role_id      UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY (user_id, role_id)
);

-- Maps an OIDC group claim value to a role, so membership is managed in the IdP
-- while the role -> permission mapping stays here.
CREATE TABLE group_roles (
    "group"      TEXT NOT NULL,
    role_id      UUID NOT NULL REFERENCES roles(id) ON DELETE CASCADE,
    PRIMARY KEY ("group", role_id)
);
