# OpenWork current state

This is the repository's evidence-scoped status source. It was refreshed on
2026-08-27 for the credential boundary, durable prompt recovery, and governed
procurement workflow integrations.

## Working

- A reviewed `SecretStore` (Postgres production backend + deterministic memory
  backend) owns credential values sealed with ChaCha20-Poly1305 under the
  shared key boundary. GitHub and Feishu/Lark connectors resolve their
  credential slots through it; a stored credential is authoritative, and
  revoked or expired credentials fail closed without operator-env fallback.
  Admin API/UI exposes credential status and rotate/revoke/enable only — raw
  values never reach API, audit, logs, or serialization
  (`docs/admin/credentials.md`).
- Durable encrypted prompt recovery is available as the durable delivery mode:
  a `prompt_envelopes` row binds run ID, digest, key, nonce, ciphertext, and a
  24h expiry; decryption happens only in the worker boundary before stdin, and
  single-use consumption is atomic. Corrupt, expired, and replayed envelopes
  fail closed; lifecycle is audited without prompt material. `OPENWORK_PROMPT_DELIVERY=durable`
  requires `OPENWORK_CRYPTO_KEY_PROMPT_ENVELOPE_V1` or the service refuses to
  start (`docs/admin/prompt-recovery.md`).
- `org.openwork.erp.procurement` ships the governed procurement demo: read-only
  budget/vendor/item capabilities plus an L3 `erp.purchase_request.create` that
  enters AwaitingApproval with exact parameter binding, consumes a single-use
  approval claim, creates a deterministic mock ERP request (`PR-2026-0001`),
  and attaches Markdown + XLSX artifacts. Denial never writes; replay is
  rejected; the full audit trail is redacted. A real ERP implements the
  `ErpAdapter` boundary without changing policy or approval semantics
  (`docs/admin/procurement.md`).

- Employee Workspace has Workspace, Tasks, Files, Apps, and Approvals, plus
  administrator Runtime, App Store, and Settings views. Its Electron renderer
  receives neither the Control API bearer token nor an arbitrary HTTP bridge.
- Uploaded files are content-addressed, bounded to 8 MiB, selected by exact ID,
  copied into a root-bounded workspace input directory, and presented to the
  sandbox as read-only task inputs.
- Runtime selection supports `auto`, `claude-code`, `codex`, `opencode`, and
  `pi`. Auto deterministically chooses the first configured provider in the
  order Codex, Claude Code, OpenCode, Pi; explicit selections never fall back.
- OpenCode uses `opencode run --format json`; Pi uses `pi --mode rpc
  --no-session`. Both prompts travel on stdin, their JSONL outputs are bounded
  and decoded into OpenWork runtime events, and production execution remains in
  the configured container sandbox.
- Capability Pack v1, the App Registry, exact capability grants, policy
  decisions, opaque authorization, single-use approved action claims, and
  scoped credential-provider boundaries are implemented.
- GitHub is a reviewed App with exact mappings for repository search, file,
  issue, workflow-run, and pull-request reads. `github.issue.create` is L3: the
  HTTP request creates an exact-bound durable approval, and execution accepts
  only the consumed claim. There is no `approved=true` bypass or arbitrary MCP
  tool-name route.
- External GitHub execution reserves the action before the remote call and
  refuses replay. An ambiguous remote result remains fail-closed instead of
  retrying a potentially completed side effect.
- Feishu/Lark discovery is configured through the official pinned stdio MCP
  package and an exact read-only tool allowlist. No write capability is exposed.
- The CLI provider accepts only a gateway authorization, sends dynamic
  parameters through bounded stdin, runs with network disabled, and exposes no
  host shell path. The CLI-Anything importer treats SKILL.md as documentation,
  not as trusted permissions or executable policy.
- The LibreOffice App image pins the official CLI-Anything source revision,
  runs LibreOffice headless with a fixed adapter, and produces scanned XLSX and
  PPTX artifacts in a hardened, network-disabled container.
- `openwork runtime list/info`, `openwork app list/install/import`, and
  `openwork doctor` expose the runtime and reviewed local App inventory.
- Postgres worker leases, heartbeat, cancellation intent/evidence, recovery,
  approvals, artifacts, and hash-chain audit remain the durable execution base.

## Real-host verified

- macOS arm64 with Docker Server 29.2.0: a locally built content-addressed
  LibreOffice App image completed the governed office-bundle path and generated
  a real spreadsheet and presentation. Both artifacts were scanned and the run
  finished `Succeeded`.
- The official digest-pinned GitHub MCP Server completed real initialize,
  `tools/list`, and a governed repository-read call using an authenticated local
  credential without returning the credential.
- A disposable PostgreSQL 17 instance and the real Control API completed an
  L3 approval flow and created
  [GitHub issue #105](https://github.com/shichenghaoshu/openwork/issues/105).
  The durable run reached `Succeeded` with approval-requested, approved,
  consumed, action-executed, and run-completed audit events.
- The pre-existing real Docker sales demo and Postgres transaction/lease suites
  remain available as separate execution-control evidence.

## Contract or fixture verified only

- OpenCode and Pi detection, command construction, stdin routing, event
  decoding, Auto routing, and worker integration compile and pass their focused
  tests. No authenticated OpenCode or Pi model task was run in this checkpoint.
- Feishu/Lark startup and allowlist configuration are implemented, but no tenant
  credential was available for a live account probe.
- The LibreOffice proof exercises the exact Pi-shaped structured office plan
  through the Capability Gateway and sandbox, but it does not prove a live Pi
  provider generated that plan.
- Podman lifecycle routing is fixture-tested; the App Platform proof used Docker.

## Known production gaps

- Runtime provider images must still be built, provenance-checked, digest-pinned,
  supplied with their provider authentication, and validated in the target
  deployment. OpenWork deliberately does not run these CLIs directly on the host.
- Stored credentials still accept operator-supplied token material; a GitHub
  App installation/OAuth lifecycle and installation-token rotation are not yet
  implemented. The credential boundary itself (SecretStore, broker, metadata,
  revoke/rotate) is delivered.
- Installed/enabled App state and the default Runtime setting are process-local;
  reviewed manifests and external capability actions are durable, but those
  administrator preferences need a database migration before multi-instance use.
- Durable prompt recovery is delivered and exercised against Postgres, but a
  full live restart of the deployed Control API + worker with a real provider
  still needs target-deployment validation. The `memory` mode remains the
  local-development default; `durable` mode is the pilot-ready setting.
- The external action reservation provides replay refusal and at-most-once
  behavior. A crash between remote success and local reconciliation is reported
  as uncertain; it cannot provide universal exactly-once semantics without an
  upstream idempotency key.
- The procurement flow runs against the deterministic mock ERP adapter and a
  real live-feishu/procurement demo still needs a real ERP adapter and
  deployment observability, backup/restore, and external security review.
