# OpenWork-controlled MCP gateway boundary

Status: Accepted; read-only discovery implemented, governed execution pending

## Context

External agent runtimes can discover and call MCP tools, but an enterprise must
not delegate identity, policy, approval, credentials, or audit decisions to the
runtime. Implementing connectors before the M1 execution path is repeatable
would create a second, ungoverned side-effect path.

## Decision

MCP servers sit behind an OpenWork-controlled gateway. A runtime submits a tool
request to the gateway; OpenWork derives the authenticated actor, evaluates the
same action policy used by the Action Gateway, obtains an exact-bound approval
when required, brokers credentials only for the authorized call, executes the
connector, and records a redacted result and audit event.

The v1 design has four provider-neutral contracts:

- `McpToolDescriptor`: stable server/tool identity, input schema digest,
  declared action/resource mapping, risk ceiling, and required capabilities.
- `McpToolRequest`: run and action IDs, descriptor identity, canonical input,
  canonical input hash, requested resource, and trusted actor context supplied
  by the gateway rather than the runtime payload.
- `McpExecutionPolicy`: allow, deny, or require-approval decision plus the exact
  action/resource/parameter binding, expiry, and policy revision.
- `McpExecutionResult`: request and claim IDs, bounded redacted status metadata,
  artifact references, external receipt digest, and timestamps. It never
  contains credentials or arbitrary provider response bodies in audit storage.

The gateway must consume a valid single-use `ActionClaim` before any L3 side
effect. L4 and unknown tools fail closed. Connectors receive short-lived scoped
credentials after authorization; agent runtimes never receive the underlying
credential. Transport and connector adapters may be replaced without changing
policy or approval semantics.

The first implementation milestone is deliberately narrower than tool
execution. The Control API owns stdio MCP processes, forwards credentials only
in their environment, performs `initialize` and `tools/list`, and returns a
redacted catalog containing schema digests. GitHub and Feishu/Lark definitions
are pinned and configured read-only. Successful discovery is cached for 60
seconds and failures for 15 seconds.

No MCP `tools/call` route is exposed yet. Tool execution remains blocked until
the request can consume an exact `ActionClaim` and produce the policy, approval,
credential, result-redaction, and audit evidence required above. Agent runtimes
therefore still cannot access MCP credentials or invoke providers directly.

## Consequences

Claude, Codex, Goose, and future runtimes use one governed tool path. Feishu,
WeCom, GitHub, Postgres, ERP, CRM, and Google Workspace remain replaceable
connectors rather than privileged runtime plugins. v0.2 must version wire
schemas before enabling a connector.

## Alternatives

Direct runtime-to-MCP access was rejected because it bypasses OpenWork approval,
credential, and audit controls. Building a marketplace in M1 was rejected as
scope expansion.

## Security implications

The gateway rejects duplicate JSON keys, non-canonical or oversized input,
descriptor drift, parameter/resource/action mutation, expired claims, replay,
unknown tools, and credential requests broader than the approved descriptor.
Secrets, authorization headers, raw prompts, private reasoning, and unbounded
tool responses are excluded from logs and audit events.

## License implications

Each connector requires its own upstream, license, and distribution review.
The first definitions invoke the official MIT-licensed GitHub MCP Server and
Feishu/Lark OpenAPI MCP packages out of process; neither is linked into an
OpenWork binary.

## Revisit trigger

Revisit before exposing the first MCP `tools/call` route or adding a durable
credential broker.
