import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import Approvals from './pages/Approvals'
import Apps from './pages/Apps'
import Files from './pages/Files'
import Admin from './pages/Admin'
import Tasks from './pages/Tasks'
import Workspace from './pages/Workspace'

type Page = 'workspace' | 'tasks' | 'files' | 'apps' | 'approvals' | 'admin'
const nav: { page: Page; label: string }[] = [
  { page: 'workspace', label: 'Workspace' },
  { page: 'tasks', label: 'Tasks' },
  { page: 'files', label: 'Files' },
  { page: 'apps', label: 'Apps' },
  { page: 'approvals', label: 'Approvals' },
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
      <div className="admin-nav"><span>Administration</span><button className={page === 'admin' ? 'active' : ''} onClick={() => setPage('admin')}>Runtimes · App Store · Settings</button></div>
      <div className="connection"><i className={online ? 'online' : ''} />{online ? 'Control API connected' : 'Control API offline'}</div>
    </aside>
    <main><header><span>{nav.find((item) => item.page === page)?.label}</span><button className="quiet" onClick={() => void refresh()}>Refresh</button></header>
      {error && <div className="notice error">{error}</div>}
      <section>{page === 'workspace' && <Workspace onNewTask={() => setPage('tasks')} />}{page === 'tasks' && <Tasks />}{page === 'files' && <Files />}{page === 'apps' && <Apps />}{page === 'approvals' && <Approvals />}{page === 'admin' && <Admin />}</section>
    </main>
  </div>
}
