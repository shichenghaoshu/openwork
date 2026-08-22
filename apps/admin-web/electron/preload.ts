import { contextBridge, ipcRenderer } from 'electron'

export interface BridgeResponse {
  ok: boolean
  status: number
  data?: unknown
  error?: string
}

const api = {
  health: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:health'),
  createRun: (body: { runtime: string; workspace: string; prompt: string; file_ids?: string[] }): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:createRun', body),
  getRun: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:getRun', id),
  listRunArtifacts: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:listRunArtifacts', id),
  listRunEvents: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:listRunEvents', id),
  cancelRun: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:cancelRun', id),
  listApprovals: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listApprovals'),
  decideApproval: (id: string, body: { expected_revision: number; reason?: string }, decision: 'approve' | 'deny'): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:decideApproval', id, body, decision),
  listConnectors: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listConnectors'),
  listConnectorTools: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:listConnectorTools', id),
  listFiles: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listFiles'),
  uploadFile: (name: string, mimeType: string, bytes: ArrayBuffer): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:uploadFile', name, mimeType, bytes),
  listApps: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listApps'),
  listStoreApps: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listStoreApps'),
  installApp: (id: string): Promise<BridgeResponse> => ipcRenderer.invoke('control:installApp', id),
  setAppEnabled: (id: string, enabled: boolean): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:setAppEnabled', id, enabled),
  listRuntimes: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:listRuntimes'),
  getSettings: (): Promise<BridgeResponse> => ipcRenderer.invoke('control:getSettings'),
  updateSettings: (body: { default_runtime: string }): Promise<BridgeResponse> =>
    ipcRenderer.invoke('control:updateSettings', body),
}

export type OpenworkApi = typeof api
contextBridge.exposeInMainWorld('openwork', api)
