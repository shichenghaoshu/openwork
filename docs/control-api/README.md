# M1 Control API

The server defaults to `127.0.0.1:8080`. `/health` is anonymous; every `/v1`
route requires `Authorization: Bearer <token>`. The M1 bootstrap uses one token
and one configured actor identity per installation. The request body can never
override that actor.

## Start the control plane

Copy `compose/.env.example` to `compose/.env`, replace every placeholder with a
long random value, then run:

```sh
docker compose --env-file compose/.env -f compose/compose.yaml up --build
```

Postgres has no host port. The Control API is published on loopback only. The
optional model gateway is started separately and is not required by Claude Code
or Codex:

```sh
docker compose --env-file compose/.env -f compose/compose.yaml --profile gateway up --build
```

Set `LITELLM_MASTER_KEY` before enabling that profile. Use a URL-safe database
password because Compose interpolates it into `OPENWORK_DATABASE_URL`.

## Current integration boundary

Read routes are backed by Postgres migrations in `migrations/`. Mutation routes
authenticate and validate their versioned payloads, but run create/cancel and
approval decisions return `503` without changing state until the execution and
policy services provide their required atomic transactions. In particular:

- create must write `run.created` as audit sequence 1 in the same transaction;
- cancel must stop Runtime and Sandbox before writing a terminal state;
- approval decisions must compare revision and persist the audit event in one
  transaction.

This fail-closed behavior prevents a queued/cancelled/approved API response from
claiming work that has not actually crossed its security boundary.

The OpenAPI source of truth is `contracts/openapi/control-api.v1.yaml`. Public
payloads never contain host mount paths, raw prompts after creation, artifact
contents, credentials, or unredacted audit metadata.
