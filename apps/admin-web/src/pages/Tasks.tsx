import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import type { Run, WorkspaceFile } from '../types'

const terminal = new Set(['succeeded', 'failed', 'cancelled', 'timed_out'])
const runtimes = [['auto', 'Auto'], ['claude-code', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['pi', 'Pi']]

export default function Tasks(): React.JSX.Element {
  const [runtime, setRuntime] = useState('auto')
  const [workspace, setWorkspace] = useState('')
  const [prompt, setPrompt] = useState('')
  const [run, setRun] = useState<Run | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [preview, setPreview] = useState<{ name: string; content: string } | null>(null)
  const [files, setFiles] = useState<WorkspaceFile[]>([])
  const [fileIds, setFileIds] = useState<string[]>([])
  const timer = useRef<number | null>(null)
  const poll = async (id: string): Promise<void> => {
    try { const next = await api.getRun(id); setRun(next); if (terminal.has(next.status) && timer.current) window.clearInterval(timer.current) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
  }
  useEffect(() => {
    void api.listFiles().then(setFiles).catch(() => undefined)
    void api.getSettings().then((settings) => {
      if (typeof settings.default_runtime === 'string' && runtimes.some(([id]) => id === settings.default_runtime)) setRuntime(settings.default_runtime)
    }).catch(() => undefined)
    return () => { if (timer.current) window.clearInterval(timer.current) }
  }, [])
  const create = async (): Promise<void> => {
    setBusy('creating'); setError(null)
    try { const created = await api.createRun({ runtime, workspace: workspace.trim(), prompt: prompt.trim(), file_ids: fileIds }); setRun(created); if (timer.current) window.clearInterval(timer.current); timer.current = window.setInterval(() => void poll(created.id), 2500) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(null) }
  }
  const cancel = async (): Promise<void> => { if (!run) return; setBusy('cancelling'); setError(null); try { await api.cancelRun(run.id); await poll(run.id) } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } finally { setBusy(null) } }
  const previewArtifact = async (artifact: { id?: string; name?: string; [key: string]: unknown }): Promise<void> => {
    if (typeof artifact.id !== 'string' || !run) return
    setBusy(`preview:${artifact.id}`)
    try {
      const content = await api.artifactContent(run.id, artifact.id)
      setPreview({ name: String(artifact.name ?? artifact.id), content })
      setError(null)
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(null) }
  }
  return <div className="page">
    <div className="page-heading"><div><p className="eyebrow">Controlled execution</p><h1>Tasks</h1></div>{run && <span className={`status ${run.status.toLowerCase()}`}>{run.status}</span>}</div>
    <div className="workspace-grid"><form onSubmit={(event) => { event.preventDefault(); void create() }}>
      <label>Runtime<select value={runtime} onChange={(event) => setRuntime(event.target.value)}>{runtimes.map(([id, name]) => <option key={id} value={id}>{name}</option>)}</select></label>
      <label>Workspace ID<input required value={workspace} onChange={(event) => setWorkspace(event.target.value)} placeholder="team/project" /></label>
      <label>Task prompt<textarea required value={prompt} onChange={(event) => setPrompt(event.target.value)} placeholder="Describe the task for the controlled worker." /></label>
      <fieldset className="file-picker"><legend>Files to use</legend>{files.length === 0 ? <span className="muted">No files are available.</span> : files.map((file) => <label key={file.id} className="check"><input type="checkbox" checked={fileIds.includes(file.id)} onChange={(event) => setFileIds((ids) => event.target.checked ? [...ids, file.id] : ids.filter((id) => id !== file.id))} />{file.name}</label>)}</fieldset>
      <button disabled={busy !== null || !workspace.trim() || !prompt.trim()}>{busy === 'creating' ? 'Working…' : 'Create task'}</button>
    </form>
    <div className="task-detail">{error && <p className="notice error">{error}</p>}{run ? <><p className="eyebrow">Task detail</p><h2>{run.id}</h2><dl><dt>Workspace</dt><dd>{run.workspace}</dd><dt>Runtime</dt><dd>{run.runtime}</dd><dt>Apps used</dt><dd>{run.apps_used?.join(', ') || 'Not reported'}</dd><dt>Artifacts</dt><dd>{run.artifacts?.length ? run.artifacts.map((item) => <button key={typeof item.id === 'string' ? item.id : String(item.name ?? 'artifact')} type="button" className="quiet artifact-link" disabled={busy === `preview:${item.id}`} onClick={() => void previewArtifact(item)}>{String(item.name ?? item.id ?? 'Artifact')}</button>) : 'Not reported'}</dd>{preview && <><dt>Preview · {preview.name}</dt><dd><pre className="artifact-preview">{preview.content}</pre></dd></>}<dt>Actions</dt><dd>{run.actions?.map((item) => item.description ?? item.id ?? item.status ?? 'Action').join(', ') || 'Not reported'}</dd><dt>Approvals</dt><dd>{run.approvals?.map((item) => item.status ?? item.id ?? 'Approval').join(', ') || 'Not reported'}</dd><dt>Updated</dt><dd>{new Date(run.updated_at).toLocaleString()}</dd>{run.terminal_reason && <><dt>Reason</dt><dd>{run.terminal_reason}</dd></>}</dl>{!terminal.has(run.status) && <button className="danger" disabled={busy !== null} onClick={() => void cancel()}>Cancel task</button>}</> : <p className="muted">Create a task to follow its details here.</p>}</div>
    </div>
  </div>
}
