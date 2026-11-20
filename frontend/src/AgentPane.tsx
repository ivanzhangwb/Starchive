import { ArrowRight, BookMarked, ChevronRight, Github, MessageSquare, Plus, Send, Settings2, X } from 'lucide-react'
import { useEffect, useRef, useState, type FormEvent } from 'react'
import { dispatch, errorMessage, formatTime, type Agent, type Candidate, type ContextRepo, type Repo, type Session, type SessionMessage, type SessionSummary } from './api'
import type { RepoSelection } from './RepoDetails'
import { Dialog, Loading, Markdown, Notice, RepoState } from './ui'

const activeSessionKey = 'starchive.active-session'
const evidenceLabels: Record<string, string> = {
  description: '仓库描述',
  readme: 'README',
  note: '个人笔记',
  repository_name: '仓库名称',
  language: '语言',
  topics: '主题',
}

type Operation = 'session' | 'agent' | 'context' | 'turn' | 'search'

function rememberSession(id?: string) {
  try {
    if (id) localStorage.setItem(activeSessionKey, id)
    return localStorage.getItem(activeSessionKey)
  } catch {
    return null
  }
}

export function AgentPane({ contextRepo, syncTimestamp, onContext, onCloseContext, onSelectRepo, onBusy, onSessionChange }: {
  contextRepo: Repo | null
  syncTimestamp: string | null
  onContext: (repo: Repo) => void
  onCloseContext: () => void
  onSelectRepo: (selection: RepoSelection) => void
  onBusy: (busy: boolean) => void
  onSessionChange: (id: string | null) => void
}) {
  const [agents, setAgents] = useState<Agent[]>([])
  const [sessions, setSessions] = useState<SessionSummary[]>([])
  const [session, setSession] = useState<Session | null>(null)
  const [preferredAgent, setPreferredAgent] = useState('')
  const [loading, setLoading] = useState(true)
  const [retry, setRetry] = useState(0)
  const [loadError, setLoadError] = useState('')
  const [agentsError, setAgentsError] = useState('')
  const [busy, setBusy] = useState<Operation | null>(null)
  const [failure, setFailure] = useState<{ operation: Operation; message: string } | null>(null)
  const [drafts, setDrafts] = useState<Record<string, string>>({})
  const [searchTerms, setSearchTerms] = useState('')
  const [searchOpen, setSearchOpen] = useState(false)
  const history = useRef<HTMLDivElement>(null)
  const currentId = useRef<string | null>(null)
  const operationPending = useRef(false)
  const refreshedTimestamp = useRef<string | null>(null)
  const draftKey = session?.id ?? ''
  const draft = drafts[draftKey] ?? ''
  const agentId = session?.agent_id ?? (!session ? preferredAgent : '')
  const selectedAgent = agents.find((agent) => agent.id === agentId)
  const locked = loading || busy !== null

  function acceptSession(next: Session) {
    currentId.current = next.id
    rememberSession(next.id)
    setSession(next)
    setSessions((current) => [
      { id: next.id, title: next.title, updated_at: next.updated_at },
      ...current.filter((item) => item.id !== next.id),
    ].sort((a, b) => b.updated_at.localeCompare(a.updated_at)))
  }

  useEffect(() => {
    let active = true
    setLoading(true)
    setLoadError('')
    setAgentsError('')
    void Promise.allSettled([dispatch('agent_list', {}), dispatch('session_list', {})]).then(async ([available, saved]) => {
      if (!active) return
      if (available.status === 'fulfilled') {
        setAgents(available.value)
        setPreferredAgent((current) => current || available.value.find((agent) => agent.available)?.id || '')
      } else {
        setAgentsError(errorMessage(available.reason))
      }
      if (saved.status === 'rejected') {
        setLoadError(errorMessage(saved.reason))
      } else {
        setSessions(saved.value)
        const target = saved.value.find((item) => item.id === rememberSession()) ?? saved.value[0]
        if (target) {
          try {
            const restored = await dispatch('session_get', { session_id: target.id })
            if (active) acceptSession(restored)
          } catch (error) {
            if (active) setLoadError(errorMessage(error))
          }
        }
      }
      if (active) setLoading(false)
    })
    return () => { active = false }
  }, [retry])

  useEffect(() => { onBusy(locked) }, [locked, onBusy])
  useEffect(() => { onSessionChange(session?.id ?? null) }, [session?.id, onSessionChange])

  useEffect(() => {
    if (!session || !syncTimestamp || locked || refreshedTimestamp.current === syncTimestamp) return
    let active = true
    void dispatch('session_get', { session_id: session.id }).then((updated) => {
      if (active) {
        refreshedTimestamp.current = syncTimestamp
        acceptSession(updated)
      }
    }).catch(() => {
      // The open conversation remains readable during a background refresh failure.
    })
    return () => { active = false }
  }, [locked, session?.id, syncTimestamp])

  useEffect(() => {
    if (history.current) history.current.scrollTop = history.current.scrollHeight
  }, [session?.id, session?.messages.length, busy])

  async function ensureSession() {
    if (session) return session
    const next = await dispatch('session_create', { agent_id: agentId || null })
    if (draft) setDrafts((current) => ({ ...current, '': '', [next.id]: draft }))
    acceptSession(next)
    return next
  }

  async function run(operation: Operation, execute: () => Promise<Session>) {
    if (locked || operationPending.current) return null
    operationPending.current = true
    setBusy(operation)
    setFailure(null)
    try {
      const next = await execute()
      acceptSession(next)
      return next
    } catch (error) {
      setFailure({ operation, message: errorMessage(error) })
      if (operation !== 'session' && currentId.current) {
        try {
          acceptSession(await dispatch('session_get', { session_id: currentId.current }))
        } catch {
          // Keep the last readable snapshot when a recovery read also fails.
        }
      }
      return null
    } finally {
      operationPending.current = false
      setBusy(null)
    }
  }

  async function send(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const text = draft.trim()
    if (!text || !selectedAgent?.available) return
    const next = await run('turn', async () => {
      const active = await ensureSession()
      return dispatch('agent_send', { session_id: active.id, text })
    })
    if (next) setDrafts((current) => ({ ...current, [next.id]: '' }))
  }

  async function expand(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const publicQuery = searchTerms.trim()
    if (!session?.can_expand || !publicQuery) return
    if (await run('search', () => dispatch('agent_expand', { session_id: session.id, public_query: publicQuery }))) {
      setSearchOpen(false)
      setSearchTerms('')
    }
  }

  function closeContext() {
    setFailure((current) => current?.operation === 'context' ? null : current)
    onCloseContext()
  }

  async function saveContext(repo: Repo, permissions: Pick<ContextRepo, 'include_private' | 'include_notes'>) {
    if (await run('context', async () => {
      const active = await ensureSession()
      return dispatch('session_set_context', { session_id: active.id, repo_id: repo.id, ...permissions })
    })) closeContext()
  }

  function showCandidate(repo: Repo) {
    if (session) onSelectRepo({ id: repo.id, fallback: repo, sessionId: session.id })
  }

  const retryLoad = () => setRetry((value) => value + 1)
  const existingContext = session?.context.find((item) => item.repo.id === contextRepo?.id)
  const visibleFailure = failure && !(failure.operation === 'context' && contextRepo) && !(failure.operation === 'search' && searchOpen)

  return (
    <aside className="agent-pane" id="agent" aria-label="Agent 会话">
      <header className="agent-header">
        <div className="section-heading">
          <h2><MessageSquare size={19} /> Agent 会话</h2>
          <button type="button" className="button small" disabled={locked} onClick={() => void run('session', () => dispatch('session_create', { agent_id: agentId || null }))}>
            <Plus size={15} /> 新建会话
          </button>
        </div>
        <div className="agent-controls">
          <label>
            <span>历史会话</span>
            <select value={session?.id ?? ''} disabled={locked || !sessions.length} onChange={(event) => {
              const id = event.target.value
              if (id) void run('session', () => dispatch('session_get', { session_id: id }))
            }}>
              <option value="" disabled>{sessions.length ? '选择会话' : '暂无会话'}</option>
              {sessions.map((item) => <option value={item.id} key={item.id}>{item.title} · {formatTime(item.updated_at)}</option>)}
            </select>
          </label>
          <label>
            <span>本地 Agent</span>
            <select value={agentId} disabled={locked} onChange={(event) => {
              const id = event.target.value
              if (session) void run('agent', () => dispatch('session_set_agent', { session_id: session.id, agent_id: id }))
              else setPreferredAgent(id)
            }}>
              <option value="" disabled>选择 Agent</option>
              {agentId && !selectedAgent && <option value={agentId} disabled>{agentId}（未找到）</option>}
              {agents.map((agent) => <option value={agent.id} key={agent.id} disabled={!agent.available}>{agent.name}{agent.available ? '' : '（未安装）'}</option>)}
            </select>
          </label>
        </div>
        <p className="agent-disclosure">本地 Agent 的模型推理可能联网。</p>
        {loadError && <Notice retry={retryLoad}>{loadError}</Notice>}
      </header>
      <section className="context-bar" aria-label="对话上下文">
        <div className="context-heading"><BookMarked size={15} /><h3>对话上下文</h3></div>
        {session?.context.length ? (
          <ul className="context-list">
            {session.context.map(({ repo, include_private, include_notes }) => (
              <li className="context-chip" key={repo.id}>
                <button type="button" disabled={locked} onClick={() => onContext(repo)} title={`编辑 ${repo.full_name} 的授权`}>
                  <Settings2 size={13} /><span>{repo.full_name}</span>
                  {include_private && <small>私有</small>}
                  {include_notes && <small>笔记</small>}
                </button>
                <button type="button" disabled={locked} aria-label={`移除 ${repo.full_name}`} onClick={() => void run('context', () => dispatch('session_remove_context', { session_id: session.id, repo_id: repo.id }))}>
                  <X size={14} />
                </button>
              </li>
            ))}
          </ul>
        ) : <span className="muted">从仓库详情添加比较对象</span>}
      </section>
      <div className="agent-history" ref={history} role="log" aria-label="会话消息" aria-live="polite">
        {loading ? <Loading label="正在恢复对话…" /> : session?.messages.length ? (
          session.messages.map((message) => <Message key={message.id} message={message} onSelectRepo={showCandidate} />)
        ) : (
          <div className="agent-empty">
            <span className="empty-icon"><MessageSquare size={23} /></span>
            <h3>从星标库找候选</h3>
            <p>描述你想实现的效果，比如为桌面应用增加中文离线搜索。</p>
          </div>
        )}
        {(busy === 'turn' || busy === 'search') && <p className="working" role="status">{busy === 'turn' ? 'Agent 正在分析…' : '正在搜索 GitHub…'}</p>}
      </div>
      <div className="agent-footer">
        {visibleFailure && <Notice>{failure.message}</Notice>}
        {agentsError ? <Notice retry={retryLoad}>{agentsError}</Notice> : !loading && !agents.some((agent) => agent.available) ? (
          <Notice retry={retryLoad}>未找到可用的本地 Agent。安装 pi 或检查可执行文件配置后，请重试检测。</Notice>
        ) : !loading && agentId && !selectedAgent?.available ? (
          <Notice>当前 Agent 不可用。请选择已安装的 Agent。</Notice>
        ) : null}
        {session?.can_expand && (
          <button type="button" className="button expand-button" disabled={locked} onClick={() => { setFailure(null); setSearchTerms(''); setSearchOpen(true) }}>
            <Github size={16} /> 扩展到 GitHub 搜索 <ArrowRight size={15} />
          </button>
        )}
        <form className="composer" onSubmit={(event) => void send(event)}>
          <label htmlFor="agent-message" className="sr-only">对话消息</label>
          <textarea id="agent-message" rows={2} value={draft} disabled={locked} placeholder="描述需求，或继续比较候选…" onChange={(event) => setDrafts((current) => ({ ...current, [draftKey]: event.target.value }))} />
          <button type="submit" className="button primary send-button" disabled={locked || !draft.trim() || !selectedAgent?.available}>
            <Send size={15} /> 发送
          </button>
        </form>
      </div>
      {contextRepo && (
        <ContextDialog
          key={contextRepo.id}
          repo={contextRepo}
          existing={existingContext}
          busy={locked}
          error={failure?.operation === 'context' ? failure.message : undefined}
          onClose={closeContext}
          onSave={saveContext}
        />
      )}
      {searchOpen && (
        <Dialog title="扩展到 GitHub 搜索" busy={locked} onClose={() => { setSearchOpen(false); setFailure(null) }}>
          <form className="dialog-form" onSubmit={(event) => void expand(event)}>
            <p>输入与当前需求相关的公开关键词。确认后，GitHub 将收到这些关键词并执行一次搜索。</p>
            <label>公开搜索词<input autoFocus value={searchTerms} disabled={locked} onChange={(event) => setSearchTerms(event.target.value)} required /></label>
            {failure?.operation === 'search' && <Notice>{failure.message}</Notice>}
            <div className="button-row">
              <button className="button primary" disabled={locked || !searchTerms.trim()}><Github size={16} /> {busy === 'search' ? '正在搜索…' : '搜索一次'}</button>
              <button className="button" type="button" disabled={locked} onClick={() => { setSearchOpen(false); setFailure(null) }}>取消</button>
            </div>
          </form>
        </Dialog>
      )}
    </aside>
  )
}

