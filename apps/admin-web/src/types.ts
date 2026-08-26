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
  [key: string]: unknown
}

export interface RuntimeInfo {
  id: string
  name?: string
  status?: string
  description?: string
  [key: string]: unknown
}

export interface CredentialMetadata {
  credential_ref: string
  provider: string
  environment_variable: string
  scope: string
  owner: string
  created_at: string
  expires_at?: string | null
  disabled_at?: string | null
  revision: number
}
