import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import { formatTime, serverErrorText, t } from '../i18n'
import type { CapabilityDescriptor, InvokeResult, WorkspaceApp } from '../types'

interface SchemaProp {
  type?: string
  enum?: unknown[]
  default?: unknown
  minimum?: number
  description?: string
}
interface InputSchema {
  type?: string
  properties?: Record<string, SchemaProp>
  required?: string[]
}

function schemaOf(cap: CapabilityDescriptor): InputSchema {
  const schema = cap.input_schema
  if (schema && typeof schema === 'object' && !Array.isArray(schema)) return schema as InputSchema
  return {}
}

function initParams(schema: InputSchema): Record<string, unknown> {
  // Every declared property gets an editable starting value: explicit default,
  // first enum option, or the type's zero value. Required strings stay empty
  // so the invoke button remains disabled until the operator fills them.
  const params: Record<string, unknown> = {}
  for (const [key, prop] of Object.entries(schema.properties ?? {})) {
    if (prop.default !== undefined) { params[key] = prop.default; continue }
    if (Array.isArray(prop.enum) && prop.enum.length > 0) { params[key] = prop.enum[0]; continue }
    if (prop.type === 'boolean') params[key] = false
    else if (prop.type === 'number' || prop.type === 'integer') params[key] = prop.minimum ?? 0
    else params[key] = ''
  }
  return params
}

export default function Apps({ onShowApprovals }: { onShowApprovals: () => void }): React.JSX.Element {
  const [apps, setApps] = useState<WorkspaceApp[]>([])
  const [error, setError] = useState<string | null>(null)
  const [cap, setCap] = useState<CapabilityDescriptor | null>(null)
  const [selectedAppId, setSelectedAppId] = useState('')
  const [resource, setResource] = useState('workspace')
  const [params, setParams] = useState<Record<string, unknown>>({})
  const [invoking, setInvoking] = useState(false)
  const [result, setResult] = useState<InvokeResult | null>(null)
  const load = useCallback(async () => {
    try { setApps(await api.listApps()); setError(null) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
  }, [])
  useEffect(() => { void load() }, [load])
  const isCreateIssue = cap?.id === 'github.issue.create'
  const schema = cap ? schemaOf(cap) : {}
  const hasSchemaProps = Object.keys(schema.properties ?? {}).length > 0
  const selectCap = (appId: string, item: CapabilityDescriptor): void => {
    setSelectedAppId(appId); setCap(item); setResult(null); setResource('workspace'); setError(null)
    setParams(initParams(schemaOf(item)))
  }
  const setParam = (key: string, value: unknown): void => {
    setParams((current) => ({ ...current, [key]: value }))
  }
  const effectiveResource = (): string => {
    const owner = typeof params.owner === 'string' ? params.owner : ''
    const repo = typeof params.repo === 'string' ? params.repo : ''
    if (isCreateIssue && resource === 'workspace') return `github://${owner}/${repo}/issues`
    return resource
  }
  const missingRequired = schema.required?.filter((key) => {
    const value = params[key]
    return value === undefined || value === '' || value === null
  }) ?? []
  const invoke = async (): Promise<void> => {
    if (!cap) return
    setInvoking(true); setError(null); setResult(null)
    try { setResult(await api.invokeCapability(selectedAppId, cap.id, { resource: effectiveResource(), parameters: params })) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
    finally { setInvoking(false) }
  }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">{t('apps.eyebrow')}</p><h1>{t('apps.title')}</h1></div><button className="quiet" onClick={() => void load()}>{t('app.refresh')}</button></div>
    {error && <p className="notice error">{error}</p>}<div className="connector-layout">
      <div className="list">{apps.length === 0 ? <p className="muted">{t('apps.empty')}</p> : apps.map((item) => <article key={item.id} className="app-card"><div><span className={`status ${item.enabled ? 'succeeded' : ''}`}>{item.enabled ? t('apps.available') : t('apps.unavailable')}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? t('apps.noDescription')}</p></div>
        <div className="actions cap-actions">{(item.capabilities ?? []).map((capability) => <button key={capability.id} className="quiet" onClick={() => selectCap(item.id, capability)}>{capability.id}</button>)}</div>
      </article>)}</div>
      <div className="task-detail"><p className="eyebrow">{t('apps.capabilities')}</p>{cap ? <><h2>{cap.id}</h2><p>{cap.description ?? t('apps.noDescription')}</p>
        <dl><dt>{t('apps.risk')}</dt><dd>{cap.risk ?? '—'}</dd></dl>
        <label>{t('apps.resource')}<input value={resource} onChange={(event) => setResource(event.target.value)} /></label>
        {hasSchemaProps ? Object.entries(schema.properties ?? {}).map(([key, prop]) => {
          const required = (schema.required ?? []).includes(key)
          const label = `${prop.description ?? key}${required ? t('apps.paramRequired') : ''}`
          const value = params[key]
          if (Array.isArray(prop.enum)) {
            return <label key={key}>{label}<select value={typeof value === 'string' ? value : ''} onChange={(event) => setParam(key, event.target.value)}>{prop.enum.map((option) => <option key={String(option)} value={String(option)}>{String(option)}</option>)}</select></label>
          }
          if (prop.type === 'boolean') {
            return <label key={key} className="check"><input type="checkbox" checked={value === true} onChange={(event) => setParam(key, event.target.checked)} />{label}</label>
          }
          if (prop.type === 'number' || prop.type === 'integer') {
            return <label key={key}>{label}<input type="number" value={typeof value === 'number' ? value : ''} onChange={(event) => setParam(key, event.target.value === '' ? undefined : Number(event.target.value))} /></label>
          }
          const long = (key === 'body' || key === 'prompt' || key === 'path' || key === 'query') && prop.type !== 'number'
          return <label key={key}>{label}{long
            ? <textarea value={typeof value === 'string' ? value : ''} onChange={(event) => setParam(key, event.target.value)} />
            : <input value={typeof value === 'string' ? value : ''} onChange={(event) => setParam(key, event.target.value)} />}</label>
        }) : <p className="muted">{t('apps.noParams')}</p>}
        <p className="eyebrow">{t('apps.parameters')}</p>
        <pre>{JSON.stringify(params, null, 2)}</pre>
        <button disabled={invoking || missingRequired.length > 0} onClick={() => void invoke()}>{invoking ? t('apps.invoking') : t('apps.invoke')}</button>
        {result && <div><p className="eyebrow">{t('apps.invokeResult')}</p>
          {result.status === 'completed' ? <p className="status succeeded">{t('apps.invokeCompleted')}</p> : <p className="status awaiting_approval">{t('apps.invokeApprovalRequired')}</p>}
          {result.status === 'approval_required' && <><dl>{result.approval_id && <><dt>{t('apps.approvalId')}</dt><dd>{result.approval_id}</dd></>}{result.expires_at && <><dt>{t('apps.expiresAt')}</dt><dd>{formatTime(result.expires_at)}</dd></>}</dl><button onClick={onShowApprovals}>{t('apps.gotoApprovals')}</button></>}
          {result.status === 'completed' && <pre>{JSON.stringify(result.output, null, 2)}</pre>}
        </div>}
      </> : <p className="muted">{t('apps.selectCapability')}</p>}</div>
    </div></div>
}
