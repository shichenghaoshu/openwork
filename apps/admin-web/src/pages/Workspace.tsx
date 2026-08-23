import { useState } from 'react'
import { api } from '../api'
import { runStatusText, serverErrorText, t } from '../i18n'
import type { SalesDemoReport } from '../types'

export default function Workspace({ onNewTask }: { onNewTask: () => void }): React.JSX.Element {
  const [demo, setDemo] = useState<SalesDemoReport | null>(null)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const runDemo = async (): Promise<void> => {
    setBusy(true); setError(null)
    try { setDemo(await api.runSalesDemo()) }
    catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) }
    finally { setBusy(false) }
  }
  return <div className="page intro">
    <p className="eyebrow">{t('app.workspace')}</p>
    <h1>{t('app.welcomeTitle')}</h1>
    <p>{t('app.workspaceHint')}</p>
    <button onClick={onNewTask}>{t('app.newTask')}</button>
    <div className="rule" />
    <section>
      <h2>{t('demo.salesTitle')}</h2>
      <p className="muted">{t('demo.runSales')}</p>
      <button className="quiet" disabled={busy} onClick={() => void runDemo()}>
        {busy ? t('demo.runningSales') : t('demo.runSales')}
      </button>
      {demo && <div className="task-detail">
        <p className="eyebrow">{t('tasks.detail')}</p>
        <span className={`status ${demo.status ?? ''}`}>{runStatusText(demo.status)}</span>
        <dl>
          <dt>{t('demo.artifacts')}</dt><dd>{(demo.artifacts ?? []).map((item) => item.path ?? item.sha256 ?? '').join(', ') || t('tasks.notReported')}</dd>
          <dt>{t('demo.auditEvents')}</dt><dd>{demo.audit_events?.length ?? 0}</dd>
          <dt>{t('demo.outputDirectory')}</dt><dd>{demo.output_directory ?? t('tasks.notReported')}</dd>
        </dl>
      </div>}
      {error && <p className="notice error">{error}</p>}
    </section>
    <p className="muted">{t('app.hubHint')}</p>
  </div>
}
