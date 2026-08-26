# Durable encrypted prompt recovery

Prompt handoff used to be one-time and process-local: a Control API or worker
restart before an undelivered prompt was consumed failed the run closed. That
is still fail-closed, but a deployment can now opt into durable encrypted
prompt recovery so queued work survives service restarts without plaintext
reaching databases, logs, audit metadata, process argv, or the renderer.

## Contract

A `prompt_envelopes` row binds:

- the run ID (primary key, one envelope per run),
- the prompt SHA-256 that is also stored on the `runs` row,
- the key identifier used by the authenticated cipher,
- the nonce and ciphertext (never the plaintext),
- creation time and a 24-hour expiry window.

The ciphertext is bound through additional authenticated data to the run ID and
the prompt digest, so a sealed envelope cannot be replayed for another run or
another prompt. Decryption happens only inside the trusted worker boundary,
immediately before provider stdin delivery. Successful single-use delivery
atomically deletes the envelope; replay is impossible.

## The key-provider boundary

`openwork-core::crypto::KeyProvider` resolves a 32-byte key by stable
identifier. The default `EnvironmentKeyProvider` reads
`OPENWORK_CRYPTO_KEY_PROMPT_ENVELOPE_V1` (64 hex characters). A KMS-backed
provider can replace it without changing the envelope contract.

## Enabling durable delivery

```bash
export OPENWORK_PROMPT_DELIVERY=durable
export OPENWORK_CRYPTO_KEY_PROMPT_ENVELOPE_V1="$(openssl rand -hex 32)"
```

On startup the Control API verifies the envelope key is resolvable; a missing
key is a configuration error and the service refuses to start, so a deployment
can never silently regress to process-local handoff. The default mode remains
`memory` for local development and tests.

In `compose/compose.yaml`:

```yaml
environment:
  OPENWORK_PROMPT_DELIVERY: durable
  OPENWORK_CRYPTO_KEY_PROMPT_ENVELOPE_V1: ${OPENWORK_CRYPTO_KEY_PROMPT_ENVELOPE_V1:?set the envelope key}
```

## Failure model

| Condition | Behavior |
|---|---|
| Service restart before claim | A new broker instance decrypts the same envelope from Postgres and delivers the prompt. |
| Lease expired / run interrupted | Existing fail-closed recovery applies; the envelope is removed or expires. |
| Expired envelope | `take_prompt` fails closed as unavailable, removes the envelope, and appends `prompt_envelope_expired`. |
| Corrupt ciphertext / wrong key | `take_prompt` fails closed as `corrupt`, appends `prompt_envelope_corrupt`, and never returns prompt bytes. |
| Digest mismatch | Rejected as a binding violation; the envelope stays for diagnosis. |
| Concurrent consumption | Exactly one worker wins; the loser fails closed as unavailable. |
| Duplicate publication | Rejected (`duplicate`); the original envelope is untouched. |

Envelope lifecycle is observable through the audit hash chain: `prompt_envelope_stored`,
`prompt_envelope_consumed`, `prompt_envelope_expired`, and `prompt_envelope_corrupt`
events carry no prompt material — only the event type and safe identifiers.

## Restart demo

```text
Create a task with a prompt
Stop Control API + worker before claim
Restart services
Worker claims the same run through the envelope
Provider receives the prompt on stdin
Run completes
Database rows, API, audit, logs: no plaintext prompt anywhere
```
