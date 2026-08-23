import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import { formatTime, serverErrorText, t } from '../i18n'
import type { WorkspaceFile } from '../types'

function fileSize(value?: number): string {
  if (value === undefined) return t('files.sizeUnavailable')
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
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
  }, [])
  useEffect(() => { void load() }, [load])
  const upload = async (file?: File): Promise<void> => {
    if (!file) return
    setBusy(true)
    try { const added = await api.uploadFile(file); setFiles((items) => [added, ...items]); setSelected(added); setError(null) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
    finally { setBusy(false) }
  }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">{t('files.eyebrow')}</p><h1>{t('files.title')}</h1></div><label className="upload"><input type="file" onChange={(event) => void upload(event.target.files?.[0])} />{busy ? t('files.uploading') : t('files.upload')}</label></div>
    {error && <p className="notice error">{error}</p>}
    <div className="connector-layout"><div className="list">{files.length === 0 ? <p className="muted">{t('files.empty')}</p> : files.map((file) => <button className={selected?.id === file.id ? 'connector selected' : 'connector'} key={file.id} onClick={() => setSelected(file)}><strong>{file.name}</strong><small>{fileSize(file.size_bytes)} · {file.content_type ?? t('files.typeUnavailable')}</small></button>)}</div>
      <div className="task-detail"><p className="eyebrow">{t('files.details')}</p>{selected ? <><h2>{selected.name}</h2><dl><dt>{t('files.type')}</dt><dd>{selected.content_type ?? t('files.typeUnavailable')}</dd><dt>{t('files.size')}</dt><dd>{fileSize(selected.size_bytes)}</dd><dt>{t('files.added')}</dt><dd>{selected.created_at ? formatTime(selected.created_at) : t('files.timeUnavailable')}</dd><dt>{t('files.fileId')}</dt><dd>{selected.id}</dd></dl><p className="muted">{t('files.hint')}</p></> : <p className="muted">{t('files.selectHint')}</p>}</div>
    </div></div>
}
