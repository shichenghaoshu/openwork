export default function Workspace(): React.JSX.Element {
  return <div className="page intro">
    <p className="eyebrow">Employee Workspace</p>
    <h1>Work that stays accountable.</h1>
    <p>Start a controlled task, follow its state, and review the approvals and connectors that shape its work.</p>
    <div className="rule" />
    <p className="muted">The desktop app talks only to the configured OpenWork Control API. API credentials stay in the Electron main process.</p>
  </div>
}
