# Troubleshooting

Run `openwork doctor --json` first. Each failed check includes remediation. Compare
the detected OS, architecture, environment, paths, permissions, memory, disk, and
prerequisites with the [evidence matrix](platform-support.md). Do not paste raw
environment files or credentials into an issue.

The Bootstrap Runtime does not start the broader service stack, migrate a database,
or contact a model provider during Doctor/status/dry-run. If an execute attempt fails,
inspect its rollback and `partial_state` fields before retrying. A command step can be
inherently irreversible even when created directories and downloaded files were
rolled back.
