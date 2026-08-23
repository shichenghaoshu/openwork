import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import { approvalStatusText, formatTime, serverErrorText, t } from '../i18n'
import type { Approval } from '../types'

export default function Approvals(): React.JSX.Element {
  const [items, setItems] = useState<Approval[]>([]); const [error, setError] = useState<string | null>(null); const [busy, setBusy] = useState<string | null>(null)
  const load = useCallback(async () => { try { setItems(await api.listApprovals()); setError(null) } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) } }, [])
  useEffect(() => { void load() }, [load])
  const decide = async (item: Approval, decision: 'approve' | 'deny'): Promise<void> => { setBusy(item.id); try { await api.decideApproval(item.id, item.revision, decision); await load() } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) } finally { setBusy(null) } }
  const execute = async (item: Approval): Promise<void> => { setBusy(item.id); try { await api.executeApproval(item.id, item.revision); await load() } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) } finally { setBusy(null) } }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">{t('approvals.eyebrow')}</p><h1>{t('approvals.title')}</h1></div><button className="quiet" onClick={() => void load()}>{t('app.refresh')}</button></div>{error && <p className="notice error">{error}</p>}<div className="list">{items.length === 0 ? <p className="muted">{t('approvals.empty')}</p> : items.map((item) => <article key={item.id}><div><span className="status">{approvalStatusText(item.status)}</span><h2>{item.request_reason}</h2><p>{t('approvals.run')} {item.run_id} · {t('approvals.requestedBy')} {item.requested_by}</p><small>{t('approvals.expires')}: {formatTime(item.expires_at)}</small></div>{item.status.toLowerCase() === 'pending' && <div className="actions"><button disabled={busy === item.id} onClick={() => void decide(item, 'approve')}>{t('approvals.approve')}</button><button className="danger" disabled={busy === item.id} onClick={() => void decide(item, 'deny')}>{t('approvals.deny')}</button></div>}{item.status.toLowerCase() === 'approved' && <div className="actions"><button disabled={busy === item.id} onClick={() => void execute(item)}>{busy === item.id ? t('approvals.executing') : t('approvals.execute')}</button></div>}</article>)}</div></div>
}
