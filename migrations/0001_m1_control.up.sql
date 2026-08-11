CREATE TYPE run_status AS ENUM (
    'queued', 'planning', 'awaiting_approval', 'running',
    'succeeded', 'failed', 'cancelled', 'timed_out'
);

CREATE TYPE approval_status AS ENUM (
    'pending', 'approved', 'denied', 'expired', 'consumed'
);

CREATE TABLE runs (
    id UUID PRIMARY KEY,
    runtime TEXT NOT NULL CHECK (length(runtime) BETWEEN 1 AND 128),
    workspace TEXT NOT NULL CHECK (
        length(workspace) BETWEEN 1 AND 256
        AND workspace ~ '^[A-Za-z0-9._/-]+$'
        AND workspace !~ '^/' AND workspace NOT LIKE '%//%' AND right(workspace, 1) <> '/'
        AND workspace !~ '(^|/)\\.{1,2}(/|$)'
    ),
    status run_status NOT NULL DEFAULT 'queued',
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 256 AND btrim(actor_id) <> ''),
    prompt_sha256 CHAR(64) NOT NULL CHECK (prompt_sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    terminal_reason TEXT,
    CHECK (updated_at >= created_at),
    CHECK (started_at IS NULL OR started_at >= created_at),
    CHECK (completed_at IS NULL OR completed_at >= created_at)
);

CREATE TABLE artifacts (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
    path TEXT NOT NULL CHECK (
        path !~ '^/' AND position(chr(92) IN path) = 0
        AND path NOT LIKE '%//%' AND right(path, 1) <> '/'
        AND path !~ '(^|/)\\.{1,2}(/|$)'
    ),
    media_type TEXT NOT NULL CHECK (length(media_type) > 0),
    size_bytes BIGINT NOT NULL CHECK (size_bytes BETWEEN 0 AND 104857600),
    sha256 CHAR(64) NOT NULL CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (run_id, path)
);

CREATE TABLE audit_events (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
    sequence BIGINT NOT NULL CHECK (sequence > 0),
    event_type TEXT NOT NULL CHECK (event_type IN (
        'run_created', 'runtime_selected', 'sandbox_created', 'action_requested',
        'policy_allowed', 'policy_denied', 'approval_requested', 'approval_approved',
        'approval_denied', 'runtime_started', 'runtime_output', 'artifact_created',
        'runtime_completed', 'sandbox_destroyed', 'run_completed', 'run_failed',
        'approval_binding_mismatch'
    )),
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 256 AND btrim(actor_id) <> ''),
    occurred_at TIMESTAMPTZ NOT NULL,
    redacted_metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    previous_hash CHAR(64) CHECK (previous_hash ~ '^[0-9a-f]{64}$'),
    event_hash CHAR(64) NOT NULL CHECK (event_hash ~ '^[0-9a-f]{64}$'),
    UNIQUE (run_id, sequence),
    CHECK ((sequence = 1 AND previous_hash IS NULL) OR (sequence > 1 AND previous_hash IS NOT NULL))
);

CREATE TABLE approval_requests (
    id UUID PRIMARY KEY,
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
    action_id UUID NOT NULL,
    parameter_hash CHAR(64) NOT NULL CHECK (parameter_hash ~ '^[0-9a-f]{64}$'),
    requested_by TEXT NOT NULL CHECK (length(requested_by) BETWEEN 1 AND 256 AND btrim(requested_by) <> ''),
    request_reason TEXT NOT NULL CHECK (length(request_reason) > 0),
    created_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    status approval_status NOT NULL DEFAULT 'pending',
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    consumed_at TIMESTAMPTZ,
    UNIQUE (run_id, action_id, parameter_hash),
    CHECK (expires_at > created_at AND expires_at <= created_at + INTERVAL '24 hours'),
    CHECK ((status = 'consumed' AND consumed_at IS NOT NULL) OR (status <> 'consumed' AND consumed_at IS NULL))
);

CREATE TABLE approval_decisions (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    approval_id UUID NOT NULL REFERENCES approval_requests(id) ON DELETE RESTRICT,
    decision TEXT NOT NULL CHECK (decision IN ('approved', 'denied')),
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 256 AND btrim(actor_id) <> ''),
    reason TEXT,
    decided_at TIMESTAMPTZ NOT NULL,
    approval_revision BIGINT NOT NULL CHECK (approval_revision > 0),
    UNIQUE (approval_id)
);

CREATE INDEX runs_status_created_idx ON runs (status, created_at);
CREATE INDEX artifacts_run_idx ON artifacts (run_id, created_at);
CREATE INDEX audit_events_run_idx ON audit_events (run_id, sequence);
CREATE INDEX approvals_pending_idx ON approval_requests (status, expires_at);
