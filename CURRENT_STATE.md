# OpenWork current state

This document is the single status source for the repository. Claims below are
scoped by evidence level and were last refreshed on 2026-08-22.

## Working

- `openwork demo sales` runs the deterministic July/August sales analysis in a
  digest-pinned, hardened container, scans the two artifacts, verifies their
  golden bytes and hashes, records the audit chain, and reports `Succeeded`.
- The reusable sales runner uses `SystemDockerCli`, `DockerSandbox`, and
  `ExecutionOrchestrator`; input mounts are read-only and the analyzer is
  invoked as an executable plus fixed arguments without a shell.
- Docker execution starts without blocking on provider stdin. A separate
  attach worker delivers bounded stdin while timeout and cancellation polling
  remain active; timeout, cancel, and transport failure kill and clean up the
  container.
- Runtime task adapters prepare Claude Code and Codex invocations with the
  prompt on stdin rather than argv. Strict JSONL decoding validates run IDs,
  event sequences, output bounds, terminal events, truncation, and exit state.
- The execution store has in-memory and Postgres implementations. Postgres
  persists runs, approvals, single-use action claims, artifacts, and hash-chain
  audit events with CAS revisions and transactional state changes.
- The authenticated Control API persists run creation, reads runs/events/
  artifacts/approvals, and performs approval, denial, and cancellation requests
  with trusted server time and actor identity. Unleased queued or
  awaiting-approval runs cancel immediately; active work records an intent and
  returns `202` without claiming a terminal state.
- The execution store provides database-mediated worker leases with random
  capability tokens, bounded heartbeats, deterministic claim ordering, and
  fail-closed expiry. Only a current lease plus a validated cancelled sandbox
  result and successful cleanup can confirm terminal `Cancelled`.
- Startup recovery fails expired leases and unleased orphaned `Planning` or
  `Running` runs while preserving valid leases. M1 has no `Cancelling` state;
  the durable cancellation intent is separate from the public run status.
- Policy tests cover automatic filesystem read/write, exact-bound L3
  `email.send` approval, single-use claim consumption, replay and parameter
  tampering rejection, and direct L4 `database.delete` denial.
- `ActionExecutor` accepts only a repository-verified `ClaimedAction`.
  `MockActionExecutor` provides side-effect-free, action-ID-idempotent M1
  execution and records an `action_executed` audit event.
- Docker and Podman share one hardened container policy builder through a
  sealed engine adapter. Docker remains the real M1 backend; Podman reports its
  host-dependent capabilities rather than implying parity.
- Compose starts a non-root, read-only Control API with a read-only workspace
  mount and a digest-pinned Postgres service. The service runs migrations and
  recovery before listening.

## Tested

- Workspace formatting, locked checking, strict Clippy, all-target tests, and
  release build pass locally.
- The default workspace suite exercises runtime decoding, sandbox lifecycle,
  artifact/path safety, policy, approval binding/replay, audit integrity,
  orchestrator terminal states, Control API fail-closed behavior, and the M1
  control-plane scenario.
- Real-Postgres tests exercise approve-versus-deny, consume-versus-consume,
  cancel-versus-complete, queue claim ownership, cancellation confirmation,
  lease expiry, revision races, and selective/idempotent crash recovery.
- CI includes a real Docker daemon sales test and a real Postgres concurrency
  job. Compose CI builds and starts the deployed services, checks health,
  creates an authenticated run, verifies prompt omission, and reads its genesis
  audit event.
- CI now runs actual CodeQL analysis and scans the built Control API image for
  critical vulnerabilities instead of using placeholder or duplicate checks.

## Real-host verified

- macOS arm64 with Docker Server 29.2.0: the digest-pinned BusyBox sales
  container completed, produced byte-identical CSV/Markdown artifacts, passed
  artifact hashing and audit verification, and cleaned up.
- macOS arm64 with a real PostgreSQL 17.6 container: all ten transaction-race,
  queue-lease, cancellation, expiry, and recovery tests passed.
- macOS arm64 Compose: migrations 1 through 4 applied; Postgres and the Control
  API became healthy; an authenticated queued run persisted without returning
  its prompt, then cancelled immediately with `cancel_confirmed` audit evidence.
- `scripts/demo-m1.sh` completed Doctor, the real-container sales demo, and both
  policy/approval/action control-plane scenarios without sending external
  email.

## Fixture only

- Claude Code and Codex adapter commands, stdin routing, and provider event
  decoders are fixture-tested. A default-ignored, explicit-auth `HostOnly`
  harness can exercise a locally installed provider CLI with bounded output and
  a cleared environment, but no real provider task was invoked during this
  checkpoint and this is not production-sandbox evidence.
- Podman command routing and hardened-argument equivalence are fixture-tested;
  no real Podman host lifecycle has been run.
- Tier-1 macOS x64, Linux x64/arm64, and Windows Server 2025 x64 compatibility
  is CI evidence. It is not evidence for Windows 11, WSL, or arbitrary desktop
  distributions.
- The external-action path ends at `MockActionExecutor`; no email, ERP, CRM, or
  other connector is enabled.

## Missing

- A durable worker/dispatcher that claims queued Control API runs and drives
  the generic `RuntimeTask -> adapter -> sandbox -> events -> artifacts ->
  terminal state` path. Lease and cancellation repositories now exist, but no
  production worker calls them. Until it does, `openwork run` fails clearly and
  does not create a misleading queued run.
- End-to-end cancellation from `POST /v1/runs/:id/cancel` through a durable
  worker to runtime and sandbox termination. The route safely persists active
  cancellation intent, but no production worker yet polls and confirms it.
- A credential-gated real Claude Code or Codex execution image and an actual
  provider run. The checked-in host probe is optional and was not invoked.
- Real Podman host validation and durable production idempotency for a real
  external-action executor.
- The thin employee/admin web UI, intentionally deferred to M1.1.

## Blockers

- The deterministic M1 demo is repeatable, but the generic API/CLI execution
  product is not production-ready until queued runs have an owned worker and a
  secure input-delivery boundary. Prompts are deliberately not persisted in
  plaintext, so a worker must not be connected by weakening that invariant.
- Enterprise pilot readiness additionally requires provider-image provenance,
  real provider validation, operational credential brokering, deployment
  observability, backup/restore, and an external security review.
