-- Experience Reset v1: per-user clinician cockpit layout.
--
-- Additive, forward-only. Stores only presentation preferences (widget order,
-- hidden widgets, widget sizes and density) for one authenticated user inside
-- one tenant. No clinical data, patient identifiers or free text are stored
-- here; the layout document is validated by the API against the fixed widget
-- catalogue before it is written.

CREATE TABLE dashboard_preferences (
    tenant_id  uuid NOT NULL REFERENCES tenants(id),
    user_id    uuid NOT NULL REFERENCES users(id),
    layout     jsonb NOT NULL,
    version    integer NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (tenant_id, user_id)
);
