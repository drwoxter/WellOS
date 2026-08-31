-- Phase 3A: browser OIDC Authorization Code + PKCE login transactions and
-- the shared PostgreSQL-backed rate limiter.

-- Server-side login transactions for the Authorization Code + PKCE flow.
-- The browser only ever sees the opaque `state` value (its hash is stored
-- here); the nonce hash and PKCE code verifier never leave the server.
-- Transactions are single-use (claimed atomically via used_at) and
-- short-lived (expires_at, at most minutes).
CREATE TABLE login_transactions (
    id            uuid PRIMARY KEY,
    state_hash    text NOT NULL UNIQUE,
    nonce_hash    text NOT NULL,
    code_verifier text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now(),
    expires_at    timestamptz NOT NULL,
    used_at       timestamptz
);

-- Shared fixed-window rate limiter. Rows are upserted atomically
-- (INSERT ... ON CONFLICT ... count + 1), so concurrent requests cannot
-- bypass a limit, and the store works across API replicas. Keys never
-- contain raw client addresses or credentials (only one-way hashes).
CREATE TABLE rate_limit_windows (
    key          text NOT NULL,
    window_start timestamptz NOT NULL,
    count        bigint NOT NULL,
    PRIMARY KEY (key, window_start)
);
