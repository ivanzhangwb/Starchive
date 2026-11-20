import { Archive, HardDrive, LibraryBig, MessageSquare } from 'lucide-react'
import { useState } from 'react'
import { createRoot } from 'react-dom/client'
import { AgentPane } from './AgentPane'
import type { Repo } from './api'
import { Library } from './Library'
import type { RepoSelection } from './RepoDetails'
import './styles.css'

function App() {
  const [selection, setSelection] = useState<RepoSelection | null>(null)
  const [contextRepo, setContextRepo] = useState<Repo | null>(null)
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null)
  const [agentBusy, setAgentBusy] = useState(true)
  const [syncTimestamp, setSyncTimestamp] = useState<string | null>(null)
  const [compactPane, setCompactPane] = useState<'library' | 'agent'>('library')

  function openRepo(next: RepoSelection | null) {
    setSelection(next)
    setCompactPane('library')
  }

  return (
    <div className="app" data-compact-pane={compactPane}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark"><Archive size={21} /></span>Starchive</div>
        <span className="local-status"><HardDrive size={15} /> 本地资料库</span>
        <nav className="compact-nav" aria-label="工作区">
          <button className="icon-button" type="button" aria-pressed={compactPane === 'library'} aria-label="资料库" onClick={() => setCompactPane('library')}><LibraryBig size={18} /></button>
          <button className="icon-button" type="button" aria-pressed={compactPane === 'agent'} aria-label="Agent 会话" onClick={() => setCompactPane('agent')}><MessageSquare size={18} /></button>
        </nav>
      </header>
      <div className="workspace">
        <Library
          selection={selection}
          activeSessionId={activeSessionId}
          contextLocked={agentBusy}
          onSelect={openRepo}
          onContext={(repo) => { setContextRepo(repo); setCompactPane('agent') }}
          onSync={setSyncTimestamp}
        />
        <AgentPane
          contextRepo={contextRepo}
          syncTimestamp={syncTimestamp}
          onContext={setContextRepo}
          onCloseContext={() => setContextRepo(null)}
          onSelectRepo={openRepo}
          onBusy={setAgentBusy}
          onSessionChange={setActiveSessionId}
        />
      </div>
    </div>
  )
}

createRoot(document.getElementById('root')!).render(<App />)