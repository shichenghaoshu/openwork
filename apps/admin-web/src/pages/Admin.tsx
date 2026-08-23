import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import { runtimeStatusText, serverErrorText, t } from '../i18n'
import type { RuntimeInfo, WorkspaceApp } from '../types'

type Panel = 'runtimes' | 'store' | 'settings'
const runtimeOptions: [string, string][] = [['auto', 'auto'], ['claude-code', 'Claude Code'], ['codex', 'Codex'], ['opencode', 'OpenCode'], ['pi', 'Pi']]

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
    } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
  }, [])
  useEffect(() => { void load(panel) }, [load, panel])
  const action = async (item: WorkspaceApp): Promise<void> => {
    setBusy(item.id)
    try {
      if (item.installed) await api.setAppEnabled(item.id, !item.enabled)
      else await api.installApp(item.id)
      await load('store')
    } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
    finally { setBusy(null) }
  }
  const saveSettings = async (): Promise<void> => {
    setBusy('settings')
    try { setSettings(await api.updateSettings(defaultRuntime)); setError(null) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
    finally { setBusy(null) }
  }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">{t('admin.eyebrow')}</p><h1>{panel === 'runtimes' ? t('admin.runtimes') : panel === 'store' ? t('admin.store') : t('admin.settings')}</h1></div><div className="tabs"><button className={panel === 'runtimes' ? 'active-tab' : 'quiet'} onClick={() => setPanel('runtimes')}>{t('admin.runtimes')}</button><button className={panel === 'store' ? 'active-tab' : 'quiet'} onClick={() => setPanel('store')}>{t('admin.store')}</button><button className={panel === 'settings' ? 'active-tab' : 'quiet'} onClick={() => setPanel('settings')}>{t('admin.settings')}</button></div></div>
    {error && <p className="notice error">{error}</p>}
    {panel === 'runtimes' && <div className="list">{runtimes.length === 0 ? <p className="muted">{t('admin.noRuntimes')}</p> : runtimes.map((item) => <article key={item.id}><div><span className="status">{runtimeStatusText(item.status ?? 'unknown')}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? item.id}</p></div></article>)}</div>}
    {panel === 'store' && <div className="list">{apps.length === 0 ? <p className="muted">{t('admin.noStore')}</p> : apps.map((item) => <article key={item.id}><div><span className="status">{item.installed ? item.enabled ? t('admin.enabled') : t('admin.disabled') : t('admin.notInstalled')}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? t('apps.noDescription')}</p></div><button className={item.installed && item.enabled ? 'danger' : ''} disabled={busy === item.id} onClick={() => void action(item)}>{busy === item.id ? t('admin.saving') : item.installed ? item.enabled ? t('admin.disable') : t('admin.enable') : t('admin.install')}</button></article>)}</div>}
    {panel === 'settings' && <div className="task-detail">{settings ? <><label>{t('admin.defaultRuntime')}<select value={defaultRuntime} onChange={(event) => setDefaultRuntime(event.target.value)}>{runtimeOptions.map(([id, name]) => <option key={id} value={id}>{name}</option>)}</select></label><button disabled={busy === 'settings'} onClick={() => void saveSettings()}>{busy === 'settings' ? t('admin.saving') : t('admin.saveRuntime')}</button><p className="muted">{t('admin.credentialNote')}</p></> : <p className="muted">{t('admin.noSettings')}</p>}</div>}
  </div>
}
