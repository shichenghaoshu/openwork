# Deterministic safe-execution demo evidence

This directory currently proves only deterministic inputs, exact expected
outputs, action bindings, and fixture validation. It does **not** yet prove that
the task ran in Docker, that network access was blocked, that a sandbox was
cleaned up, or that the complete Run → Policy → Runtime → Sandbox → Artifact →
Audit pipeline succeeded.

## Checked-in evidence

`samples/sales/` contains LF-terminated UTF-8 inputs with these exact facts:

- July total: `33000`
- August total: `28500`
- Change: `-4500`
- Crown: `7000 → 4000`, decline `3000` (largest)
- Acme: decline `2000`
- Beta: growth `500`
- Delta: change `0`

The analyzer sorts customers by decline descending, then `customer_id`
ascending. It emits locale-independent integers and byte-exact
`sales-analysis.csv` and `summary.md`. Tests pin SHA-256 for both inputs and both
goldens and reject duplicate customers, invalid numbers, reordered output,
traversal, and symlink fixtures.

The scenario fixtures define, but do not execute:

- `email.send` with exact L3 parameters and expected `REQUIRE_APPROVAL`;
- `database.delete` with exact L4 parameters and expected `DENY`.

Each scenario pins the frozen action parameter hash. Changing the action,
resource, or parameters invalidates the fixture.

## Evidence still required

The release-grade safe vertical slice must later run `MockRuntime` through the
real execution store, policy engine, Docker sandbox, artifact scanner, and audit
chain. Docker assertions must prove non-root execution, read-only rootfs, no
network, no Docker socket, bounded resources/output, timeout/cancel, and cleanup.
Provider artifact events remain untrusted hints; only the output scanner may
create artifacts.

Real Claude Code and Codex tests stay opt-in behind
`OPENWORK_REAL_RUNTIME_TESTS=1`. Their provider adapters may prepare invocations
and decode events, but they are not the enterprise security boundary. Existing
host command execution, provider authentication, or a successful process exit
must not be presented as sandbox evidence. Real-provider authentication may
also conflict with the default no-network sandbox and must be documented rather
than bypassed.
