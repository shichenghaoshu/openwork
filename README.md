# OpenWork

The open-source enterprise AI agent execution control plane.

Install once. Give every employee a private AI assistant with company knowledge,
business tools, and safe execution.

[中文](README.zh-CN.md) · [Getting started](docs/getting-started.md) ·
[Deploy for a client](docs/deploy-for-client.md) · [Build a pack](docs/packs/build-your-first-pack.md)

> Status: the M1 completion work is under integration. A real-container sales
> demo, Postgres control state, policy/approval/action controls, artifacts,
> hash-chain audit, durable worker loop, and one-time prompt handoff are
> implemented. Employee Workspace and read-only GitHub/Feishu MCP discovery are
> now included; governed MCP tool execution remains disabled. See the evidence-scoped
> [current state](CURRENT_STATE.md).

## What employees will be able to do

- Ask questions using authorized company knowledge.
- Analyze spreadsheets and generate documents in an isolated sandbox.
- Query explicitly allowed business data with read-only credentials.
- Run business tools only when policy and approvals permit them.

## Built for AI service providers

- Deploy one isolated installation for one client company.
- Add versioned capability packs and adapters without forking the control plane.
- Diagnose, back up, upgrade, roll back, and support installations consistently.

Apache-2.0 Community code permits commercial implementation services, subject to
the licenses of third-party components. See [licensing](docs/licensing.md).

## Developer quick start

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
./scripts/demo-m1.sh
./target/release/openwork --version
./target/release/openwork status --json
./target/release/openwork doctor --json
./target/release/openwork install --dry-run --json
```

Release archives for the five native Tier 1 build targets are installed by the
checksum-verifying [POSIX](scripts/install.sh) and [PowerShell](scripts/install.ps1)
scripts. Existing binaries are refused
unless an explicit force option creates a backup first. See the
[release checklist](docs/release/checklist.md) and reproducible
[Bootstrap demo](docs/demo/bootstrap-runtime.md). See the
[alpha release notes](docs/release/v0.1.0-alpha.1.md) for delivered scope and
known limitations. The newer M1 source workflow is described in
[Getting started](docs/getting-started.md); release artifacts remain at the
published bootstrap alpha until the M1 integration is merged and released.

See the [platform evidence matrix](docs/platform-support.md) for the difference
between fixtures, CI smoke tests, and real-host validation.

The desktop [Employee Workspace](apps/admin-web/README.md) provides tasks,
approvals, and cached connector discovery through the authenticated Control API.
