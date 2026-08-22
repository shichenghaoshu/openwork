import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { WorkspaceApp } from '../types'

export default function Apps(): React.JSX.Element {
  const [apps, setApps] = useState<WorkspaceApp[]>([])
  const [error, setError] = useState<string | null>(null)
  const load = useCallback(async () => {
    try { setApps(await api.listApps()); setError(null) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
  }, [])
  useEffect(() => { void load() }, [load])
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Available for your work</p><h1>Apps</h1></div><button className="quiet" onClick={() => void load()}>Refresh</button></div>
    {error && <p className="notice error">{error}</p>}<div className="list">{apps.length === 0 ? <p className="muted">No apps returned by the Control API.</p> : apps.map((item) => <article key={item.id}><div><span className={`status ${item.enabled ? 'succeeded' : ''}`}>{item.enabled ? 'Available' : 'Unavailable'}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? 'No description returned.'}</p></div></article>)}</div>
  </div>
}
