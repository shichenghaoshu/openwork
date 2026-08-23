import { useCallback, useEffect, useState } from 'react'
import { api } from './api'
import { getLocale, setLocale, serverErrorText, t } from './i18n'
import type { Locale } from './i18n'
import Approvals from './pages/Approvals'
import Apps from './pages/Apps'
import Connectors from './pages/Connectors'
import Files from './pages/Files'
import Admin from './pages/Admin'
import Tasks from './pages/Tasks'
import Workspace from './pages/Workspace'

type Page = 'workspace' | 'tasks' | 'files' | 'apps' | 'approvals' | 'connectors' | 'admin'

function nav(): { page: Page; label: string }[] {
  // Resolved at render time so language switches update every label.
  return [
    { page: 'workspace', label: t('nav.workspace') },
    { page: 'tasks', label: t('nav.tasks') },
    { page: 'files', label: t('nav.files') },
    { page: 'apps', label: t('nav.apps') },
    { page: 'approvals', label: t('nav.approvals') },
    { page: 'connectors', label: t('nav.connectors') },
  ]
}

export default function App(): React.JSX.Element {
  const [page, setPage] = useState<Page>('workspace')
  const [online, setOnline] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const [lang, setLang] = useState<Locale>(getLocale())
  const refresh = useCallback(async () => {
    try { await api.health(); setOnline(true); setError(null) }
    catch (cause) { setOnline(false); setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
  }, [])
  useEffect(() => { void refresh() }, [refresh])
  useEffect(() => { document.title = lang === 'zh' ? 'OpenWork 管理台' : 'OpenWork Admin' }, [lang])
  const toggleLang = (): void => {
    const next: Locale = lang === 'zh' ? 'en' : 'zh'
    setLocale(next); setLang(next)
  }
  const items = nav()
  return <div className="app-shell">
    <aside className="sidebar"><div className="brand"><span>OpenWork</span><small>{t('app.workspace')}</small></div>
      <nav>{items.map((item) => <button key={item.page} className={page === item.page ? 'active' : ''} onClick={() => setPage(item.page)}>{item.label}</button>)}</nav>
      <div className="admin-nav"><span>{t('app.admin')}</span><button className={page === 'admin' ? 'active' : ''} onClick={() => setPage('admin')}>{t('app.adminShort')}</button></div>
      <div className="connection"><i className={online ? 'online' : ''} />{online ? t('app.connected') : t('app.offline')}</div>
    </aside>
    <main><header><span>{items.find((item) => item.page === page)?.label}</span><span className="header-actions"><button className="quiet" onClick={() => void refresh()}>{t('app.refresh')}</button><button className="quiet" onClick={toggleLang}>{lang === 'zh' ? 'EN' : '中文'}</button></span></header>
      {error && <div className="notice error">{error}</div>}
      <section>{page === 'workspace' && <Workspace onNewTask={() => setPage('tasks')} />}{page === 'tasks' && <Tasks />}{page === 'files' && <Files />}{page === 'apps' && <Apps onShowApprovals={() => setPage('approvals')} />}{page === 'approvals' && <Approvals />}{page === 'connectors' && <Connectors />}{page === 'admin' && <Admin />}</section>
    </main>
  </div>
}
