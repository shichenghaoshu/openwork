CREATE TABLE capability_actions (
    action_id UUID PRIMARY KEY,
    approval_id UUID NOT NULL UNIQUE REFERENCES approval_requests(id) ON DELETE RESTRICT,
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
    app_id TEXT NOT NULL CHECK (length(app_id) BETWEEN 1 AND 128),
    capability_id TEXT NOT NULL CHECK (length(capability_id) BETWEEN 1 AND 128),
    resource TEXT NOT NULL CHECK (length(resource) BETWEEN 1 AND 2048),
    parameters JSONB NOT NULL CHECK (jsonb_typeof(parameters) = 'object'),
    parameter_hash CHAR(64) NOT NULL CHECK (parameter_hash ~ '^[0-9a-f]{64}$'),
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 256),
    created_at TIMESTAMPTZ NOT NULL,
    UNIQUE (run_id, action_id, parameter_hash)
);

CREATE TABLE capability_action_executions (
    action_id UUID PRIMARY KEY REFERENCES capability_actions(action_id) ON DELETE RESTRICT,
    parameter_hash CHAR(64) NOT NULL CHECK (parameter_hash ~ '^[0-9a-f]{64}$'),
    state TEXT NOT NULL CHECK (state IN ('started', 'performed', 'uncertain')),
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    result_sha256 CHAR(64) CHECK (result_sha256 ~ '^[0-9a-f]{64}$'),
    CHECK (
        (state = 'started' AND completed_at IS NULL AND result_sha256 IS NULL)
        OR (state = 'performed' AND completed_at IS NOT NULL AND result_sha256 IS NOT NULL)
        OR (state = 'uncertain' AND completed_at IS NOT NULL)
    )
);
