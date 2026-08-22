export default function Workspace({ onNewTask }: { onNewTask: () => void }): React.JSX.Element {
  return <div className="page intro">
    <p className="eyebrow">Employee Workspace</p>
    <h1>Start work. Keep context close.</h1>
    <p>Create a task, add the files it needs, and stay informed when a decision needs you.</p>
    <button onClick={onNewTask}>New task</button>
    <div className="rule" />
    <p className="muted">Your tasks, files, apps, and decisions live here.</p>
  </div>
}
