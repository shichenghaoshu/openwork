import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { CredentialMetadata, RuntimeInfo, WorkspaceApp } from '../types'

type Panel = 'runtimes' | 'store' | 'credentials' | 'settings'

interface CredentialDraft {
  credential_ref: string
  provider: string
  environment_variable: string
  scope: string
  owner: string
  value: string
  expires_at: string
}

const emptyDraft: CredentialDraft = {
  credential_ref: '',
  provider: 'github',
  environment_variable: 'GITHUB_PERSONAL_ACCESS_TOKEN',
  scope: 'identity',
  owner: 'control-admin',
  value: '',
  expires_at: '',
}

export default function Admin(): React.JSX.Element {
  const [panel, setPanel] = useState<Panel>('runtimes')
  const [runtimes, setRuntimes] = useState<RuntimeInfo[]>([])
  const [apps, setApps] = useState<WorkspaceApp[]>([])
  const [credentials, setCredentials] = useState<CredentialMetadata[]>([])
  const [settings, setSettings] = useState<Record<string, unknown> | null>(null)
  const [defaultRuntime, setDefaultRuntime] = useState('auto')
  const [draft, setDraft] = useState<CredentialDraft>(emptyDraft)
  const [rotateValue, setRotateValue] = useState<Record<string, string>>({})
  const [error, setError] = useState<string | null>(null)
  const [busy, setBusy] = useState<string | null>(null)
  const load = useCallback(async (next: Panel) => {
    try {
      if (next === 'runtimes') setRuntimes(await api.listRuntimes())
      if (next === 'store') setApps(await api.listStoreApps())
      if (next === 'credentials') setCredentials(await api.listCredentials())
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
  const saveCredential = async (): Promise<void> => {
    setBusy('create-credential')
    try {
      await api.createCredential({
        credential_ref: draft.credential_ref,
        provider: draft.provider,
        environment_variable: draft.environment_variable,
        scope: draft.scope,
        owner: draft.owner,
        value: draft.value,
        expires_at: draft.expires_at || undefined,
      })
      setDraft(emptyDraft)
      setError(null)
      await load('credentials')
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(null) }
  }
  const credentialAction = async (ref: string, operation: 'rotate' | 'revoke' | 'enable'): Promise<void> => {
    setBusy(`${operation}:${ref}`)
    try {
      if (operation === 'rotate') {
        const value = rotateValue[ref]
        if (!value) throw new Error('Enter a new value to rotate this credential.')
        await api.rotateCredential(ref, value)
        setRotateValue((previous) => ({ ...previous, [ref]: '' }))
      } else if (operation === 'revoke') await api.revokeCredential(ref)
      else await api.enableCredential(ref)
      setError(null)
      await load('credentials')
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) }
    finally { setBusy(null) }
  }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Administrator</p><h1>{panel === 'runtimes' ? 'AI Runtimes' : panel === 'store' ? 'App Store' : panel === 'credentials' ? 'Credentials' : 'Settings'}</h1></div><div className="tabs"><button className={panel === 'runtimes' ? 'active-tab' : 'quiet'} onClick={() => setPanel('runtimes')}>AI Runtimes</button><button className={panel === 'store' ? 'active-tab' : 'quiet'} onClick={() => setPanel('store')}>App Store</button><button className={panel === 'credentials' ? 'active-tab' : 'quiet'} onClick={() => setPanel('credentials')}>Credentials</button><button className={panel === 'settings' ? 'active-tab' : 'quiet'} onClick={() => setPanel('settings')}>Settings</button></div></div>
    {error && <p className="notice error">{error}</p>}
    {panel === 'runtimes' && <div className="list">{runtimes.length === 0 ? <p className="muted">No runtimes returned by the Control API.</p> : runtimes.map((item) => <article key={item.id}><div><span className="status">{item.status ?? 'Unknown'}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? item.id}</p></div></article>)}</div>}
    {panel === 'store' && <div className="list">{apps.length === 0 ? <p className="muted">No app store entries returned by the Control API.</p> : apps.map((item) => <article key={item.id}><div><span className="status">{item.installed ? item.enabled ? 'Enabled' : 'Disabled' : 'Not installed'}</span><h2>{item.name ?? item.id}</h2><p>{item.description ?? 'No description returned.'}</p></div><button className={item.installed && item.enabled ? 'danger' : ''} disabled={busy === item.id} onClick={() => void action(item)}>{busy === item.id ? 'Saving…' : item.installed ? item.enabled ? 'Disable' : 'Enable' : 'Install'}</button></article>)}</div>}
    {panel === 'credentials' && <div className="task-detail">
      <section className="list">
        <h2>Stored credentials</h2>
        {credentials.length === 0 ? <p className="muted">No credentials are stored. Configure one below to enable brokered provider access.</p> : credentials.map((item) => <article key={item.credential_ref}><div><span className="status">{item.disabled_at ? 'Revoked' : item.expires_at && item.expires_at < new Date().toISOString() ? 'Expired' : 'Active'}</span><h2>{item.credential_ref}</h2><p>{item.provider} · → {item.environment_variable} · scope: {item.scope} · owner: {item.owner} · revision {item.revision}{item.expires_at ? ` · expires ${item.expires_at}` : ''}</p></div><div className="inline-actions"><input aria-label={`New value for ${item.credential_ref}`} placeholder="New value" value={rotateValue[item.credential_ref] ?? ''} onChange={(event) => setRotateValue((previous) => ({ ...previous, [item.credential_ref]: event.target.value }))} /><button disabled={busy === `rotate:${item.credential_ref}`} onClick={() => void credentialAction(item.credential_ref, 'rotate')}>{busy === `rotate:${item.credential_ref}` ? 'Rotating…' : 'Rotate'}</button><button className="danger" disabled={busy === `revoke:${item.credential_ref}` || item.disabled_at !== null} onClick={() => void credentialAction(item.credential_ref, 'revoke')}>Revoke</button><button disabled={busy === `enable:${item.credential_ref}` || item.disabled_at === null} onClick={() => void credentialAction(item.credential_ref, 'enable')}>Enable</button></div></article>)}
      </section>
      <section>
        <h2>Add or replace a credential</h2>
        <div className="form-grid">
          <label>Reference<input value={draft.credential_ref} placeholder="github.app" onChange={(event) => setDraft({ ...draft, credential_ref: event.target.value })} /></label>
          <label>Provider<input value={draft.provider} onChange={(event) => setDraft({ ...draft, provider: event.target.value })} /></label>
          <label>Injected environment variable<input value={draft.environment_variable} onChange={(event) => setDraft({ ...draft, environment_variable: event.target.value })} /></label>
          <label>Scope<input value={draft.scope} onChange={(event) => setDraft({ ...draft, scope: event.target.value })} /></label>
          <label>Owner<input value={draft.owner} onChange={(event) => setDraft({ ...draft, owner: event.target.value })} /></label>
          <label>Value<input type="password" value={draft.value} placeholder="Secret value (never stored in the renderer)" onChange={(event) => setDraft({ ...draft, value: event.target.value })} /></label>
          <label>Expires at (optional)<input value={draft.expires_at} placeholder="2026-12-31T00:00:00Z" onChange={(event) => setDraft({ ...draft, expires_at: event.target.value })} /></label>
        </div>
        <button disabled={busy === 'create-credential'} onClick={() => void saveCredential()}>{busy === 'create-credential' ? 'Saving…' : 'Store credential'}</button>
        <p className="muted">Credential values are encrypted by the Control API and never leave the secret boundary. Only status metadata is returned to this desktop renderer.</p>
      </section>
    </div>}
    {panel === 'settings' && <div className="task-detail">{settings ? <><label>Default runtime<select value={defaultRuntime} onChange={(event) => setDefaultRuntime(event.target.value)}><option value="auto">Auto</option><option value="claude-code">Claude Code</option><option value="codex">Codex</option><option value="opencode">OpenCode</option><option value="pi">Pi</option></select></label><button disabled={busy === 'settings'} onClick={() => void saveSettings()}>{busy === 'settings' ? 'Saving…' : 'Save runtime preference'}</button><p className="muted">Provider credentials remain in the Control API secret boundary and are never sent to this desktop renderer.</p></> : <p className="muted">No settings returned by the Control API.</p>}</div>}
  </div>
}
