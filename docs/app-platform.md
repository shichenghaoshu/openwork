# App Platform runbook

This runbook starts the App Platform integration without putting provider
prompts or credentials in command arguments.

## Inspect runtimes and Apps

```bash
cargo run -p openwork-cli -- runtime list --json
cargo run -p openwork-cli -- app list --registry registry/apps --json
cargo run -p openwork-cli -- doctor --json
```

OpenCode and Pi are external-managed CLIs for host detection. Production tasks
use only explicitly configured, digest-pinned provider images and absolute
executables inside those images.

## Control API

The safe default Compose deployment keeps the worker disabled because the
Control API container has no container-engine socket. For a host deployment
that deliberately owns a Docker worker, configure:

```bash
export OPENWORK_WORKER_ENABLED=1
export OPENWORK_DOCKER_BIN=/usr/local/bin/docker
export OPENWORK_WORKER_TEMP_ROOT=/absolute/openwork/tmp
export OPENWORK_WORKER_OUTPUT_ROOT=/absolute/openwork/outputs
export OPENWORK_PROVIDER_NETWORK=openwork-provider-egress

# Configure one or more digest-pinned provider images.
export OPENWORK_CODEX_IMAGE='registry.example/codex@sha256:...'
export OPENWORK_CLAUDE_IMAGE='registry.example/claude@sha256:...'
export OPENWORK_OPENCODE_IMAGE='registry.example/opencode@sha256:...'
export OPENWORK_PI_IMAGE='registry.example/pi@sha256:...'
```

The corresponding executable variables are optional and default to
`/usr/local/bin/{codex,claude,opencode,pi}`. Auto selects Codex, Claude Code,
OpenCode, then Pi from the configured set. The provider network must be created
and restricted by the operator; OpenWork never falls back to default or host
networking.

## GitHub App capability

Set a least-privilege token in the Control API process and provide an absolute
Docker executable:

```bash
export OPENWORK_GITHUB_TOKEN='...'
export OPENWORK_GITHUB_MCP_DOCKER_BIN=/usr/local/bin/docker
```

Install `org.openwork.github` from Employee Workspace or with the local CLI.
Read capabilities execute their exact MCP mapping immediately. Issue creation
returns `202 approval_required`; approve it, then call
`POST /v1/approvals/{id}/execute` with the approved revision. This second route
consumes the exact action claim and refuses replay.

## Feishu discovery

```bash
export OPENWORK_LARK_MCP_NPX_BIN=/absolute/path/to/npx
export OPENWORK_LARK_APP_ID='...'
export OPENWORK_LARK_APP_SECRET='...'
export OPENWORK_LARK_DOMAIN='https://open.feishu.cn'
```

Only the reviewed read-only allowlist is exposed. Absence of credentials leaves
Feishu visible but unavailable and does not block GitHub or LibreOffice.

## LibreOffice App

```bash
docker build -t openwork/libreoffice-app:dev containers/libreoffice-app
docker image inspect openwork/libreoffice-app:dev --format '{{.Id}}'
```

Use the resulting `sha256:...` content ID for local evidence or publish the
image and use a registry digest in production. The explicit real-container
smoke test is:

```bash
OPENWORK_REAL_DOCKER_TESTS=1 \
OPENWORK_DOCKER_BIN=/usr/local/bin/docker \
OPENWORK_LIBREOFFICE_IMAGE='sha256:...' \
cargo test -p openwork-e2e --test real_libreoffice --locked -- --ignored --nocapture
```

## Employee Workspace

```bash
cd apps/admin-web
npm install
OPENWORK_CONTROL_API_BASE_URL=http://127.0.0.1:8080 \
OPENWORK_CONTROL_API_TOKEN='...' \
npm run dev
```

The desktop bridge exposes only fixed routes. Uploads are bounded to 8 MiB;
selected files are copied into the chosen workspace and mounted read-only for
the task. Task detail polls the real run, artifacts, action audit, approval
records, and the per-run audit event chain rather than using sample data.

The renderer is bilingual: the header switch toggles zh-CN/English and the
choice persists in `localStorage`. From the Apps page, employees can invoke
reviewed capabilities with parameter forms generated from each capability's
JSON schema; `github.issue.create` shows the required owner/repo/title/body
fields and routes the request into the durable approval flow, which the
Approvals page executes with a dedicated Execute button after approval. The
Workspace page can run the real-container sales demo, and the sample report
lists its artifacts and audit evidence. `LibreOffice` capability invocation
uses the demo harness path (`OPENWORK_LIBREOFFICE_IMAGE` required; a missing
image returns a clean `demo_not_configured` 503), which covers
`libreoffice.office_bundle.create` only. The Connectors page shows connector
status and the tool allowlist (Feishu is reported as `not_configured` until
tenant credentials are supplied).
