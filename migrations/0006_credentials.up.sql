-- Seed and status boundary for operator-supplied credentials.
--
-- Only encrypted values live here. No column stores a raw secret, and the
-- value column is never selected by status/observability queries. All
-- credential reads go through the reviewed SecretStore boundary.

CREATE TABLE credentials (
    credential_ref TEXT PRIMARY KEY CHECK (credential_ref ~ '^[a-z][a-z0-9._-]{0,127}$'),
    provider TEXT NOT NULL CHECK (provider ~ '^[a-z][a-z0-9._-]{1,128}$'),
    environment_variable TEXT NOT NULL CHECK (environment_variable ~ '^[A-Za-z_][A-Za-z0-9_]{0,127}$'),
    scope TEXT NOT NULL CHECK (scope ~ '^[a-z0-9._:/+=+-]{1,512}$'),
    owner TEXT NOT NULL CHECK (owner ~ '^[a-z0-9._@:/-]{1,128}$'),
    encrypted_value BYTEA NOT NULL,
    key_id TEXT NOT NULL CHECK (key_id ~ '^[a-z][a-z0-9._-]{0,127}$'),
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ,
    disabled_at TIMESTAMPTZ,
    revision BIGINT NOT NULL DEFAULT 0 CHECK (revision >= 0),
    CHECK (created_at <= updated_at)
);

CREATE INDEX credentials_status_idx ON credentials (disabled_at, expires_at);
