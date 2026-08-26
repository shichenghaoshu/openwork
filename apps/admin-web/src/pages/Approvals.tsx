import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { Approval } from '../types'

function purchaseDetail(item: Approval): React.JSX.Element | null {
  const params = item.parameters as Record<string, unknown> | null
  if (!item.capability || !item.capability.startsWith('erp.') || !params) return null
  const row = (label: string, value: unknown): React.JSX.Element => (
    <div key={label} className="field"><dt>{label}</dt><dd>{value !== undefined && value !== null ? String(value) : '—'}</dd></div>
  )
  return <div className="task-detail">{item.capability === 'erp.purchase_request.create' ? (
    <>
      <p className="eyebrow">Exact purchase request</p>
      <dl>
        {row('Requester', params.requester)}
        {row('Department', params.department)}
        {row('Cost center', params.cost_center)}
        {row('Item', params.item_description)}
        {row('Quantity', params.quantity)}
        {row('Unit estimate', params.unit_estimate)}
        {row('Currency', params.currency)}
        {row('Vendor', params.vendor)}
        {row('Delivery location', params.delivery_location)}
        {row('Requested date', params.requested_date)}
        {row('Budget reference', params.budget_reference)}
        {row('Justification', params.justification)}
      </dl>
    </>
  ) : (
    <>
      <p className="eyebrow">Capability read</p>
      <dl>{row('Capability', item.capability)}</dl>
    </>
  )}</div>
}

export default function Approvals(): React.JSX.Element {
  const [items, setItems] = useState<Approval[]>([]); const [error, setError] = useState<string | null>(null); const [busy, setBusy] = useState<string | null>(null)
  const load = useCallback(async () => { try { setItems(await api.listApprovals()); setError(null) } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } }, [])
  useEffect(() => { void load() }, [load])
  const decide = async (item: Approval, decision: 'approve' | 'deny'): Promise<void> => { setBusy(item.id); try { await api.decideApproval(item.id, item.revision, decision); await load() } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } finally { setBusy(null) } }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Human decision point</p><h1>Approvals</h1></div><button className="quiet" onClick={() => void load()}>Refresh</button></div>{error && <p className="notice error">{error}</p>}<div className="list">{items.length === 0 ? <p className="muted">No approval requests returned by the Control API.</p> : items.map((item) => <article key={item.id}><div><span className="status">{item.status}</span><h2>{item.request_reason}</h2><p>Run {item.run_id} · requested by {item.requested_by}</p><small>Expires {new Date(item.expires_at).toLocaleString()}</small></div>{purchaseDetail(item)}{item.status.toLowerCase() === 'pending' && <div className="actions"><button disabled={busy === item.id} onClick={() => void decide(item, 'approve')}>Approve</button><button className="danger" disabled={busy === item.id} onClick={() => void decide(item, 'deny')}>Deny</button></div>}</article>)}</div></div>
}
