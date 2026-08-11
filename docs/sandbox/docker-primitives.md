# Docker sandbox filesystem primitives

The M1 Docker backend treats its host filesystem inputs as untrusted. Output
discovery uses an iterative traversal and rejects a tree after 4,096 combined
file and directory entries, more than 1,024 files, or more than 64 directory
levels. Symlinks and special files also fail the scan. These limits prevent an
empty-directory tree from bypassing the file count and prevent recursive stack
exhaustion.

On Unix, the backend-owned metadata directory and `container.env` are created
with owner-only modes (`0700` and `0600`) in the creation operation. The
container-writable runtime directory is created separately and contains no
host credentials.

Rust's standard library does not provide an owner-only Windows ACL creation
primitive. Until an audited ACL implementation is available, the Docker
sandbox therefore returns `SandboxUnavailable` on non-Unix platforms before
creating a metadata directory or environment file. It must not silently fall
back to inherited or world-readable permissions.
