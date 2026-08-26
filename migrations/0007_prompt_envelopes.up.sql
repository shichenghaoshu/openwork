-- Durable encrypted prompt recovery.
--
-- Prompt plaintext never touches this table. The envelope binds the run, the
-- prompt digest, the sealing key, an expiry window, and a one-time consume
-- state. Only the trusted worker boundary decrypts, immediately before stdin
-- delivery, and successful single-use delivery removes the envelope.

CREATE TABLE prompt_envelopes (
    run_id UUID PRIMARY KEY REFERENCES runs(id) ON DELETE RESTRICT,
    prompt_sha256 CHAR(64) NOT NULL CHECK (prompt_sha256 ~ '^[0-9a-f]{64}$'),
    key_id TEXT NOT NULL CHECK (key_id ~ '^[a-z][a-z0-9._-]{0,127}$'),
    nonce BYTEA NOT NULL CHECK (octet_length(nonce) = 12),
    ciphertext BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK (expires_at > created_at)
);

CREATE INDEX prompt_envelopes_expiry_idx ON prompt_envelopes (expires_at);

ALTER TABLE audit_events DROP CONSTRAINT audit_events_event_type_check;
ALTER TABLE audit_events ADD CONSTRAINT audit_events_event_type_check CHECK (event_type IN (
    'run_created', 'runtime_selected', 'sandbox_created', 'action_requested',
    'policy_allowed', 'policy_denied', 'approval_requested', 'approval_approved',
    'approval_denied', 'approval_expired', 'approval_consumed', 'action_executed',
    'runtime_started', 'runtime_output', 'artifact_created', 'runtime_completed',
    'sandbox_destroyed', 'run_completed', 'run_failed', 'approval_binding_mismatch',
    'cancel_requested', 'cancel_confirmed', 'prompt_envelope_stored',
    'prompt_envelope_consumed', 'prompt_envelope_expired',
    'prompt_envelope_corrupt'
));
