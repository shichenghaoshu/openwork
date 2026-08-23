import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import { formatTime, runStatusText, serverErrorText, t } from '../i18n'
import type { Run, WorkspaceFile } from '../types'

const terminal = new Set(['succeeded', 'failed', 'cancelled', 'timed_out'])
const runtimes: [string, string][] = [['auto', 'auto'], ['claude-code', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['pi', 'Pi']]

export default function Tasks(): React.JSX.Element {
  const [runtime, setRuntime] = useState('auto')
  const [workspace, setWorkspace] = useState('')
  const [prompt, setPrompt] = useState('')
  const [run, setRun] = useState<Run | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [files, setFiles] = useState<WorkspaceFile[]>([])
  const [fileIds, setFileIds] = useState<string[]>([])
  const timer = useRef<number | null>(null)
  const poll = async (id: string): Promise<void> => {
    try { const next = await api.getRun(id); setRun(next); if (terminal.has(next.status) && timer.current) window.clearInterval(timer.current) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
  }
  useEffect(() => {
    void api.listFiles().then(setFiles).catch(() => undefined)
    void api.getSettings().then((settings) => {
      if (typeof settings.default_runtime === 'string' && runtimes.some(([id]) => id === settings.default_runtime)) setRuntime(settings.default_runtime)
    }).catch(() => undefined)
    return () => { if (timer.current) window.clearInterval(timer.current) }
  }, [])
  const create = async (): Promise<void> => {
    setBusy(true); setError(null)
    try { const created = await api.createRun({ runtime, workspace: workspace.trim(), prompt: prompt.trim(), file_ids: fileIds }); setRun(created); if (timer.current) window.clearInterval(timer.current); timer.current = window.setInterval(() => void poll(created.id), 2500) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
    finally { setBusy(false) }
  }
  const cancel = async (): Promise<void> => { if (!run) return; setBusy(true); setError(null); try { await api.cancelRun(run.id); await poll(run.id) } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) } finally { setBusy(false) } }
  return <div className="page">
    <div className="page-heading"><div><p className="eyebrow">{t('tasks.eyebrow')}</p><h1>{t('tasks.title')}</h1></div>{run && <span className={`status ${run.status.toLowerCase()}`}>{runStatusText(run.status)}</span>}</div>
    <div className="workspace-grid"><form onSubmit={(event) => { event.preventDefault(); void create() }}>
      <label>{t('tasks.runtime')}<select value={runtime} onChange={(event) => setRuntime(event.target.value)}>{runtimes.map(([id, name]) => <option key={id} value={id}>{name}</option>)}</select></label>
      <label>{t('tasks.workspace')}<input required value={workspace} onChange={(event) => setWorkspace(event.target.value)} placeholder={t('tasks.workspacePlaceholder')} /></label>
      <label>{t('tasks.prompt')}<textarea required value={prompt} onChange={(event) => setPrompt(event.target.value)} placeholder={t('tasks.promptPlaceholder')} /></label>
      <fieldset className="file-picker"><legend>{t('tasks.files')}</legend>{files.length === 0 ? <span className="muted">{t('tasks.noFiles')}</span> : files.map((file) => <label key={file.id} className="check"><input type="checkbox" checked={fileIds.includes(file.id)} onChange={(event) => setFileIds((ids) => event.target.checked ? [...ids, file.id] : ids.filter((id) => id !== file.id))} />{file.name}</label>)}</fieldset>
      <button disabled={busy || !workspace.trim() || !prompt.trim()}>{busy ? t('tasks.working') : t('tasks.create')}</button>
    </form>
    <div className="task-detail">{error && <p className="notice error">{error}</p>}{run ? <><p className="eyebrow">{t('tasks.detail')}</p><h2>{run.id}</h2><dl><dt>{t('tasks.workspace')}</dt><dd>{run.workspace}</dd><dt>{t('tasks.runtime')}</dt><dd>{run.runtime}</dd><dt>{t('tasks.appsUsed')}</dt><dd>{run.apps_used?.join(', ') || t('tasks.notReported')}</dd><dt>{t('tasks.artifacts')}</dt><dd>{run.artifacts?.map((item) => item.name ?? item.id ?? 'Artifact').join(', ') || t('tasks.notReported')}</dd><dt>{t('tasks.actions')}</dt><dd>{run.actions?.map((item) => item.description ?? item.id ?? item.status ?? 'Action').join(', ') || t('tasks.notReported')}</dd><dt>{t('tasks.approvals')}</dt><dd>{run.approvals?.map((item) => item.status ?? item.id ?? 'Approval').join(', ') || t('tasks.notReported')}</dd><dt>{t('tasks.updated')}</dt><dd>{formatTime(run.updated_at)}</dd>{run.terminal_reason && <><dt>{t('tasks.reason')}</dt><dd>{run.terminal_reason}</dd></>}</dl>{!terminal.has(run.status) && <button className="danger" disabled={busy} onClick={() => void cancel()}>{t('tasks.cancel')}</button>}
        <div className="rule" />
        <p className="eyebrow">{t('tasks.audit')}</p>
        <div className="list">{(run.events ?? []).length === 0 ? <p className="muted">{t('tasks.notReported')}</p> : run.events?.map((event) => <article key={event.id}><div><span className="status">{event.event_type}</span><small>{event.timestamp ? formatTime(event.timestamp) : ''}</small></div></article>)}</div>
      </> : <p className="muted">{t('tasks.noDetail')}</p>}</div>
    </div>
  </div>
}
