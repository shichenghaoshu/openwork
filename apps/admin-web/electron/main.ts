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
  ipcMain.handle('control:createRun', (_event, body: { runtime: string; workspace: string; prompt: string }) =>
    controlRequest('POST', '/v1/runs', body),
  )
  ipcMain.handle('control:getRun', (_event, id: string) =>
    validId(id) ? controlRequest('GET', `/v1/runs/${id}`) : Promise.resolve({ ok: false, status: 400, error: 'Invalid run ID.' }),
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
