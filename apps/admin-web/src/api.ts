import type { OpenworkApi, BridgeResponse } from '../electron/preload'
import type { Approval, Connector, ConnectorTool, Run, RuntimeInfo, WorkspaceApp, WorkspaceFile } from './types'

declare global {
  interface Window { openwork: OpenworkApi }
}

function unwrap<T>(response: BridgeResponse): T {
  if (!response.ok) throw new Error(response.error ?? `Request failed (${response.status})`)
  return response.data as T
}

interface FileWire { id: string; name: string; bytes: number; sha256: string }
interface AppWire {
  app: { metadata: { id: string; name: string; description: string }; capabilities?: unknown[] }
  installed: boolean
  enabled: boolean
}
interface RuntimeWire { id: string; configured: boolean; worker_ready: boolean }
interface ArtifactWire { id: string; path: string; media_type: string; size_bytes: number; [key: string]: unknown }
interface AuditEventWire { id: string; event_type: string; metadata?: Record<string, unknown>; [key: string]: unknown }

function normalizeFile(item: FileWire): WorkspaceFile {
  return { id: item.id, name: item.name, size_bytes: item.bytes, sha256: item.sha256 }
}

function normalizeApp(item: AppWire): WorkspaceApp {
  return {
    id: item.app.metadata.id,
    name: item.app.metadata.name,
    description: item.app.metadata.description,
    installed: item.installed,
    enabled: item.enabled,
    capabilities: item.app.capabilities ?? [],
  }
}

function normalizeRuntime(item: RuntimeWire): RuntimeInfo {
  return { id: item.id, name: item.id, status: item.worker_ready ? 'Ready' : item.configured ? 'Configured' : 'Not configured' }
}

function appNameFromCapability(value: unknown): string | null {
  if (typeof value !== 'string') return null
  if (value.startsWith('github.')) return 'GitHub'
  if (value.startsWith('libreoffice.')) return 'LibreOffice'
  if (value.startsWith('feishu.')) return 'Feishu'
  return null
}

async function getRunDetail(id: string): Promise<Run> {
  const [run, artifacts, events, approvals] = await Promise.all([
    window.openwork.getRun(id),
    window.openwork.listRunArtifacts(id),
    window.openwork.listRunEvents(id),
    window.openwork.listApprovals(),
  ])
  const base = unwrap<Run>(run)
  const artifactItems = unwrap<ArtifactWire[]>(artifacts)
  const eventItems = unwrap<AuditEventWire[]>(events)
  const approvalItems = unwrap<Approval[]>(approvals).filter((item) => item.run_id === id)
  const actions = eventItems
    .filter((item) => item.event_type === 'action_requested' || item.event_type === 'action_executed')
    .map((item) => ({ id: item.id, status: item.event_type, description: typeof item.metadata?.capability === 'string' ? item.metadata.capability : item.event_type }))
  const appsUsed = [...new Set(eventItems.map((item) => appNameFromCapability(item.metadata?.capability)).filter((item): item is string => item !== null))]
  return {
    ...base,
    apps_used: appsUsed,
    artifacts: artifactItems.map((item) => ({ ...item, name: item.path.split('/').pop() || item.path })),
    actions,
    approvals: approvalItems.map((item) => ({ id: item.id, status: item.status })),
  }
}

export const api = {
  health: async (): Promise<{ status: string; postgres: string }> => unwrap(await window.openwork.health()),
  createRun: async (body: { runtime: string; workspace: string; prompt: string; file_ids?: string[] }): Promise<Run> => unwrap(await window.openwork.createRun(body)),
  getRun: getRunDetail,
  cancelRun: async (id: string): Promise<{ status: string; confirmed: boolean }> => unwrap(await window.openwork.cancelRun(id)),
  listApprovals: async (): Promise<Approval[]> => unwrap(await window.openwork.listApprovals()),
  decideApproval: async (id: string, expected_revision: number, decision: 'approve' | 'deny', reason?: string): Promise<Approval> =>
    unwrap(await window.openwork.decideApproval(id, { expected_revision, reason: reason || undefined }, decision)),
  listConnectors: async (): Promise<Connector[]> => unwrap(await window.openwork.listConnectors()),
  listConnectorTools: async (id: string): Promise<ConnectorTool[]> => unwrap(await window.openwork.listConnectorTools(id)),
  listFiles: async (): Promise<WorkspaceFile[]> => unwrap<FileWire[]>(await window.openwork.listFiles()).map(normalizeFile),
  uploadFile: async (file: File): Promise<WorkspaceFile> => normalizeFile(unwrap<FileWire>(await window.openwork.uploadFile(file.name, file.type, await file.arrayBuffer()))),
  listApps: async (): Promise<WorkspaceApp[]> => unwrap<AppWire[]>(await window.openwork.listApps()).map(normalizeApp),
  listStoreApps: async (): Promise<WorkspaceApp[]> => unwrap<AppWire[]>(await window.openwork.listStoreApps()).map(normalizeApp),
  installApp: async (id: string): Promise<WorkspaceApp> => ({ id, ...unwrap<{ installed: boolean; enabled: boolean }>(await window.openwork.installApp(id)) }),
  setAppEnabled: async (id: string, enabled: boolean): Promise<WorkspaceApp> => ({ id, ...unwrap<{ installed: boolean; enabled: boolean }>(await window.openwork.setAppEnabled(id, enabled)) }),
  listRuntimes: async (): Promise<RuntimeInfo[]> => unwrap<RuntimeWire[]>(await window.openwork.listRuntimes()).map(normalizeRuntime),
  getSettings: async (): Promise<Record<string, unknown>> => unwrap(await window.openwork.getSettings()),
  updateSettings: async (default_runtime: string): Promise<Record<string, unknown>> =>
    unwrap(await window.openwork.updateSettings({ default_runtime })),
}
