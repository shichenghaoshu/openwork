# Employee Workspace

Employee Workspace is the OpenWork desktop interface for the authenticated
Control API. It provides four operational views: Workspace, Tasks, Approvals,
and Connectors.

## Security boundary

The renderer never receives a Control API URL or bearer token. Electron's main
process reads both from its environment and exposes only a fixed IPC allowlist:

- task creation, status polling, and cancellation;
- approval listing and approve/deny decisions;
- connector and connector-tool reads;
- unauthenticated health checks.

There is no CLI bridge, shell execution, filesystem access, or arbitrary HTTP
method/path IPC channel. Set these variables in the process that starts
Electron:

```bash
OPENWORK_CONTROL_API_BASE_URL=http://127.0.0.1:8080
OPENWORK_CONTROL_API_TOKEN=replace-with-the-control-api-token
```

The connector screens call `/v1/connectors` and `/v1/connectors/:id/tools`.
The Control API caches successful MCP tool discovery for 60 seconds and failed
probes for 15 seconds, so refreshing the interface does not repeatedly call an
upstream service. Connector execution and credentials stay in the Control API
process; the renderer receives only redacted tool metadata.

## Development

```bash
npm install
npm run dev
npm run build
```

The production bundle is generated locally by Vite and remains ignored by git.