function Message({ message, onSelectRepo }: { message: SessionMessage; onSelectRepo: (repo: Repo) => void }) {
  return (
    <article className={`message message-${message.role}`}>
      <h3>{message.role === 'user' ? '你' : 'Agent'}</h3>
      {message.text && (message.role === 'user' ? <p className="user-text">{message.text}</p> : <Markdown>{message.text}</Markdown>)}
      {!!message.candidates.length && <ul className="candidates">
        {message.candidates.map((candidate) => <CandidateCard key={candidate.repo.id} candidate={candidate} onSelect={() => onSelectRepo(candidate.repo)} />)}
      </ul>}
      <Gaps gaps={message.gaps} />
    </article>
  )
}

function CandidateCard({ candidate, onSelect }: { candidate: Candidate; onSelect: () => void }) {
  const { repo, fit, evidence, gaps } = candidate
  return (
    <li className="candidate">
      <button type="button" className="candidate-title" onClick={onSelect}>
        <strong>{repo.full_name}</strong><ChevronRight size={16} />
      </button>
      <div className="row-meta"><RepoState repo={repo} />{repo.language && <span className="language"><i />{repo.language}</span>}</div>
      {fit && <p className="candidate-fit">{fit}</p>}
      {!!evidence.length && <div className="candidate-evidence">
        <h4>资料依据</h4>
        {evidence.map(({ source, excerpt }, index) => (
          <blockquote key={index}><strong>{evidenceLabels[source] ?? source}</strong><p>{excerpt}</p></blockquote>
        ))}
      </div>}
      <Gaps gaps={gaps} />
    </li>
  )
}

