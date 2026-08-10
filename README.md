# OpenWork

The open-source AI workspace installer for small businesses.

Install once. Give every employee a private AI assistant with company knowledge,
business tools, and safe execution.

[中文](README.zh-CN.md) · [Getting started](docs/getting-started.md) ·
[Deploy for a client](docs/deploy-for-client.md) · [Build a pack](docs/packs/build-your-first-pack.md)

> Status: `v0.1.0-alpha.1` Bootstrap Runtime Milestone. The native Rust CLI
> supports version, structured doctor/status output, runtime discovery, and
> consent-gated install planning and execution.

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

## Bootstrap developer quick start

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release
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
known limitations.

See the [platform evidence matrix](docs/platform-support.md) for the difference
between fixtures, CI smoke tests, and real-host validation.
