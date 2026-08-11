# Postgres approval repository integration note

This is a migration and transaction suggestion, not a Postgres implementation
or runtime-validation claim. The infrastructure owner must reconcile it with the
canonical migrations before enabling server mutations.

## Required storage invariants

`approval_requests` must persist every frozen `ApprovalRequest` field, including
`revision`, exact `run_id`/`action_id`/`parameter_hash`, decision actor and time,
and `consumed_at`. Recommended checks are:

- revision is non-negative;
- expiry is after creation and no more than 24 hours later;
- pending has no decision or consumption time;
- approved has an approved decision and no consumption time;
- denied has a denied decision and no consumption time;
- consumed has an approved decision and a consumption time before expiry;
- one live pending/approved request exists per exact action binding.

The action claim is separate durable proof that consumption authorized exactly
one execution:

```sql
CREATE TABLE action_claims (
    approval_id UUID PRIMARY KEY REFERENCES approval_requests(id) ON DELETE RESTRICT,
    run_id UUID NOT NULL REFERENCES runs(id) ON DELETE RESTRICT,
    action_id UUID NOT NULL UNIQUE,
    parameter_hash CHAR(64) NOT NULL CHECK (parameter_hash ~ '^[0-9a-f]{64}$'),
    actor_id TEXT NOT NULL CHECK (length(actor_id) BETWEEN 1 AND 256),
    claimed_at TIMESTAMPTZ NOT NULL
);
```

## Decision and expiry transaction

1. Begin a transaction.
2. Select the approval row `FOR UPDATE` by ID.
3. Validate `revision = expected_revision`, the current state, and the trusted
   server-clock value. `trusted_now >= expires_at` is expired.
4. Update status, decision fields, and `revision = revision + 1` with the old
   revision in the `WHERE` clause.
5. Lock the run's audit tail, allocate the next sequence, construct the canonical
   centrally-redacted `AuditEvent`, and insert it.
6. Commit. Any zero-row CAS or audit failure rolls the entire transaction back.

The authenticated handler supplies the decision actor. SQL must never select an
actor from a JSON request document.

## Consumption transaction

1. Begin a transaction and lock the approval row `FOR UPDATE`.
2. Recompute and compare the exact run/action/binding hash against the action
   being claimed; reject stale revision, replay, or `trusted_now >= expires_at`.
3. Insert `action_claims` first under its unique keys.
4. CAS-update approved to consumed, set `consumed_at`, and increment revision.
5. Append the typed/redacted audit event under the same locked audit tail.
6. Commit.

Unique `approval_id` and `action_id` constraints are the final replay defense;
the application mutex used by `InMemoryExecutionStore` is not a substitute for
these database constraints.

## Audit mapping

Persist machine-only `status` metadata and use the exact event type:

- requested to `approval_requested`;
- approved to `approval_approved`;
- consumed to `approval_consumed`;
- denied to `approval_denied`;
- expired to `approval_expired`;
- exact-binding mismatch to `approval_binding_mismatch`.

Never store decision reasons, action parameters, raw tool payloads, credentials,
or authorization headers in audit metadata.
