# Credential and secret boundary

OpenWork never lets Apps, agents, runtimes, or renderers read raw provider
credentials. The control plane owns a first-class secret boundary through which
Apps and runtimes receive exactly the scoped credential material they need, for
the shortest practical lifetime, without exposing raw secrets to the agent,
renderer, logs, audit records, or unrelated processes.

## Contracts

### `SecretStore` (v1)

The store is the single owner of credential values. It stores opaque values
under reviewed references and exposes only metadata plus a provider-scoped,
environment-bound value. Versioned contract in
`crates/openwork-apps/src/credential.rs`.

- `resolve(credential_ref)` — returns a provider-scoped value after enforcing
  disabled and expiry state. Failed resolution is fail-closed.
- `metadata(credential_ref)` / `list()` — one-way status: provider, scope,
  owner, creation/expiry/disabled timestamps, revision. Never includes a value.
- `store(entry)` — creates or replaces a reviewed entry. Replacing an existing
  entry keeps the original creation time and advances the revision.
- `rotate(credential_ref, value)` — replaces only the value, advancing the
  revision without touching metadata or the owning App.
- `revoke(credential_ref)` / `enable(credential_ref)` — disables/enables the
  credential for new executions while metadata stays observable.

Backends:

| Backend | Purpose |
|---|---|
| Postgres (`PostgresSecretStore`) | Production backend. Values are sealed with ChaCha20-Poly1305 under a configured key before reaching Postgres. |
| Memory (`MemorySecretStore`) | Deterministic local/test backend. Values live only in process memory and are dropped with the store. |

All values are sealed by `openwork-core::crypto::EncryptedValue`, which binds
ciphertext to a domain context and the credential reference, so a ciphertext
cannot be replayed under a different reference or context.

### `CredentialBroker` (v1)

The broker ties credential resolution to an authorization issued by the
`CapabilityGateway`. It resolves a credential only for an installed capability
whose manifest declares `permissions.credentials`, and only after the gateway
has authorized the exact invocation (`crates/openwork-apps/src/gateway.rs`).
`ScopedCredential` implements neither `Debug` nor `Serialize`, exposes no byte
accessor, and injects its value only into the exact child process environment.

## Integration paths

- **GitHub MCP** connector (`connector_runtime.rs`) resolves its token from the
  `github.app` slot. When a reviewed credential exists, the store is
  authoritative — a revoked or expired stored credential makes the connector
  unavailable **without** falling back to `OPENWORK_GITHUB_TOKEN`. When no
  reviewed credential exists, the legacy operator environment variable remains
  a fallback for controlled deployments.
- **Feishu/Lark MCP** connector resolves its tenant secret from the `feishu.app`
  slot with the same authority rules (`OPENWORK_LARK_APP_SECRET` is the
  fallback; `OPENWORK_LARK_APP_ID` stays static configuration).
- **AI runtime providers** authenticate from the provider image; the runtime
  containers remain credential-free from the host side. Provider image builds
  may consume an injected credential only through the same boundary.
- The Agent prompt path never carries provider credentials; prompts travel on
  stdin, never in argv or environment.

## Administrator workflow

All credential administration is metadata-only. Raw values are never returned
by the API, the Employee Workspace renderer, audit events, log output, support
bundles, or database metadata queries.

1. Generate an encryption key:

   ```bash
   export OPENWORK_CRYPTO_KEY_CREDENTIAL_STORE_V1="$(openssl rand -hex 32)"
   ```

   The control plane reads keys from environment variables named
   `OPENWORK_CRYPTO_KEY_<ID>` (uppercase, `.`/`-` → `_`). Key IDs: the common
   one is `credential-store-v1`. A KMS-backed `KeyProvider` can replace
   `EnvironmentKeyProvider` without changing the store contract.

2. Store a reviewed credential:

   ```bash
   curl -H "Authorization: Bearer $OPENWORK_API_TOKEN" \
     -X POST https://control.example.invalid/v1/admin/credentials \
     -d '{
       "credential_ref": "github.app",
       "provider": "github",
       "environment_variable": "GITHUB_PERSONAL_ACCESS_TOKEN",
       "scope": "repos,read_user",
       "owner": "control-admin",
       "value": "ghp_…",
       "expires_at": "2026-12-31T00:00:00Z"
     }'
   ```

   The value is sealed and never read back. The response contains metadata
   only.

3. Observe status:

   ```bash
   curl -H "Authorization: Bearer $OPENWORK_API_TOKEN" \
     https://control.example.invalid/v1/admin/credentials
   ```

   Returns reference, provider, environment variable, scope, owner, creation
   time, optional expiry/disabled time, and the revision. No value is ever
   present.

4. Rotate without reinstalling Apps:

   ```bash
   curl -H "Authorization: Bearer $OPENWORK_API_TOKEN" \
     -X POST https://control.example.invalid/v1/admin/credentials/github.app/rotate \
     -d '{"value": "ghp_…"}'
   ```

5. Revoke (blocks new executions immediately) and re-enable:

   ```bash
   curl -H "Authorization: Bearer $OPENWORK_API_TOKEN" \
     -X POST https://control.example.invalid/v1/admin/credentials/github.app/revoke
   curl -H "Authorization: Bearer $OPENWORK_API_TOKEN" \
     -X POST https://control.example.invalid/v1/admin/credentials/github.app/enable
   ```

The Employee Workspace Administrator → Credentials tab exposes the same
lifecycle (status, add/replace, rotate, revoke, enable) without a secret value
ever reaching the renderer.

## Demo flow

```text
Configure github.app in the Secret Store (Admin API or Workspace)
Restart control API (values are sealed at rest)
Run governed repository read via github.repository.read -> succeeds
Revoke github.app
Run again -> connector fails closed as credential unavailable
Rotate github.app (new value, same reference)
Run again -> succeeds
Meta-data queries, UI, audit, logs, task events: no raw token anywhere
```
