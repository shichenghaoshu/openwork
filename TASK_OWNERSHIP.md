# M1 task ownership

Contract-breaking changes require Lead approval. Agents work in separate Git
worktrees and do not edit another owner's paths.

| Agent | Issues | Branch | Owned paths | Dependencies | Status |
| --- | --- | --- | --- | --- | --- |
| Lead | #64 | `opencat/m1-contracts` | root Cargo files, `openwork-core`, frozen `openwork-execution/src/lib.rs`, ADRs, ownership, shared schemas, README, global workflows | M0 main | In progress |
| A Infrastructure | #3, #6 | `opencat/m1-control` | `crates/openwork-control-api/**`, `compose/**`, `migrations/**`, `contracts/openapi/**`, `docs/control-api/**` | #64 | Waiting for contract merge |
| B Sandbox | #12 | `opencat/m1-sandbox` | `crates/openwork-sandbox/**`, `tests/sandbox/**`, `docs/security/sandbox.md` | #64 | Waiting for contract merge |
| C Execution | #13 | `opencat/m1-execution-state` | implementation modules below `crates/openwork-execution/src/`, `tests/execution/**` | #64 | Waiting for contract merge |
| D Policy | #14, #15 | `opencat/m1-policy` | `crates/openwork-policy/**`, `contracts/schemas/policy/**`, `tests/policy/**`, `docs/admin/approvals.md` | #64 | Waiting for contract merge |
| E Runtime | #63 | `opencat/m1-runtime-run` | runtime provider modules, `tests/runtime-execution/**` | #12, #13, #64 | Waiting for first-wave interfaces |
| F QA | #65 | `opencat/m1-safe-e2e` | `tests/safe-execution/**`, `samples/sales/**`, `docs/demo/safe-execution.md`; global workflow changes are suggestions to Lead | #3, #12–#15, #63–#64 | Harness planning |

## Shared-file rule

Root `Cargo.toml`, `Cargo.lock`, bilingual READMEs, `ROADMAP.md`, `WORKLOG.md`,
schema indexes, release files, and global CI workflows are Lead-owned. Subagents
report required changes instead of editing those files.
