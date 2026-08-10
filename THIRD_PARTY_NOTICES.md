# Third-party notices

Reviewed on 2026-08-10 from official upstream sources. A listing is not a statement
that the component is already distributed by OpenWork. `candidate` and `optional`
components remain disabled until their integration issue passes tests and license review.

| Component        | Reviewed version   | License boundary                    | OpenWork decision                                                                       |
| ---------------- | ------------------ | ----------------------------------- | --------------------------------------------------------------------------------------- |
| LibreChat        | v0.8.7             | MIT                                 | Default UI candidate; blocked until a release-correlated image can be pinned by digest. |
| LiteLLM          | v1.95.0            | MIT outside `enterprise/`           | Default gateway candidate; OSS-only use.                                                |
| Goose            | v1.45.0            | Apache-2.0                          | Default runtime candidate through an adapter.                                           |
| Unstructured API | 0.1.2              | Apache-2.0                          | Default parser candidate through an adapter.                                            |
| PostgreSQL       | 18.4               | PostgreSQL License                  | Default database candidate.                                                             |
| pgvector         | v0.8.6             | PostgreSQL License                  | Default vector extension candidate.                                                     |
| gVisor           | release-20260803.0 | Apache-2.0                          | Optional hardened sandbox profile.                                                      |
| Activepieces     | 0.87.0             | MIT CE; commercial `ee` areas       | Optional CE adapter only.                                                               |
| Langfuse         | v4.6.0             | MIT core; commercial `ee` areas     | Optional observability profile only.                                                    |
| RAGFlow          | v0.26.4            | Apache-2.0                          | Optional advanced knowledge profile.                                                    |
| MinerU           | 3.4.4              | Apache-2.0 plus additional terms    | Optional; blocked on formal license review.                                             |
| New API          | v1.0.0-rc.24       | AGPL-3.0 plus documented conditions | External optional adapter; never bundled by default.                                    |
| Claude Code      | v2.1.226           | Proprietary commercial terms        | Detect/call a user-installed copy only; no redistribution.                              |
| serde            | 1.0.229            | MIT OR Apache-2.0                    | Rust data model serialization.                                                          |
| sysinfo          | 0.39.6             | MIT                                 | Read-only host memory and OS facts.                                                     |
| fs2              | 0.4.3              | MIT OR Apache-2.0                    | Read-only filesystem capacity facts.                                                   |
| serde_json       | 1.0.151            | MIT OR Apache-2.0                    | Development-time JSON diagnostic assertions.                                           |
| clap             | 4.6.6              | MIT OR Apache-2.0                    | Rust command-line parsing and help output.                                              |
| tempfile         | 3.27.0             | MIT OR Apache-2.0                    | Development-only dry-run side-effect tests.                                             |
| atomicwrites     | 0.4.4              | MIT                                 | Cross-platform atomic replacement of OpenWork-managed state files.                      |

Exact repositories, commits, image digests, and evidence links are in
[the upstream matrix](docs/upstream-matrix.md), [the version lock](installer/versions.lock.yaml),
and `third_party/`.
