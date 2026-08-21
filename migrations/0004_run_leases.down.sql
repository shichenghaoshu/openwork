-- Preconditions: operators must first verify there are no cancel audit events
-- and no active leases. Refuse rollback rather than erase audit semantics.
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM audit_events WHERE event_type IN ('cancel_requested', 'cancel_confirmed'))
       OR EXISTS (SELECT 1 FROM runs WHERE cancel_requested_at IS NOT NULL)
       OR EXISTS (SELECT 1 FROM run_leases) THEN
        RAISE EXCEPTION 'cannot roll back 0004 while cancellation evidence or leases exist';
    END IF;
END $$;
ALTER TABLE audit_events DROP CONSTRAINT audit_events_event_type_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_event_type_check CHECK (event_type IN (
    'run_created', 'runtime_selected', 'sandbox_created', 'action_requested',
    'policy_allowed', 'policy_denied', 'approval_requested', 'approval_approved',
    'approval_denied', 'approval_expired', 'approval_consumed', 'action_executed',
    'runtime_started', 'runtime_output', 'artifact_created', 'runtime_completed',
    'sandbox_destroyed', 'run_completed', 'run_failed', 'approval_binding_mismatch'
));
DROP TABLE run_leases;
ALTER TABLE runs DROP COLUMN cancel_requested_at;
