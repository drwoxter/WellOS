-- Phase 1 identity and authorization hardening.

-- Stable OIDC subject mapping for human identities. Tenant, roles, and
-- permissions are always resolved locally, never from client claims.
ALTER TABLE users ADD COLUMN oidc_subject text UNIQUE;

-- Machine principals authenticate with random high-entropy bearer secrets.
-- Only a one-way hash is stored; the plaintext exists only at issuance.
CREATE TABLE service_credentials (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    user_id      uuid NOT NULL REFERENCES users(id),
    name         text NOT NULL,
    token_hash   text NOT NULL UNIQUE,
    scopes       text[] NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz,
    revoked_at   timestamptz,
    last_used_at timestamptz
);

-- Break-glass events become immutable review-workflow records.
ALTER TABLE break_glass_events
    ADD COLUMN correlation_id uuid,
    ADD COLUMN purpose_of_use text,
    ADD COLUMN review_status text NOT NULL DEFAULT 'pending',
    ADD COLUMN reviewed_by uuid REFERENCES users(id),
    ADD COLUMN reviewed_at timestamptz,
    ADD COLUMN review_note text;
ALTER TABLE break_glass_events DROP COLUMN reviewed;
CREATE INDEX break_glass_events_user_created_idx
    ON break_glass_events (user_id, created_at);
