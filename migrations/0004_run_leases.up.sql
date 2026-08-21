-- Worker ownership is database-mediated. Expired leases fail closed; they are
-- never silently requeued because the original worker may have side effects.
ALTER TABLE runs ADD COLUMN cancel_requested_at TIMESTAMPTZ;
ALTER TABLE runs ADD CONSTRAINT runs_cancel_requested_at_check CHECK (
    cancel_requested_at IS NULL OR cancel_requested_at >= created_at
);

ALTER TABLE audit_events DROP CONSTRAINT audit_events_event_type_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_event_type_check CHECK (event_type IN (
    'run_created', 'runtime_selected', 'sandbox_created', 'action_requested',
    'policy_allowed', 'policy_denied', 'approval_requested', 'approval_approved',
    'approval_denied', 'approval_expired', 'approval_consumed', 'action_executed',
    'runtime_started', 'runtime_output', 'artifact_created', 'runtime_completed',
    'sandbox_destroyed', 'run_completed', 'run_failed', 'approval_binding_mismatch',
    'cancel_requested', 'cancel_confirmed'
));

CREATE TABLE run_leases (
    run_id UUID PRIMARY KEY REFERENCES runs(id) ON DELETE RESTRICT,
    lease_token UUID NOT NULL UNIQUE,
    owner_id TEXT NOT NULL CHECK (length(owner_id) BETWEEN 1 AND 256 AND btrim(owner_id) <> ''),
    acquired_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK (expires_at > acquired_at)
);

CREATE INDEX run_leases_expiry_idx ON run_leases (expires_at);
