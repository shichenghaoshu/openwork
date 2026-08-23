export type RunStatus = 'queued' | 'planning' | 'awaiting_approval' | 'running' | 'succeeded' | 'failed' | 'cancelled' | 'timed_out'

export interface Run {
  id: string
  runtime: string
  workspace: string
  status: RunStatus
  revision: number
  created_at: string
  updated_at: string
  terminal_reason?: string | null
  apps_used?: string[]
  artifacts?: Array<{ id?: string; name?: string; [key: string]: unknown }>
  actions?: Array<{ id?: string; status?: string; description?: string; [key: string]: unknown }>
  approvals?: Array<{ id?: string; status?: string; [key: string]: unknown }>
  events?: AuditEventRow[]
}

export interface Approval {
  id: string
  run_id: string
  action_id: string
  requested_by: string
  request_reason: string
  created_at: string
  expires_at: string
  status: string
  revision: number
  decision?: { decision: string; actor: string; reason?: string | null; decided_at: string } | null
}

export interface Connector {
  id: string
  name?: string
  status?: string
  [key: string]: unknown
}

export interface ConnectorTool {
  id: string
  name?: string
  description?: string
  [key: string]: unknown
}

export interface WorkspaceFile {
  id: string
  name: string
  content_type?: string
  size_bytes?: number
  created_at?: string
  [key: string]: unknown
}

export interface WorkspaceApp {
  id: string
  name?: string
  description?: string
  enabled?: boolean
  installed?: boolean
  capabilities?: CapabilityDescriptor[]
  [key: string]: unknown
}

export interface CapabilityDescriptor {
  id: string
  description?: string
  risk?: string
  input_schema?: Record<string, unknown>
  provider?: unknown
  [key: string]: unknown
}

export interface InvokeResult {
  status: 'completed' | 'approval_required'
  app_id: string
  capability: string
  driver?: string
  risk?: string
  run_id?: string
  approval_id?: string
  approval_revision?: number
  expires_at?: string
  output?: unknown
}

export interface SalesDemoReport {
  run_id?: string
  status?: string
  revision?: number
  artifacts?: Array<{ path?: string; media_type?: string; size_bytes?: number; sha256?: string }>
  audit_events?: Array<{ sequence?: number; event_type?: string; event_hash?: string }>
  output_directory?: string
  [key: string]: unknown
}

export interface AuditEventRow {
  id: string
  event_type: string
  sequence?: number
  actor?: string
  timestamp?: string
  previous_hash?: string
  event_hash?: string
  metadata?: Record<string, unknown>
}

export interface RuntimeInfo {
  id: string
  name?: string
  status?: 'ready' | 'configured' | 'not_configured'
  description?: string
  [key: string]: unknown
}
