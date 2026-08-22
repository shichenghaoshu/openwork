import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { Run } from '../types'

const terminal = new Set(['Succeeded', 'Failed', 'Cancelled', 'TimedOut'])

export default function Tasks(): React.JSX.Element {
  const [runtime, setRuntime] = useState('codex')
  const [workspace, setWorkspace] = useState('')
  const [prompt, setPrompt] = useState('')
  const [run, setRun] = useState<Run | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const timer = useRef<number | null>(null)
  const poll = async (id: string): Promise<void> => {
    try { const next = await api.getRun(id); setRun(next); if (terminal.has(next.status) && timer.current) window.clearInterval(timer.current) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
  }
  useEffect(() => () => { if (timer.current) window.clearInterval(timer.current) }, [])
  const create = async (): Promise<void> => {
    setBusy(true); setError(null)
    try { const created = await api.createRun({ runtime, workspace: workspace.trim(), prompt: prompt.trim() }); setRun(created); if (timer.current) window.clearInterval(timer.current); timer.current = window.setInterval(() => void poll(created.id), 2500) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  const cancel = async (): Promise<void> => { if (!run) return; setBusy(true); setError(null); try { await api.cancelRun(run.id); await poll(run.id) } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } finally { setBusy(false) } }
  return <div className="page">
    <div className="page-heading"><div><p className="eyebrow">Controlled execution</p><h1>Tasks</h1></div>{run && <span className={`status ${run.status.toLowerCase()}`}>{run.status}</span>}</div>
    <div className="workspace-grid"><form onSubmit={(event) => { event.preventDefault(); void create() }}>
      <label>Runtime<select value={runtime} onChange={(event) => setRuntime(event.target.value)}><option value="codex">Codex</option><option value="claude-code">Claude Code</option></select></label>
      <label>Workspace ID<input required value={workspace} onChange={(event) => setWorkspace(event.target.value)} placeholder="team/project" /></label>
      <label>Task prompt<textarea required value={prompt} onChange={(event) => setPrompt(event.target.value)} placeholder="Describe the task for the controlled worker." /></label>
      <button disabled={busy || !workspace.trim() || !prompt.trim()}>{busy ? 'Working…' : 'Create task'}</button>
    </form>
    <div className="task-detail">{error && <p className="notice error">{error}</p>}{run ? <><p className="eyebrow">Live task</p><h2>{run.id}</h2><dl><dt>Workspace</dt><dd>{run.workspace}</dd><dt>Runtime</dt><dd>{run.runtime}</dd><dt>Revision</dt><dd>{run.revision}</dd><dt>Updated</dt><dd>{new Date(run.updated_at).toLocaleString()}</dd>{run.terminal_reason && <><dt>Reason</dt><dd>{run.terminal_reason}</dd></>}</dl>{!terminal.has(run.status) && <button className="danger" disabled={busy} onClick={() => void cancel()}>Cancel task</button>}</> : <p className="muted">Create a task to begin polling its durable Control API state.</p>}</div>
    </div>
  </div>
}
