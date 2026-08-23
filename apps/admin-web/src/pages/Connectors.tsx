import { useCallback, useEffect, useState } from 'react'
import { api } from '../api'
import { connectorStatusText, serverErrorText, t } from '../i18n'
import type { Connector, ConnectorTool } from '../types'

export default function Connectors(): React.JSX.Element {
  const [connectors, setConnectors] = useState<Connector[]>([]); const [tools, setTools] = useState<ConnectorTool[]>([]); const [selected, setSelected] = useState<string | null>(null); const [error, setError] = useState<string | null>(null)
  const load = useCallback(async () => { try { setConnectors(await api.listConnectors()); setError(null) } catch (cause) { setError(serverErrorText(cause instanceof Error ? cause : String(cause))) } }, [])
  useEffect(() => { void load() }, [load])
  const choose = async (id: string): Promise<void> => { setSelected(id); try { setTools(await api.listConnectorTools(id)); setError(null) } catch (cause) { setTools([]); setError(serverErrorText(cause instanceof Error ? cause : String(cause))) } }
  return <div className="page"><div className="page-heading"><div><p className="eyebrow">{t('connectors.eyebrow')}</p><h1>{t('connectors.title')}</h1></div><button className="quiet" onClick={() => void load()}>{t('app.refresh')}</button></div>{error && <p className="notice error">{error}</p>}<div className="connector-layout"><div className="list">{connectors.length === 0 ? <p className="muted">{t('connectors.empty')}</p> : connectors.map((connector) => <button className={selected === connector.id ? 'connector selected' : 'connector'} key={connector.id} onClick={() => void choose(connector.id)}><strong>{connector.name ?? connector.id}</strong><small>{connectorStatusText(connector.status)}</small></button>)}</div><div className="task-detail"><p className="eyebrow">{t('connectors.tools')}</p>{selected ? tools.length === 0 ? <p className="muted">{t('connectors.noToolsFor', { name: selected })}</p> : tools.map((tool) => <article className="tool" key={tool.id}><h2>{tool.name ?? tool.id}</h2><p>{tool.description ?? tool.id}</p></article>) : <p className="muted">{t('connectors.selectHint')}</p>}</div></div></div>
}
