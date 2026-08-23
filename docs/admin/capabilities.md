# M1 managed-action policy

OpenWork evaluates only actions executed by an OpenWork-managed adapter. The
M1 gateway does not claim to intercept arbitrary provider-native tools, model
syscalls, or activity outside the sandbox boundary, and it never executes an
action itself.

## Configuration

Policy is a versioned YAML document. Unknown fields, duplicate mappings,
unsupported versions, malformed rules, and unknown actions fail closed.

```yaml
version: 1
defaults:
  unknown: deny
actions:
  filesystem.read:
    risk: L0
    decision: allow
  filesystem.write:
    risk: L1
    decision: allow
    resources:
      exact:
        /workspace/protected.txt: deny
      default: allow
  email.send:
    risk: L3
    decision: approval
  database.delete:
    risk: L4
    decision: deny
```

An action rule derives both risk and decision. Request parameters cannot lower
risk. M1 permits `allow` for L0/L1 only, requires approval or denial for L2/L3,
and requires denial for L4. Unknown actions are treated as L4 and denied.

Resource matching is case-sensitive and exact. When an `exact` key does not
match, the resource rule's explicit `default` applies. Rules without a resource
section use the action-level decision.

`REQUIRE_APPROVAL` evaluations carry the exact canonical parameter hash from
the frozen action contract. Any action, resource, or integer-only parameter
change produces a different binding and requires reevaluation.

## Managed-action dispatch (v0.1)

The control plane dispatches exactly these reviewed capability actions; every
other invocation fails closed with `capability_dispatch_unavailable`:

- GitHub MCP read mappings through the digest-pinned official `github-mcp-server`
  container (`repository.read`, `file.read`, `issue.read`, `actions.read`,
  `pull_request.read`), with `github.issue.create` (L3) gated behind the
  single-use approval claim described above;
- the `LibreOffice` `office_bundle.create` demo path (L1, local write) through
  the in-process real-container harness also used by the gated e2e test, which
  requires `OPENWORK_LIBREOFFICE_IMAGE` and returns `demo_not_configured` when
  it is absent;
- connector tool discovery for the GitHub MCP container and the Feishu read-only
  allowlist (`/v1/connectors/{id}/tools`). Feishu business invocation is not
  routed in v0.1.

The CLI provider accepts only a gateway authorization with parameters on
bounded stdin, network disabled, and an exact single-run sandbox; it never
exposes a host shell path.
