import { contextBridge, ipcRenderer } from 'electron'

export interface BridgeResponse {
  ok: boolean
  status: number
  data?: unknown
  error?: string
}

const api = {
  health: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:health'),
  createRun: (body: { runtime: string; workspace: string; prompt: string }): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:createRun', body),
  getRun: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:getRun', id),
  cancelRun: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:cancelRun', id),
  listApprovals: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listApprovals'),
  decideApproval: (id: string, body: { expected_revision: number; reason?: string }, decision: 'approve' | 'deny'): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:decideApproval', id, body, decision),
  listConnectors: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listConnectors'),
  listConnectorTools: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:listConnectorTools', id),
}

export type OpenworkApi = typeof api
contextBridge.exposeInMainWorld('openwork', api)
