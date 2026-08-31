-- Track when an alert left the 'open' state so the worklist can compute a
-- snapshot-stable priority: whether an alert was open as of a given instant
-- is derivable from (created_at, closed_at) regardless of later mutations.
ALTER TABLE alerts ADD COLUMN closed_at timestamptz;
UPDATE alerts SET closed_at = now() WHERE status <> 'open';
