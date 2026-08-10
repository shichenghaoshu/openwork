# Getting started

The Bootstrap Runtime is a native developer preview. It does not install the
broader OpenWork service stack.

## Developer requirements

- Rust 1.95 or newer for source builds
- Git for source checkout and selected runtime workflows
- Docker is optional and reported as `SKIP` when absent

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

Never put provider keys on a command line or in the runtime lockfile. See the
[platform evidence matrix](platform-support.md) before treating CI as proof of a
Windows 11 or WSL2 host.
