import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import Approvals from './pages/Approvals'
import Connectors from './pages/Connectors'
import Tasks from './pages/Tasks'
import Workspace from './pages/Workspace'

type Page = 'workspace' | 'tasks' | 'approvals' | 'connectors'
const nav: { page: Page; label: string }[] = [
  { page: 'workspace', label: 'Workspace' },
  { page: 'tasks', label: 'Tasks' },
  { page: 'approvals', label: 'Approvals' },
  { page: 'connectors', label: 'Connectors' },
]

export default function App(): React.JSX.Element {
  const [page, setPage] = useState<Page>('workspace')
  const [online, setOnline] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const refresh = useCallback(async () => {
    try { await api.health(); setOnline(true); setError(null) }
    catch (cause) { setOnline(false); setError(cause instanceof Error ? cause.message : String(cause)) }
  }, [])
  useEffect(() => { void refresh() }, [refresh])
  return <div className="app-shell">
    <aside className="sidebar"><div className="brand"><span>OpenWork</span><small>Employee Workspace</small></div>
      <nav>{nav.map((item) => <button key={item.page} className={page === item.page ? 'active' : ''} onClick={() => setPage(item.page)}>{item.label}</button>)}</nav>
      <div className="connection"><i className={online ? 'online' : ''} />{online ? 'Control API connected' : 'Control API offline'}</div>
    </aside>
    <main><header><span>{nav.find((item) => item.page === page)?.label}</span><button className="quiet" onClick={() => void refresh()}>Refresh</button></header>
      {error && <div className="notice error">{error}</div>}
      <section>{page === 'workspace' && <Workspace />}{page === 'tasks' && <Tasks />}{page === 'approvals' && <Approvals />}{page === 'connectors' && <Connectors />}</section>
    </main>
  </div>
}
