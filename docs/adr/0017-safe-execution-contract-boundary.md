# Safe execution contract boundary

Status: Accepted

## Context

M1 adds independently implemented control, sandbox, execution, policy, approval,
audit, artifact, and runtime-execution modules. Parallel work without one frozen
contract would create circular dependencies and inconsistent security semantics.

## Decision

`openwork-execution` owns versioned, provider-neutral data contracts and only the
small pure helpers needed to bind parameters and prompts to SHA-256 digests. The
contract distinguishes Run, Artifact, AuditEvent, SandboxRequest/SandboxResult,
ActionRequest/PolicyDecision, ApprovalRequest, and RuntimeTask/RuntimeEvent.

Implementations depend inward on these contracts. The contract crate does not
depend on Docker, HTTP, databases, a provider adapter, or an async runtime. Shared
changes require Lead review after this ADR is merged.

## Consequences

Infrastructure, sandbox, state, policy, and test harness work can proceed in
separate worktrees. Alpha contract changes remain possible, but must be explicit,
versioned, and integrated before dependent branches rebase.

## Alternatives

Duplicated DTOs in each module and a single control-plane implementation crate
were rejected because both obscure trust boundaries and impede independent tests.

## Security implications

Unknown JSON fields fail closed. Approvals bind to canonical parameter hashes.
Prompts carry hashes; audit storage must never retain raw enterprise content.
Sandbox commands are executable-plus-argument arrays with explicit environment.
The action hash is domain-separated and binds run, action, resource, and exact
parameters. API actors come from authenticated server context, never request
bodies. The Control API never mounts the Docker socket; a host-side sandbox
backend is injected across a narrow boundary instead.

The v1 approval binding is the SHA-256 of the UTF-8 compact JSON encoding of
`["openwork-action-approval-v1", run_id, action_id, action, resource,
canonical_parameters]`. Object keys are recursively sorted, numbers must be
integers, depth is at most 32, and the encoded binding is at most 64 KiB. HTTP
and IPC decoders must reject duplicate object keys before constructing a JSON
value. The server computes this binding; callers cannot supply risk, actor, or
the authoritative hash.

Run and approval mutations use the persisted `revision` as compare-and-swap
state. Approval consumption and the associated action claim happen in one
database transaction. A pending approval can become approved, denied, or
expired; an approved approval can become consumed or expired and can be
consumed only once for its exact run/action/binding.

`SandboxRequest` is an internal worker contract, not an HTTP request body.
Public DTOs never grant host mount authority. The backend canonicalizes input
and output below configured roots, owns the temporary directory, rejects
symlinks and special files, clears the host environment, and passes only the
explicit container environment allowlist. Runtime provider artifact events are
untrusted hints; only the sandbox output scanner can create an Artifact.

Audit persistence never stores raw prompts, stdout, stderr, runtime message
content, credentials, cookies, authorization headers, or tool payloads. It stores
hashes, sizes, counters, machine codes, and centrally redacted structured
metadata. The v1 event hash is SHA-256 over the UTF-8 compact JSON encoding of
`["openwork-audit-event-v1", event_id, run_id, sequence, event_type, actor,
utc_timestamp, canonical_redacted_metadata, previous_hash_or_null]`. Sequence
starts at 1; only sequence 1 has no previous hash. Inserts allocate the next
per-run sequence and previous hash in the same transaction, and deserialization
recomputes the digest before accepting a persisted event.

Approval expiry is evaluated only against the trusted server clock. `now >=
expires_at` is expired, TTL cannot exceed 24 hours, an approval decision cannot
extend expiry, and consumption requires the exact approved revision and binding
inside the action-claim transaction.

## License implications

The contract adds only Apache-2.0/MIT-compatible dependencies recorded in the
repository's third-party notices.

## Revisit trigger

Revisit when two production sandbox or runtime backends cannot safely implement
the frozen contract without vendor-specific fields in the common layer.
