# M1 Docker sandbox

`openwork-sandbox` is the host-side process boundary for an M1 run. It accepts
only the frozen internal `SandboxRequest`; public API callers cannot supply host
mounts or a temporary directory.

## Lifecycle

The backend uses the Docker CLI without a shell:

1. Validate that the already-approved input, output, and backend temporary roots
   are real directories rather than symlinks.
2. Create a private backend metadata directory and a separate runtime temporary
   directory. The metadata directory is never mounted into the task container.
3. Write the explicit container environment allowlist to a private environment
   file. Values are not placed in the Docker CLI argument list.
4. Run `docker create --cidfile ...` and read the full container ID from the
   backend-owned cidfile. A failed create that produced an ID is still cleaned.
5. Run `docker start <id>` (detached), then poll `docker inspect <id>` until the
   container exits, is cancelled, runs out of memory, or exceeds its deadline.
6. Read stdout and stderr through one shared bounded capture budget.
7. Attempt `docker kill <id>` and `docker rm --force <id>` from a scope guard.
   The guard also runs on early returns and panic unwinding.
8. Scan only the approved output mount for portable regular-file paths, remove
   backend temporary data, and return a machine-readable cleanup status.

The active-run registry has its own scope guard. A failed `start`, `inspect`, or
log operation therefore cannot leave a stale cancellation entry. User output is
outside the backend temporary directory and remains readable after container
cleanup, timeout, or cancellation.

## Enforced Docker configuration

Every task container uses:

- a digest-pinned image from the frozen contract;
- the contract's non-zero UID and GID;
- `--network none`;
- `--read-only` root filesystem;
- `--cap-drop ALL` and `no-new-privileges`;
- CPU quota, memory limit, and PID limit from bounded contract values;
- input bind mount as read-only;
- output and runtime-temporary bind mounts as writable;
- a bounded, `noexec,nosuid,nodev` `/tmp` tmpfs;
- explicit environment allowlist only.

The backend never emits `--privileged`, host PID/network flags, or a Docker
socket mount. The configured Docker CLI executable must be an absolute path.
Its subprocess calls `env_clear` and receives only explicitly configured local
transport variables. Docker's daemon-default seccomp and AppArmor behavior is
retained; this M1 backend does not claim a custom profile.

## Verification

Deterministic fake-CLI tests verify command ordering and exact hardening flags,
bounded output, non-root identity, no-network and no-socket representation,
timeout, cancellation, OOM classification, ID-based cleanup, failed-create
recovery, stale-registry prevention, retained output, cleanup failure reporting,
output symlink rejection, and mount replacement with a symlink. Unix tests also
exercise the real process runner to prove inherited environment clearing,
combined output bounds, and CLI timeout behavior.

Run:

```bash
cargo test --locked -p openwork-sandbox
cargo clippy --locked -p openwork-sandbox --all-targets -- -D warnings
```

These tests do not prove behavior of a live Docker daemon. A Docker-backed CI
integration test must still demonstrate that rootfs writes and network access
fail inside a real container and that Docker daemon cleanup leaves no container.

## Known M1 limitations

- No custom seccomp or AppArmor policy is installed.
- Bind-mounted output has no portable Docker disk quota; deployment must bound
  host storage and concurrent runs.
- On Windows, privacy of the backend metadata root relies on its configured ACL;
  Unix explicitly applies private metadata and environment-file modes.
- A privileged host process can race filesystem validation. The temporary root
  and approved mount roots must be controlled by the OpenWork service account.
- Output discovery rejects symlinks and special files, but artifact hashing and
  the 100 MiB per-artifact limit belong to the execution artifact scanner.
