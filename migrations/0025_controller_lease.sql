-- The controller lease: which control plane runs the loops (design §48, ADR-021).
--
-- Exactly one row, ever. Its identity is a constant name rather than a UUID so
-- acquisition is a plain UPDATE against a known key — there is no election
-- protocol here, just a row that one instance holds at a time.
--
-- Deliberately not a session advisory lock. sqlx hands out arbitrary pooled
-- connections, so a session-scoped lock is held by whichever connection took it
-- rather than by the instance, and returning that connection to the pool makes
-- ownership unobservable. A row can be read with a plain SELECT by an operator
-- asking "who is the leader", which is the question they will actually have.
--
-- `epoch` increments on every acquisition. Nothing enforces it today; it exists
-- so the agent-side fencing token (ADR-021, deferred) has something monotonic to
-- carry, and so an operator can see how many times leadership has moved.

CREATE TABLE controller_lease (
    -- One row. The CHECK is what makes that true rather than conventional.
    name        TEXT PRIMARY KEY CHECK (name = 'controller'),
    holder      TEXT NOT NULL,
    epoch       BIGINT NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL,
    -- Renewed on a timer; a lease past this is free for the taking. Times come
    -- from PostgreSQL's clock, never an instance's, so instances that disagree
    -- about the time still agree about the lease.
    expires_at  TIMESTAMPTZ NOT NULL
);

-- Seeded already expired, so the first instance to start takes it immediately
-- rather than waiting out a TTL against a row that never existed.
INSERT INTO controller_lease (name, holder, epoch, acquired_at, expires_at)
VALUES ('controller', '', 0, now(), now() - interval '1 second');
