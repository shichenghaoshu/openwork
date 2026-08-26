DROP TABLE IF EXISTS prompt_envelopes;

ALTER TABLE audit_events DROP CONSTRAINT audit_events_event_type_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_event_type_check CHECK (event_type IN (
    'run_created', 'runtime_selected', 'sandbox_created', 'action_requested',
    'policy_allowed', 'policy_denied', 'approval_requested', 'approval_approved',
    'approval_denied', 'approval_expired', 'approval_consumed', 'action_executed',
    'runtime_started', 'runtime_output', 'artifact_created', 'runtime_completed',
    'sandbox_destroyed', 'run_completed', 'run_failed', 'approval_binding_mismatch',
    'cancel_requested', 'cancel_confirmed'
));
