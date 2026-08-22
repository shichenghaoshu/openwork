import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { RuntimeInfo, WorkspaceApp } from '../types'

type Panel = 'runtimes' | 'store' | 'settings'

export default function Admin(): React.JSX.Element {
  const [panel, setPanel] = useState<Panel>('runtimes')
  const [runtimes, setRuntimes] = useState<RuntimeInfo[]>([])
  const [apps, setApps] = useState<WorkspaceApp[]>([])
  const [settings, setSettings] = useState<Record<string, unknown> | null>(null)
  const [defaultRuntime, setDefaultRuntime] = useState('auto')
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const load = useCallback(async (next: Panel) => {
    try {
      if (next === 'runtimes') setRuntimes(await api.listRuntimes())
      if (next === 'store') setApps(await api.listStoreApps())
      if (next === 'settings') {
        const value = await api.getSettings()
        setSettings(value)
        if (typeof value.default_runtime === 'string') setDefaultRuntime(value.default_runtime)
      }
      setError(null)
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
  }, [])
  useEffect(() => { void load(panel) }, [load, panel])
  const action = async (item: WorkspaceApp): Promise<void> => {
    setBusy(item.id)
    try {
      if (item.installed) await api.setAppEnabled(item.id, !item.enabled)
      else await api.installApp(item.id)
      await load('store')
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(null) }
  }
  const saveSettings = async (): Promise<void> => {
    setBusy('settings')
    try { setSettings(await api.updateSettings(defaultRuntime)); setError(null) }
    catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(null) }
  }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Administrator</p><h1>{panel === 'runtimes' ? 'AI Runtimes' : panel === 'store' ? 'App Store' : 'Settings'}</h1></div><div className="tabs"><button className={panel === 'runtimes' ? 'active-tab' : 'quiet'} onClick={() => setPanel('runtimes')}>AI Runtimes</button><button className={panel === 'store' ? 'active-tab' : 'quiet'} onClick={() => setPanel('store')}>App Store</button><button className={panel === 'settings' ? 'active-tab' : 'quiet'} onClick={() => setPanel('settings')}>Settings</button></div></div>
    {error && <p className="notice error">{error}</p>}
    {panel === 'runtimes' && <div className="list">{runtimes.length === 0 ? <p className="muted">No runtimes returned by the Control API.</p> : runtimes.map((item) => <article key={item.id}><div><span className="status">{item.status ?? 'Unknown'}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? item.id}</p></div></article>)}</div>}
    {panel === 'store' && <div className="list">{apps.length === 0 ? <p className="muted">No app store entries returned by the Control API.</p> : apps.map((item) => <article key={item.id}><div><span className="status">{item.installed ? item.enabled ? 'Enabled' : 'Disabled' : 'Not installed'}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? 'No description returned.'}</p></div><button className={item.installed && item.enabled ? 'danger' : ''} disabled={busy === item.id} onClick={() => void action(item)}>{busy === item.id ? 'Saving…' : item.installed ? item.enabled ? 'Disable' : 'Enable' : 'Install'}</button></article>)}</div>}
    {panel === 'settings' && <div className="task-detail">{settings ? <><label>Default runtime<select value={defaultRuntime} onChange={(event) => setDefaultRuntime(event.target.value)}><option value="auto">Auto</option><option value="claude-code">Claude Code</option><option value="codex">Codex</option><option value="opencode">OpenCode</option><option value="pi">Pi</option></select></label><button disabled={busy === 'settings'} onClick={() => void saveSettings()}>{busy === 'settings' ? 'Saving…' : 'Save runtime preference'}</button><p className="muted">Provider credentials remain in the Control API secret boundary and are never sent to this desktop renderer.</p></> : <p className="muted">No settings returned by the Control API.</p>}</div>}
  </div>
}