function Gaps({ gaps }: { gaps: string[] }) {
  if (!gaps.length) return null
  return (
    <div className="gaps">
      <h4>待确认</h4>
      <ul>{gaps.map((gap, index) => <li key={index}>{gap}</li>)}</ul>
    </div>
  )
}

function ContextDialog({ repo, existing, busy, error, onClose, onSave }: {
  repo: Repo
  existing?: ContextRepo
  busy: boolean
  error?: string
  onClose: () => void
  onSave: (repo: Repo, permissions: Pick<ContextRepo, 'include_private' | 'include_notes'>) => Promise<void>
}) {
  const [includePrivate, setIncludePrivate] = useState(existing?.include_private ?? false)
  const [includeNotes, setIncludeNotes] = useState(existing?.include_notes ?? false)
  return (
    <Dialog title={existing ? '编辑对话上下文' : '加入对话上下文'} busy={busy} onClose={onClose}>
      <form className="dialog-form" onSubmit={(event) => {
        event.preventDefault()
        void onSave(repo, { include_private: repo.private && includePrivate, include_notes: includeNotes })
      }}>
        <p className="dialog-repo">{repo.full_name}</p>
        <p>仅对本次会话授权。所选资料可能随 Agent 的模型推理发送到远程服务。</p>
        {repo.private && <label className="checkbox-label"><input type="checkbox" checked={includePrivate} disabled={busy} onChange={(event) => setIncludePrivate(event.target.checked)} />允许 Agent 读取此私有仓库的资料</label>}
        <label className="checkbox-label"><input type="checkbox" checked={includeNotes} disabled={busy} onChange={(event) => setIncludeNotes(event.target.checked)} />允许 Agent 读取此仓库的个人笔记</label>
        {error && <Notice>{error}</Notice>}
        <div className="button-row">
          <button className="button primary" disabled={busy}>{busy ? '正在保存…' : existing ? '保存授权' : '加入会话'}</button>
          <button type="button" className="button" disabled={busy} onClick={onClose}>取消</button>
        </div>
      </form>
    </Dialog>
  )
}
