import type { OpenworkApi, BridgeResponse } from '../electron/preload'
import type { Approval, Connector, ConnectorTool, Run } from './types'

declare global {
  interface Window { openwork: OpenworkApi }
}

function unwrap<T>(response: BridgeResponse): T {
  if (!response.ok) throw new Error(response.error ?? `Request failed (${response.status})`)
  return response.data as T
}

export const api = {
  health: async (): Promise<{ status: string; postgres: string }> => unwrap(await window.openwork.health()),
  createRun: async (body: { runtime: string; workspace: string; prompt: string }): Promise<Run> => unwrap(await window.openwork.createRun(body)),
  getRun: async (id: string): Promise<Run> => unwrap(await window.openwork.getRun(id)),
  cancelRun: async (id: string): Promise<{ status: string; confirmed: boolean }> => unwrap(await window.openwork.cancelRun(id)),
  listApprovals: async (): Promise<Approval[]> => unwrap(await window.openwork.listApprovals()),
  decideApproval: async (id: string, expected_revision: number, decision: 'approve' | 'deny', reason?: string): Promise<Approval> =>
    unwrap(await window.openwork.decideApproval(id, { expected_revision, reason: reason || undefined }, decision)),
  listConnectors: async (): Promise<Connector[]> => unwrap(await window.openwork.listConnectors()),
  listConnectorTools: async (id: string): Promise<ConnectorTool[]> => unwrap(await window.openwork.listConnectorTools(id)),
}
