import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import type { Connector, ConnectorTool } from '../types'

export default function Connectors(): React.JSX.Element {
  const [connectors, setConnectors] = useState<Connector[]>([]); const [tools, setTools] = useState<ConnectorTool[]>([]); const [selected, setSelected] = useState<string | null>(null); const [error, setError] = useState<string | null>(null)
  const load = useCallback(async () => { try { setConnectors(await api.listConnectors()); setError(null) } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)) } }, [])
  useEffect(() => { void load() }, [load])
  const choose = async (id: string): Promise<void> => { setSelected(id); try { setTools(await api.listConnectorTools(id)); setError(null) } catch (cause) { setTools([]); setError(cause instanceof Error ? cause.message : String(cause)) } }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">Available integrations</p><h1>Connectors</h1></div><button className="quiet" onClick={() => void load()}>Refresh</button></div>{error && <p className="notice error">{error}</p>}<div className="connector-layout"><div className="list">{connectors.length === 0 ? <p className="muted">No connectors returned. If this Control API does not yet provide /v1/connectors, its real server error is shown above.</p> : connectors.map((connector) => <button className={selected === connector.id ? 'connector selected' : 'connector'} key={connector.id} onClick={() => void choose(connector.id)}><strong>{connector.name ?? connector.id}</strong><small>{connector.status ?? connector.id}</small></button>)}</div><div className="task-detail"><p className="eyebrow">Tools</p>{selected ? tools.length === 0 ? <p className="muted">No tools returned for {selected}.</p> : tools.map((tool) => <article className="tool" key={tool.id}><h2>{tool.name ?? tool.id}</h2><p>{tool.description ?? tool.id}</p></article>) : <p className="muted">Select a connector to query its tools.</p>}</div></div></div>
}
