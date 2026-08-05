-- Agent auto-enrollment (design M16): one-time, TTL'd bootstrap tokens. An
-- operator enrolls a host (creating the row + a token); the new agent presents
-- the token to have its CSR signed by the intermediate CA. Only the SHA-256
-- hash of the token is stored; it is single-use (used_at) and expires.
CREATE TABLE enrollment_tokens (
    id         UUID PRIMARY KEY,
    host_id    UUID NOT NULL REFERENCES hosts(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    expires_at TIMESTAMPTZ NOT NULL,
    used_at    TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX enrollment_tokens_host_idx ON enrollment_tokens(host_id);
