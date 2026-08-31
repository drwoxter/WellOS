-- Bind an in-flight browser login to the browser that started it.
--
-- The BFF holds a one-time binding secret in a short-lived HttpOnly cookie
-- and presents it at the callback; only its hash is stored here. Without
-- this, a completed callback URL could be planted in another user's browser
-- and replace that browser's WellOS session (login CSRF / session swapping).
--
-- In-flight transactions are ephemeral (minutes) and worthless once the
-- state is unusable, so pending rows are discarded rather than backfilled.
DELETE FROM login_transactions;

ALTER TABLE login_transactions ADD COLUMN binding_hash text NOT NULL;
