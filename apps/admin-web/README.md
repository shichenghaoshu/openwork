# Employee Workspace

Employee Workspace is the OpenWork desktop interface for the authenticated
Control API. It provides employee views for Workspace, Tasks, Files, Apps,
Approvals, and Connectors, plus administrator-only views for AI Runtimes, the
App Store, and Settings. The interface is bilingual (zh-CN and English) with a
language switch in the header and the preference stored locally.

## Security boundary

The renderer never receives a Control API URL or bearer token. Electron's main
process reads both from its environment and exposes only a fixed IPC allowlist:

- task creation, status polling, and cancellation;
- approval listing, approve/deny, and execute decisions;
- file listing, upload, and metadata reads;
- employee app listing, administrator app management, and reviewed capability
  invocation with schema-generated parameter forms;
- connector status and tool discovery reads;
- the real-container sales demo run;
- administrator runtime and settings reads;
- unauthenticated health checks.

There is no CLI bridge, shell execution, filesystem access, or arbitrary HTTP
method/path IPC channel. Set these variables in the process that starts
Electron:

```bash
OPENWORK_CONTROL_API_BASE_URL=http://127.0.0.1:8080
OPENWORK_CONTROL_API_TOKEN=replace-with-the-control-api-token
```

The desktop bridge uses a fixed route allowlist. In addition to task and
approval routes, it calls `/v1/files`, `/v1/apps`, `/v1/connectors`,
`/v1/apps/{id}/capabilities/{capability}/invoke`, `/v1/approvals/{id}/execute`,
`/v1/admin/demo-sales`, and the corresponding `/v1/admin/*` routes. A server
that has not implemented one of those routes returns its real error to the
employee; the desktop app does not substitute sample data.

An enabled runtime worker also requires `OPENWORK_PROVIDER_NETWORK`. This must
name a separately provisioned container network whose egress policy permits
only the configured AI provider endpoints; OpenWork does not fall back to the
Docker default bridge or host networking.

## Development

```bash
npm install
npm run dev
npm run build
```

The production bundle is generated locally by Vite and remains ignored by git.
