# Contracts

OpenAPI and JSON Schema sources of truth live here. M1 typed safe-execution
contracts are frozen in `crates/openwork-execution`; external API schemas must
reference the same version and fail closed on unknown fields.

- `schemas/safe-execution.v1.schema.json` mirrors the M1 persistence and worker
  envelope. HTTP request schemas are narrower and never expose trusted actor,
  policy risk, authoritative hashes, or host mount paths as caller authority.
