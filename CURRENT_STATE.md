# OpenWork current state

This is the repository's evidence-scoped status source. It was refreshed on
2026-08-23 for the App Platform integration.

## Working

- Employee Workspace has Workspace, Tasks, Files, Apps, Approvals, and
  Connectors, plus administrator Runtime, App Store, and Settings views. The
  renderer is bilingual (zh-CN/EN switch persisted in localStorage), and it
  receives neither the Control API bearer token nor an arbitrary HTTP bridge.
- Apps can invoke reviewed capabilities directly: parameter forms are generated
  from each capability's JSON schema, `github.issue.create` runs through the
  durable approval flow with an in-app Execute step, and a Connectors page
  shows live connector status and the tool allowlist. Workspace runs the
  real-container sales demo and Tasks shows the per-run audit event chain.
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
- The worker inherits provider authentication and model routing from the host
  process through a fixed environment allowlist, so credentials never enter an
  image layer or the logs. Both demos (sales sandbox and `LibreOffice` bundle)
  are real-container harnesses exposed through authenticated routes that fail
  closed with a clean 503 when Docker or the office image is not configured.

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
- WSL2 (Docker 29.7.2) with the zh-CN/EN bilingual Employee Workspace: the
  worker-mode Control API completed a real claude-code task, the suite of three
  GitHub read capabilities (issue, actions, pull-request) and the L3 issue
  creation flow (issues #109 and #110 created), a `LibreOffice`
  `office_bundle.create` invocation, the sales demo endpoint, and connector
  discovery (43 GitHub tools; Feishu honestly reported as not configured).

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
- The current GitHub credential source is a scoped token supplied by the
  operator. A GitHub App installation/OAuth lifecycle and installation-token
  rotation are not yet implemented.
- Installed/enabled App state and the default Runtime setting are process-local;
  reviewed manifests and external capability actions are durable, but those
  administrator preferences need a database migration before multi-instance use.
- Prompt handoff is one-time and process-local, so restart loses undelivered
  prompt material and the worker fails closed. Durable encrypted prompt recovery
  remains a pilot-readiness requirement.
- The external action reservation provides replay refusal and at-most-once
  behavior. A crash between remote success and local reconciliation is reported
  as uncertain; it cannot provide universal exactly-once semantics without an
  upstream idempotency key.
- Enterprise pilot readiness still requires target-provider and Feishu live
  validation, secret-store integration, deployment observability, backup/restore,
  and external security review.
- Capability dispatch covers GitHub MCP reads, the L3 issue-creation approval
  flow, and the LibreOffice bundle demo path only. Other CLI-anything
  capabilities and Feishu invocations fail closed with
  `capability_dispatch_unavailable`; Feishu remains tool-discovery only.
- `github.issue.read` maps to the GitHub MCP v1.10 `list_issues` tool, which
  uses cursor pagination: the OpenWork schema exposes `limit` for the first
  page, and `page > 1` is rejected with an exact 422. Other read tools accept
  the current parameters unchanged.
- Connector failures use a short in-process negative cache (15 s); a transient
  provider failure can surface once and then succeed moments later, which the
  Workspace surfaces without hiding the server error.
