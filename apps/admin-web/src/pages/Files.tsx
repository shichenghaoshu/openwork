import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { WorkspaceFile } from '../types'

function fileSize(value?: number): string {
  if (value === undefined) return 'Size unavailable'
  if (value < 1024) return `${value} B`
  if (value < 1024 * 1024) return `${Math.ceil(value / 1024)} KB`
  return `${(value / (1024 * 1024)).toFixed(1)} MB`
}

export default function Files(): React.JSX.Element {
  const [files, setFiles] = useState<WorkspaceFile[]>([])
  const [selected, setSelected] = useState<WorkspaceFile | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState(false)
  const load = useCallback(async () => {
    try { setFiles(await api.listFiles()); setError(null) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
  }, [])
  useEffect(() => { void load() }, [load])
  const upload = async (file?: File): Promise<void> => {
    if (!file) return
    setBusy(true)
    try { const added = await api.uploadFile(file); setFiles((items) => [added, ...items]); setSelected(added); setError(null) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(false) }
  }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Shared task context</p><h1>Files</h1></div><label className="upload"><input type="file" onChange={(event) => void upload(event.target.files?.[0])} />{busy ? 'Uploading…' : 'Upload file'}</label></div>
    {error && <p className="notice error">{error}</p>}
    <div className="connector-layout"><div className="list">{files.length === 0 ? <p className="muted">No files returned by the Control API.</p> : files.map((file) => <button className={selected?.id === file.id ? 'connector selected' : 'connector'} key={file.id} onClick={() => setSelected(file)}><strong>{file.name}</strong><small>{fileSize(file.size_bytes)} · {file.content_type ?? 'file'}</small></button>)}</div>
      <div className="task-detail"><p className="eyebrow">File details</p>{selected ? <><h2>{selected.name}</h2><dl><dt>Type</dt><dd>{selected.content_type ?? 'Unavailable'}</dd><dt>Size</dt><dd>{fileSize(selected.size_bytes)}</dd><dt>Added</dt><dd>{selected.created_at ? new Date(selected.created_at).toLocaleString() : 'Unavailable'}</dd><dt>File ID</dt><dd>{selected.id}</dd></dl><p className="muted">Choose this file from New task to include it in the work context.</p></> : <p className="muted">Select a file to view its details.</p>}</div>
    </div></div>
}
