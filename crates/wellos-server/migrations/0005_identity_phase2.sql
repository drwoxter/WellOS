-- Phase 2 identity hardening: provider-aware identity mapping and
-- server-side browser sessions.

-- Provider-aware identity mapping: (issuer, subject) -> local user. The
-- legacy single-provider users.oidc_subject column is kept for a
-- backward-compatible transition; matched legacy subjects are migrated into
-- this table lazily at authentication time using the configured issuer.
CREATE TABLE user_identities (
    id         uuid PRIMARY KEY,
    user_id    uuid NOT NULL REFERENCES users(id),
    issuer     text NOT NULL,
    subject    text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (issuer, subject)
);
CREATE INDEX user_identities_user_idx ON user_identities (user_id);

-- Server-side browser sessions. The cookie holds only an opaque random
-- identifier; the database stores a one-way hash of it plus a hashed CSRF
-- token, absolute expiration, inactivity tracking, and revocation.
CREATE TABLE web_sessions (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    user_id      uuid NOT NULL REFERENCES users(id),
    token_hash   text NOT NULL UNIQUE,
    csrf_hash    text NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz NOT NULL,
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    revoked_at   timestamptz
);
CREATE INDEX web_sessions_user_idx ON web_sessions (user_id);
