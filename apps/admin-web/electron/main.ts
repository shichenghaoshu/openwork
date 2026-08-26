import { app, BrowserWindow, ipcMain } from 'electron'
import path from 'node:path'
import { fileURLToPath } from 'node:url'

const __dirname = path.dirname(fileURLToPath(import.meta.url))

type HttpMethod = 'GET' | 'POST'
interface BridgeResponse {
  ok: boolean
  status: number
  data?: unknown
  error?: string
}

function controlApiConfig(): { baseUrl: URL; token: string } {
  const rawBaseUrl = process.env.OPENWORK_CONTROL_API_BASE_URL
  const token = process.env.OPENWORK_CONTROL_API_TOKEN
  if (!rawBaseUrl || !token) {
    throw new Error('Control API is not configured in the Electron main process.')
  }
  const baseUrl = new URL(rawBaseUrl)
  if (!['http:', 'https:'].includes(baseUrl.protocol)) {
    throw new Error('Control API base URL must use HTTP or HTTPS.')
  }
  return { baseUrl, token }
}

function validId(id: string): boolean {
  return /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(id)
}

function validCredentialRef(id: string): boolean {
  return /^[a-z][a-z0-9._-]{0,127}$/.test(id)
}

async function controlRequest(
  method: HttpMethod,
  endpoint: string,
  body?: unknown,
): Promise<BridgeResponse> {
  try {
    const { baseUrl, token } = controlApiConfig()
    const response = await fetch(new URL(endpoint, baseUrl), {
      method,
      headers: {
        Authorization: `Bearer ${token}`,
        ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    })
    const text = await response.text()
    let data: unknown = undefined
    try {
      data = text.length > 0 ? JSON.parse(text) : undefined
    } catch {
      data = undefined
    }
    if (response.ok) return { ok: true, status: response.status, data }
    const message = typeof data === 'object' && data !== null && 'message' in data
      ? String(data.message)
      : text || `Control API request failed (${response.status})`
    return { ok: false, status: response.status, error: message }
  } catch (error) {
    return { ok: false, status: 0, error: error instanceof Error ? error.message : String(error) }
  }
}

/** Read-only text endpoint for artifact previews. Raw bytes are never parsed
 * as JSON; the renderer receives the text as `data`. */
async function controlRequestText(
  method: HttpMethod,
  endpoint: string,
): Promise<BridgeResponse> {
  try {
    const { baseUrl, token } = controlApiConfig()
    const response = await fetch(new URL(endpoint, baseUrl), {
      method,
      headers: { Authorization: `Bearer ${token}` },
    })
    const text = await response.text()
    if (response.ok) return { ok: true, status: response.status, data: text }
    return { ok: false, status: response.status, error: text || `Control API request failed (${response.status})` }
  } catch (error) {
    return { ok: false, status: 0, error: error instanceof Error ? error.message : String(error) }
  }
}

async function uploadFile(
  name: string,
  mimeType: string,
  bytes: ArrayBuffer,
): Promise<BridgeResponse> {
  if (!/^[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}$/.test(name) || bytes.byteLength === 0 || bytes.byteLength > 8 * 1024 * 1024) {
    return { ok: false, status: 400, error: 'Choose a non-empty file smaller than 8 MB with a portable filename.' }
  }
  try {
    const { baseUrl, token } = controlApiConfig()
    const response = await fetch(new URL('/v1/files', baseUrl), {
      method: 'POST',
      headers: {
        Authorization: `Bearer ${token}`,
        'Content-Type': mimeType || 'application/octet-stream',
        'x-openwork-filename': name,
      },
      body: bytes,
    })
    const text = await response.text()
    let data: unknown
    try { data = text ? JSON.parse(text) : undefined } catch { data = undefined }
    if (response.ok) return { ok: true, status: response.status, data }
    const message = typeof data === 'object' && data !== null && 'message' in data
      ? String(data.message) : text || `Control API request failed (${response.status})`
    return { ok: false, status: response.status, error: message }
  } catch (error) {
    return { ok: false, status: 0, error: error instanceof Error ? error.message : String(error) }
  }
}

function createWindow(): void {
  const window = new BrowserWindow({
    width: 1280,
    height: 860,
    minWidth: 960,
    minHeight: 640,
    backgroundColor: '#f6f8f7',
    title: 'Employee Workspace',
    webPreferences: {
      preload: path.join(__dirname, 'preload.cjs'),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
    },
  })
  const devUrl = process.env.VITE_DEV_SERVER_URL
  if (devUrl) void window.loadURL(devUrl)
  else void window.loadFile(path.join(__dirname, '../dist/index.html'))
}

function registerIpc(): void {
  ipcMain.handle('control:health', () => controlRequest('GET', '/health'))
  ipcMain.handle('control:createRun', (_event, body: { runtime: string; workspace: string; prompt: string; file_ids?: string[] }) =>
    controlRequest('POST', '/v1/runs', body),
  )
  ipcMain.handle('control:getRun', (_event, id: string) =>
    validId(id) ? controlRequest('GET', `/v1/runs/${id}`) : Promise.resolve({ ok: false, status: 400, error: 'Invalid run ID.' }),
  )
  ipcMain.handle('control:listRunArtifacts', (_event, id: string) =>
    validId(id) ? controlRequest('GET', `/v1/runs/${id}/artifacts`) : Promise.resolve({ ok: false, status: 400, error: 'Invalid run ID.' }),
  )
  ipcMain.handle('control:artifactContent', (_event, runId: string, artifactId: string) =>
    validId(runId) && validId(artifactId)
      ? controlRequestText('GET', `/v1/runs/${runId}/artifacts/${artifactId}/content`)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid artifact ID.' }),
  )
  ipcMain.handle('control:listRunEvents', (_event, id: string) =>
    validId(id) ? controlRequest('GET', `/v1/runs/${id}/events`) : Promise.resolve({ ok: false, status: 400, error: 'Invalid run ID.' }),
  )
  ipcMain.handle('control:cancelRun', (_event, id: string) =>
    validId(id) ? controlRequest('POST', `/v1/runs/${id}/cancel`) : Promise.resolve({ ok: false, status: 400, error: 'Invalid run ID.' }),
  )
  ipcMain.handle('control:listApprovals', () => controlRequest('GET', '/v1/approvals'))
  ipcMain.handle('control:decideApproval', (_event, id: string, body: { expected_revision: number; reason?: string }, decision: 'approve' | 'deny') =>
    validId(id)
      ? controlRequest('POST', `/v1/approvals/${id}/${decision}`, body)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid approval ID.' }),
  )
  // These routes are intentionally typed and separately allowlisted. Older
  // control planes may return 404; the renderer shows that server response.
  ipcMain.handle('control:listConnectors', () => controlRequest('GET', '/v1/connectors'))
  ipcMain.handle('control:listConnectorTools', (_event, id: string) =>
    encodeURIComponent(id) === id && id.length > 0
      ? controlRequest('GET', `/v1/connectors/${encodeURIComponent(id)}/tools`)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid connector ID.' }),
  )
  ipcMain.handle('control:listFiles', () => controlRequest('GET', '/v1/files'))
  ipcMain.handle('control:uploadFile', (_event, name: string, mimeType: string, bytes: ArrayBuffer) =>
    uploadFile(name, mimeType, bytes),
  )
  ipcMain.handle('control:listApps', () => controlRequest('GET', '/v1/apps'))
  ipcMain.handle('control:listStoreApps', () => controlRequest('GET', '/v1/apps'))
  ipcMain.handle('control:installApp', (_event, id: string) =>
    encodeURIComponent(id) === id && id.length > 0
      ? controlRequest('POST', `/v1/apps/${encodeURIComponent(id)}/install`)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid app ID.' }),
  )
  ipcMain.handle('control:setAppEnabled', (_event, id: string, enabled: boolean) =>
    encodeURIComponent(id) === id && id.length > 0
      ? controlRequest('POST', `/v1/apps/${encodeURIComponent(id)}/${enabled ? 'enable' : 'disable'}`)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid app ID.' }),
  )
  ipcMain.handle('control:listRuntimes', () => controlRequest('GET', '/v1/admin/runtimes'))
  ipcMain.handle('control:getSettings', () => controlRequest('GET', '/v1/admin/settings'))
  ipcMain.handle('control:updateSettings', (_event, body: { default_runtime: string }) =>
    controlRequest('POST', '/v1/admin/settings', body),
  )
  // Credential administration is metadata-only: the value is never read back
  // and every renderer-visible response is redacted by the Control API.
  ipcMain.handle('control:listCredentials', () => controlRequest('GET', '/v1/admin/credentials'))
  ipcMain.handle('control:createCredential', (_event, body: { credential_ref: string; provider: string; environment_variable: string; scope: string; owner: string; value: string; expires_at?: string }) =>
    validCredentialRef(body.credential_ref)
      ? controlRequest('POST', '/v1/admin/credentials', body)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid credential reference.' }),
  )
  ipcMain.handle('control:rotateCredential', (_event, id: string, value: string) =>
    validCredentialRef(id) && typeof value === 'string' && value.length > 0
      ? controlRequest('POST', `/v1/admin/credentials/${encodeURIComponent(id)}/rotate`, { value })
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid credential reference.' }),
  )
  ipcMain.handle('control:revokeCredential', (_event, id: string) =>
    validCredentialRef(id)
      ? controlRequest('POST', `/v1/admin/credentials/${encodeURIComponent(id)}/revoke`)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid credential reference.' }),
  )
  ipcMain.handle('control:enableCredential', (_event, id: string) =>
    validCredentialRef(id)
      ? controlRequest('POST', `/v1/admin/credentials/${encodeURIComponent(id)}/enable`)
      : Promise.resolve({ ok: false, status: 400, error: 'Invalid credential reference.' }),
  )
}

app.whenReady().then(() => {
  registerIpc()
  createWindow()
  app.on('activate', () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow()
  })
})

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') app.quit()
})
