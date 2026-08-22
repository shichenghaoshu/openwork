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
