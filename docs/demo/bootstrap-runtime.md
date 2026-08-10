# Bootstrap Runtime demo runbook

This is a reproducible command sequence, not a prerecorded claim. Capture stdout,
stderr, exit status, release tag, commit, host, architecture, and date when running
it for release evidence.

```bash
openwork --version
openwork status --json
openwork doctor --json
openwork runtime list --json
openwork runtime info claude-code --json
openwork runtime info codex --json
openwork install --dry-run --json
```

The final command is intentionally a dry-run: it must not create directories,
download files, execute subprocesses, or replace an existing runtime. Runtime
detection can establish executable/version/auth observations; it does not prove a
provider account can complete a task.

For Windows PowerShell, replace `openwork` with `.\openwork.exe`. For WSL2, also
capture `uname -a` and the Windows `wsl.exe --status` output outside the Linux guest.
Do not label a Windows Server CI result as Windows 11 or WSL2 validation.
