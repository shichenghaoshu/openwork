# Getting started

OpenWork is an enterprise AI agent execution control plane under active M1
integration. The deterministic M1 demo uses a real Docker container and no
provider credentials or external side effects. The published binary release is
still the earlier Bootstrap Runtime developer preview.

## Developer requirements

- Rust 1.95 or newer for source builds
- Git for source checkout and selected runtime workflows
- Docker is optional and reported as `SKIP` when absent

## Repeat the M1 demo from source

Start Docker, then run:

```bash
./scripts/demo-m1.sh
```

The script builds the CLI, runs Doctor, executes the digest-pinned sales
analysis container, verifies artifacts and audit, and exercises automatic,
approval-required, replay/tamper, and destructive-denial policy paths with a
side-effect-free mock action executor. Outputs are retained below
`openwork-demo-output/` unless `OPENWORK_DEMO_OUTPUT_ROOT` is set to another
absolute directory. No email is sent.

See [CURRENT_STATE.md](../CURRENT_STATE.md) before treating this demo as proof
of a generic queued-run worker or real Claude Code/Codex provider execution.

## Optional real-provider host probe

An ignored `HostOnly` test can validate a locally authenticated Claude Code or
Codex CLI without making provider access part of normal CI. It is not container
sandbox evidence and may consume provider quota or make provider network calls,
so run it only with explicit authorization:

```bash
OPENWORK_REAL_RUNTIME_TESTS=1 \
OPENWORK_REAL_RUNTIME_AUTH=1 \
OPENWORK_REAL_RUNTIME_PROVIDER=codex \
OPENWORK_REAL_RUNTIME_BIN=/absolute/path/to/codex \
cargo test -p openwork-e2e --test real_provider_runtime --locked -- --ignored --nocapture
```

Use `claude-code` and the absolute Claude executable for the other adapter.
Authenticate the selected CLI through its supported mechanism before opting in;
do not place credentials in this command. The harness clears the inherited
environment except for a minimal platform/authentication allowlist, supplies the
prompt on stdin, bounds captured output, and enforces a timeout.

## Bootstrap CLI commands

```bash
cargo test --workspace
cargo run -p openwork-cli -- --version
cargo run -p openwork-cli -- status
cargo run -p openwork-cli -- doctor --json
cargo run -p openwork-cli -- install --dry-run --json
cargo run -p openwork-cli -- install --dry-run --runtime claude --json
```

Dry-run does not create directories, download files, or execute subprocesses.
An actual managed-path change requires both `--execute` and `--yes`; review the
dry-run output first. Existing Claude Code or Codex installations are preserved and
must be updated with their official updater rather than silently overwritten.
After a successful execution, `runtime.lock.json` is atomically written under the
detected OpenWork data directory. `openwork status --json` validates and returns
that secret-free provenance; an invalid lockfile is an error rather than an
"installed" claim.

Never put provider keys on a command line or in the runtime lockfile. See the
[platform evidence matrix](platform-support.md) before treating CI as proof of a
Windows 11 or WSL2 host.
