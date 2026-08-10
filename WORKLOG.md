# Work log

This file is updated before each development session ends.

## 2026-08-10 — Phase 0 bootstrap

- Read the full 2,252-line construction prompt and limited implementation to Phase 0.
- Created the public `shichenghaoshu/openwork` repository, required labels, four milestones,
  and canonical Issues #1–#30. Closed three transient duplicate issues created from a stale
  list response.
- Verified 12 requested upstreams from official repositories, documentation, releases, and
  registries; recorded exact commits, licenses, candidate image digests, and integration status.
- Marked LibreChat integration blocked because its official compose still uses an image that
  cannot be correlated with v0.8.7; recorded Goose's AAIF repository migration; opened Issue #34
  for MinerU's additional license terms.
- Added Apache-2.0 governance, bilingual README skeletons, security/support/contribution docs,
  nine ADRs, third-party notices, version lock, GitHub templates, Project bootstrap instructions,
  and CI skeleton.
- Implemented the TypeScript installer skeleton with `version`, `doctor [--json]`, and the
  non-mutating `install --dry-run [--json]` plan.
- Added tests first, observed the expected missing-implementation failure, then reached 6/6 tests
  passing. Verified formatting, lint, typecheck, unit, integration, build, Markdown links, YAML,
  dependency audit, secret patterns, and the exact Apache license text.
- GitHub Project remains pending because the authenticated token lacks `project`/`read:project`;
  `.github/PROJECT_SETUP.md` and `scripts/bootstrap-github.sh` contain the exact continuation.
- Opened the first pull request: https://github.com/shichenghaoshu/openwork/pull/35

## 2026-08-10 — Bootstrap Runtime Milestone complete

- Clarified the input scope: the earlier construction prompt contains 2,252
  lines, while the final attached engineering prompt contains 5,865 lines. Read
  the final prompt in full and implemented the Bootstrap Runtime Milestone it
  defines; later product phases remain outside this release.
- Delivered and merged PRs #44–#60. Closed every issue in the milestone: #1, #2,
  #5, #10, #39, #40, #41, #42, #43, #55, and #59.
- Replaced the temporary TypeScript CLI with a seven-crate native Rust workspace
  pinned to Rust 1.95.0. Implemented platform detection, structured Doctor
  results, stable errors, JSON schemas, redaction, runtime discovery, status,
  dry-run, consent-gated execution, and `OpenWork 0.1.0` version output.
- Implemented the complete runtime lifecycle contract, registry and manifests,
  MockRuntime compatibility coverage, and external-managed Claude Code and Codex
  adapters tied to official upstream sources without redistributing their code.
- Added a shell-free command runner and allowlisted HTTPS downloader with
  cancellation, timeouts, bounded data and output, redirect validation,
  streaming SHA-256, atomic no-clobber persistence, rollback, and explicit
  partial-state reporting after an irreversible command begins.
- Added a versioned runtime lockfile carrying requested/resolved versions,
  source, checksum authority, install path, timestamps, status, upstream, and
  license. Secrets live separately; sidecar file locking, atomic replacement,
  schema migration boundaries, concurrency tests, and Unix `0600` permissions
  protect the file.
- Persisted validated runtime provenance after a successful install and made
  `openwork status` distinguish installed, not-installed, and corrupt-lockfile
  states. An isolated directory-only execution test proves first-run lockfile
  creation without contacting a provider.
- Added native compatibility CI for macOS arm64/x64, Ubuntu arm64/x64, and
  Windows Server 2025 x64, plus POSIX and PowerShell installer syntax checks.
  All 20 PR checks passed, including the five native platform jobs.
- Added tag-only release automation with SemVer/workspace-version enforcement,
  locked tests and builds, five archives, per-asset and consolidated SHA-256,
  embedded build provenance, GitHub attestations, and a pinned Syft SPDX SBOM.
  Added checksum-verifying POSIX and PowerShell installers with default overwrite
  refusal and forced-update backup/restore.
- Local verification passed formatting, strict Clippy, the locked workspace test
  suite (52 passed, one subprocess helper intentionally ignored), release build,
  schemas, documentation links, and installer syntax.
- On the real macOS arm64 host, Claude Code was healthy and authenticated at
  version `2.1.126 (Claude Code)`. The installed Codex wrapper was broken because
  its vendor executable is absent. OpenWork reported both states and did not
  repair, overwrite, install, or run a provider task. Both provider install plans
  were verified in non-mutating dry-run mode.
- Kept platform claims evidence-scoped: Windows Server is natively CI-tested;
  Windows 11 client and WSL2 are still manual-validation targets. macOS Doctor's
  low-memory warning exit was accepted only after validating its structured JSON.
- GitHub Project creation remains the sole authorization blocker because the
  token lacks `project`/`read:project`; `.github/PROJECT_SETUP.md` and
  `scripts/bootstrap-github.sh` preserve the exact continuation. MinerU's
  additional-license review remains tracked in later milestone Issue #34.
- Prepared `docs/release/v0.1.0-alpha.1.md` and the final tag/release verification
  workflow. Release URL and immutable asset evidence are recorded after the tag
  workflow completes.
- Merged the final documentation PR #61 after all 20 checks passed, protected
  `main` with those 20 required checks and strict PR/linear-history rules, and
  published the annotated `v0.1.0-alpha.1` tag from commit `4d8cbec`. The release
  workflow tested and built all five native targets, attested every archive,
  generated the SPDX SBOM, and published 13 assets at
  https://github.com/shichenghaoshu/openwork/releases/tag/v0.1.0-alpha.1.
- Independently downloaded every release asset. That check caught CRLF inherited
  from the Windows checksum file inside the otherwise-valid consolidated
  `SHA256SUMS`; GNU verification in CI had accepted it, while macOS `shasum -c`
  did not. Normalized the published Windows checksum and manifest to LF,
  regenerated `SHA256SUMS.sha256`, re-uploaded the three corrected assets, and
  made the workflow emit LF on Windows plus defensively normalize all inputs.
  A fresh portable checksum and GitHub attestation verification is required
  before Issue #30 and the milestone are closed.
