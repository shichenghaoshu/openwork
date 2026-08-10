# Bootstrap Runtime release checklist

Releases are cut from immutable, v-prefixed SemVer tags. The release workflow
checks out the tag rather than the current branch, builds on five native runner
targets, runs the complete workspace test suite on each target, and publishes only
after every package job succeeds.

## Before tagging

1. Confirm `Cargo.lock` is committed and `cargo test --workspace --all-targets
   --locked` passes.
2. Confirm the Compatibility workflow is green for the exact commit.
3. Confirm `openwork --version`, install dry-run, Doctor, and runtime discovery
   output are suitable for the release notes.
4. Record any Windows 11 and WSL2 manual validation separately. Windows Server CI
   and Linux CI are not substitutes for those environments.
5. Review `LICENSE`, `NOTICE`, and `THIRD_PARTY_NOTICES.md`.

## Publish and verify

1. Create and push an annotated tag, for example `v0.1.0-alpha.1`.
2. Wait for `.github/workflows/release.yml` to finish. Do not manually upload a
   partial platform set.
3. Download `SHA256SUMS` and the desired archive, then verify the archive checksum.
   The consolidated manifest uses LF line endings so both GNU `sha256sum` and
   POSIX-oriented `shasum -a 256 -c` consumers can parse every platform entry.
4. Confirm `openwork-<tag>-sbom.spdx.json` is present and its digest is listed in
   `SHA256SUMS`. It is generated from the tagged source tree with pinned Syft.
5. Verify GitHub's build provenance attestation:

   ```bash
   gh attestation verify openwork-v0.1.0-alpha.1-aarch64-apple-darwin.tar.gz \
     --repo shichenghaoshu/openwork
   ```

6. Run the install script with the explicit tag and repeat the CLI smoke checks.

Every archive contains `build-provenance.json` with the source commit, target,
toolchain, and locked-build flag. SHA-256 proves that the downloaded bytes match the
release checksum; the GitHub attestation links the archive digest to its workflow.
