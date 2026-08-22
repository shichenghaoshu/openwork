import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { Approval } from '../types'

export default function Approvals(): React.JSX.Element {
  const [items, setItems] = useState<Approval[]>([]); const [error, setError] = useState<string | null>(null); const [busy, setBusy] = useState<string | null>(null)
  const load = useCallback(async () => { try { setItems(await api.listApprovals()); setError(null) } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } }, [])
  useEffect(() => { void load() }, [load])
  const decide = async (item: Approval, decision: 'approve' | 'deny'): Promise<void> => { setBusy(item.id); try { await api.decideApproval(item.id, item.revision, decision); await load() } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } finally { setBusy(null) } }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Human decision point</p><h1>Approvals</h1></div><button className="quiet" onClick={() => void load()}>Refresh</button></div>{error && <p className="notice error">{error}</p>}<div className="list">{items.length === 0 ? <p className="muted">No approval requests returned by the Control API.</p> : items.map((item) => <article key={item.id}><div><span className="status">{item.status}</span><h2>{item.request_reason}</h2><p>Run {item.run_id} · requested by {item.requested_by}</p><small>Expires {new Date(item.expires_at).toLocaleString()}</small></div>{item.status.toLowerCase() === 'pending' && <div className="actions"><button disabled={busy === item.id} onClick={() => void decide(item, 'approve')}>Approve</button><button className="danger" disabled={busy === item.id} onClick={() => void decide(item, 'deny')}>Deny</button></div>}</article>)}</div></div>
}
