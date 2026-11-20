import { Archive, HardDrive, LibraryBig, MessageSquare, Settings } from 'lucide-react'
import { useCallback, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { AgentPane } from './AgentPane'
import type { Repo } from './api'
import { Library } from './Library'
import type { RepoSelection } from './RepoDetails'
import { SettingsDialog } from './SettingsDialog'
import './styles.css'

function App() {
  const [selection, setSelection] = useState<RepoSelection | null>(null)
  const [contextRepo, setContextRepo] = useState<Repo | null>(null)
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null)
  const [agentBusy, setAgentBusy] = useState(true)
  const [syncTimestamp, setSyncTimestamp] = useState<string | null>(null)
  const [compactPane, setCompactPane] = useState<'library' | 'agent'>('library')
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [agentRevision, setAgentRevision] = useState(0)
  const refreshAgents = useCallback(() => setAgentRevision((current) => current + 1), [])
  const openSettings = () => setSettingsOpen(true)

  function openRepo(next: RepoSelection | null) {
    setSelection(next)
    setCompactPane('library')
  }

  return (
    <div className="app" data-compact-pane={compactPane}>
      <header className="topbar">
        <div className="brand"><span className="brand-mark" aria-hidden="true"><Archive size={21} /></span>Starchive</div>
        <div className="header-actions">
          <span className="local-status"><HardDrive size={15} aria-hidden="true" /> 本地资料库</span>
          <nav className="compact-nav" aria-label="工作区">
            <button className="icon-button" type="button" aria-pressed={compactPane === 'library'} aria-label="资料库" onClick={() => setCompactPane('library')}><LibraryBig size={18} /></button>
            <button className="icon-button" type="button" aria-pressed={compactPane === 'agent'} aria-label="Agent 会话" onClick={() => setCompactPane('agent')}><MessageSquare size={18} /></button>
          </nav>
          <button className="button header-settings" type="button" aria-label="设置" aria-haspopup="dialog" onClick={openSettings}><Settings size={17} aria-hidden="true" /><span>设置</span></button>
        </div>
      </header>
      <div className="workspace">
        <Library
          selection={selection}
          activeSessionId={activeSessionId}
          contextLocked={agentBusy}
          onSelect={openRepo}
          onContext={(repo) => { setContextRepo(repo); setCompactPane('agent') }}
          onSync={setSyncTimestamp}
          onOpenSettings={openSettings}
        />
        <AgentPane
          contextRepo={contextRepo}
          syncTimestamp={syncTimestamp}
          agentRevision={agentRevision}
          onContext={setContextRepo}
          onCloseContext={() => setContextRepo(null)}
          onSelectRepo={openRepo}
          onBusy={setAgentBusy}
          onSessionChange={setActiveSessionId}
          onOpenSettings={openSettings}
        />
      </div>
      {settingsOpen && <SettingsDialog onClose={() => setSettingsOpen(false)} onDetected={refreshAgents} />}
    </div>
  )
}

createRoot(document.getElementById('root')!).render(<App />)