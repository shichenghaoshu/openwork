# Platform support and evidence

OpenWork reports evidence, not aspiration. A fixture, a cross-compile, and a run on
the target operating system are different kinds of proof.

## Evidence vocabulary

- **Locally tested**: a maintainer ran the built binary on the named physical or
  virtual host and recorded the date.
- **CI tested**: GitHub Actions runs the workspace tests, release build, version
  smoke test, install dry-run, and Doctor on a native runner of that architecture.
- **Build only**: the target compiled, but the binary did not execute on that host.
- **Fixture tested**: deterministic platform-detector fixtures passed. This does not
  prove the binary runs on that platform.
- **Manual**: the documented check must be performed by a human; no automated claim
  is made until its evidence is recorded.

## Bootstrap Runtime matrix

The CI status below describes what `.github/workflows/compatibility.yml` is
configured to prove. A target becomes **CI tested** for a particular commit only
after that workflow has completed successfully for the commit.

| Target | Tier | Automated runner | Evidence available in repository | Real-host evidence |
| --- | --- | --- | --- | --- |
| macOS arm64 | 1 | `macos-15` (native arm64) | fixture tested; native CI configured | locally tested on macOS 15 arm64, 2026-08-10 |
| macOS x64 | 1 | `macos-15-intel` (native x64) | fixture tested; native CI configured | not yet recorded |
| Ubuntu x64 | 1 | `ubuntu-24.04` (native x64) | fixture tested; native CI configured | not yet recorded |
| Ubuntu arm64 | 1 | `ubuntu-24.04-arm` (native arm64) | fixture tested; native CI configured | not yet recorded |
| Windows x64 | 1 | `windows-2025` (native x64, Windows Server) | fixture tested; native CI configured | Windows 11 client-host run not yet recorded |
| Windows 11 x64 | 1 | no Windows 11 client runner | Windows Server CI is a build/test proxy, not Windows 11 proof | manual validation pending |
| WSL2 (Ubuntu) | 1 | no nested WSL2 hosted runner | Linux CI and fixtures only | manual validation pending |
| Windows arm64 | 2 | none | fixture tested; build not currently published | not yet recorded |
| Debian | 2 | none | Linux fixtures only | not yet recorded |

No cross-compiled artifact is labeled as runtime-tested. Release artifacts are built
on native runners for macOS arm64/x64, Ubuntu arm64/x64, and Windows x64. The release
workflow embeds the Git commit, Rust toolchain, target triple, and locked-build flag
inside each archive, emits SHA-256 checksums, and requests GitHub artifact provenance
attestations.

## Manual Windows 11 and WSL2 validation

For a release candidate, record the release tag, host build, architecture, command
output, and date. Do not replace the placeholders in this section with assumptions.

On Windows 11 x64 PowerShell:

```powershell
.\openwork.exe --version
.\openwork.exe install --dry-run --json
.\openwork.exe doctor --json
.\openwork.exe runtime list --json
```

Inside WSL2 Ubuntu:

```bash
uname -a
openwork --version
openwork install --dry-run --json
openwork doctor --json
openwork runtime list --json
```

The platform detector is read-only. Docker is reported when present but is not a
Bootstrap prerequisite. Unsupported operating systems or architectures fail before
installation planning with an actionable error.

## Installing verified release inputs

The bootstrap scripts download only from the canonical
`shichenghaoshu/openwork` GitHub Release location and verify the adjacent SHA-256
file before extracting or replacing anything:

```bash
curl -fsSLO https://raw.githubusercontent.com/shichenghaoshu/openwork/main/scripts/install.sh
sh install.sh --version v0.1.0-alpha.1
```

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/shichenghaoshu/openwork/main/scripts/install.ps1 -OutFile install.ps1
.\install.ps1 -Version v0.1.0-alpha.1
```

Existing binaries are refused by default. `--force` on POSIX or `-Force` on
Windows creates a timestamped backup immediately before replacement. These scripts
verify release checksums; GitHub provenance attestations can additionally be checked
with `gh attestation verify <archive> --repo shichenghaoshu/openwork`.
