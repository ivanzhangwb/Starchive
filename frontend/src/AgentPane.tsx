import { ArrowRight, BookMarked, ChevronRight, Github, MessageSquare, Plus, Send, Settings2, X } from 'lucide-react'
import { memo, useCallback, useEffect, useLayoutEffect, useRef, useState, type FormEvent, type KeyboardEvent, type ReactNode } from 'react'
import { dispatch, errorMessage, formatTime, type Agent, type Candidate, type ContextRepo, type Repo, type SessionMessage } from './api'
import type { RepoSelection } from './RepoDetails'
import { Dialog, Loading, Markdown, Notice, RepoState } from './ui'
import { useChatSession } from './useChatSession'

const bottomThreshold = 48
const evidenceLabels: Record<string, string> = {
  description: '仓库描述',
  readme: 'README',
  note: '个人笔记',
  repository_name: '仓库名称',
  language: '语言',
  topics: '主题',
}

export function AgentPane({ contextRepo, syncTimestamp, agentRevision, onContext, onCloseContext, onSelectRepo, onBusy, onSessionChange, onOpenSettings }: {
  contextRepo: Repo | null
  syncTimestamp: string | null
  agentRevision: number
  onContext: (repo: Repo) => void
  onCloseContext: () => void
  onSelectRepo: (selection: RepoSelection) => void
  onBusy: (busy: boolean) => void
  onSessionChange: (id: string | null) => void
  onOpenSettings: () => void
}) {
  const [availability, setAvailability] = useState<{ revision: number; agents?: Agent[]; error?: string } | null>(null)
  const [preferredAgent, setPreferredAgent] = useState('')
  const [search, setSearch] = useState<{ key: string; terms: string } | null>(null)
  const chat = useChatSession(preferredAgent, syncTimestamp)
  const { key, session, busy, turn, draft, failure } = chat
  const searchDialog = search?.key === key ? search : null
  const searchFailure = failure?.operation === 'search' ? failure.message : undefined
  const contextFailure = contextRepo && failure?.operation === 'context' && failure.repoId === contextRepo.id
    ? failure.message : undefined
  const history = useRef<HTMLDivElement>(null)
  const composer = useRef<HTMLTextAreaElement>(null)
  const composing = useRef(false)
  const positions = useRef(new Map<string, { top: number; follow: boolean }>())
  const previousKey = useRef(key)
  const currentContext = useRef(contextRepo)
  const agentsLoading = availability?.revision !== agentRevision
  const agents = availability?.agents
  const agentId = session ? session.agent_id ?? '' : preferredAgent
  const selectedAgent = agents?.find((agent) => agent.id === agentId)
  const locked = chat.restoring || chat.missingSession || agentsLoading || busy !== null
  const canSend = !locked && selectedAgent?.available
  const loadingHistory = chat.restoring || chat.loadingSession
  const searching = busy === 'search'

  useEffect(() => {
    let active = true
    void dispatch('agent_list', {}).then((available) => {
      if (!active) return
      setAvailability({ revision: agentRevision, agents: available })
      setPreferredAgent((current) => available.some((agent) => agent.id === current && agent.available)
        ? current : available.find((agent) => agent.available)?.id ?? '')
    }).catch((error: unknown) => {
      if (active) setAvailability({ revision: agentRevision, error: errorMessage(error) })
    })
    return () => { active = false }
  }, [agentRevision])

  useEffect(() => { onBusy(locked) }, [locked, onBusy])
  useEffect(() => { onSessionChange(session?.id ?? null) }, [session?.id, onSessionChange])
  useLayoutEffect(() => { currentContext.current = contextRepo }, [contextRepo])

  useLayoutEffect(() => {
    const view = history.current
    const position = positions.current.get(key)
    if (view) {
      if (position?.follow !== false) view.scrollTop = view.scrollHeight
      else if (previousKey.current !== key) view.scrollTop = position.top
    }
    if (previousKey.current !== key) composing.current = false
    previousKey.current = key
  }, [key, session?.messages, turn, searching, loadingHistory])

  useEffect(() => {
    const view = history.current
    if (!view) return
    const observer = new ResizeObserver(() => {
      if (view.clientHeight && positions.current.get(key)?.follow !== false) view.scrollTop = view.scrollHeight
    })
    observer.observe(view)
    return () => observer.disconnect()
  }, [key])

  function followLatest() {
    positions.current.delete(key)
    composer.current?.focus({ preventScroll: true })
  }

  function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (canSend && !composing.current) void chat.send(false, followLatest)
  }

  function composeKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key !== 'Enter' || event.shiftKey) return
    if (composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
    event.preventDefault()
    if (!event.repeat) event.currentTarget.form?.requestSubmit()
  }

  async function expand(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    const publicQuery = searchDialog?.terms.trim()
    if (locked || !session?.can_expand || !publicQuery) return
    if (await chat.mutate('search', (active) => dispatch('agent_expand', { session_id: active.id, public_query: publicQuery }))) {
      setSearch((current) => current === searchDialog ? null : current)
    }
  }

  function closeSearch() {
    chat.clearFailure('search')
    setSearch(null)
  }

  function closeContext() {
    if (contextRepo) chat.clearFailure('context', contextRepo.id)
    onCloseContext()
  }

  async function saveContext(repo: Repo, permissions: Pick<ContextRepo, 'include_private' | 'include_notes'>) {
    if (locked) return
    if (await chat.mutate('context', (active) => dispatch('session_set_context', { session_id: active.id, repo_id: repo.id, ...permissions }), repo.id)
      && currentContext.current === repo) closeContext()
  }

  const sessionId = session?.id
  const showCandidate = useCallback((repo: Repo) => {
    if (sessionId) onSelectRepo({ id: repo.id, fallback: repo, sessionId })
  }, [sessionId, onSelectRepo])

  const selectedSession = sessionId ?? key
  const existingContext = session?.context.find((item) => item.repo.id === contextRepo?.id)
  const visibleFailure = failure && failure.operation !== 'load' && !contextFailure && !(searchDialog && searchFailure)
  const agentUnavailable = agents && !agents.some((agent) => agent.available)
  const agentIssue = agentsLoading ? '' : availability?.error || (agentUnavailable
    ? '未找到可用的本地 Agent。请在设置中检测或指定 pi 的路径。'
    : agents && agentId && !selectedAgent?.available ? '当前 Agent 不可用。请检查设置或选择已安装的 Agent。' : '')

  return (
    <aside className="agent-pane" id="agent" aria-label="Agent 会话">
      <header className="agent-header">
        <div className="section-heading">
          <h2><MessageSquare size={19} /> Agent 会话</h2>
          <button type="button" className="button small" disabled={chat.restoring || agentsLoading || busy === 'create'} onClick={() => void chat.create(agentId || preferredAgent)}>
            <Plus size={15} /> 新建会话
          </button>
        </div>
        <div className="agent-controls">
          <label>
            <span>历史会话</span>
            <select value={selectedSession} disabled={chat.restoring || !chat.options.length} onChange={(event) => void chat.select(event.currentTarget.value)}>
              {!chat.options.some((item) => item.id === selectedSession) && <option value={selectedSession} disabled>{chat.options.length ? '选择会话' : '暂无会话'}</option>}
              {chat.options.map((item) => <option value={item.id} key={item.id}>{item.title}{item.updated_at && ` · ${formatTime(item.updated_at)}`}</option>)}
            </select>
          </label>
          <label>
            <span>本地 Agent</span>
            <select value={agentId} disabled={locked} onChange={(event) => {
              const id = event.target.value
              if (session) void chat.mutate('agent', (active) => dispatch('session_set_agent', { session_id: active.id, agent_id: id }))
              else setPreferredAgent(id)
            }}>
              <option value="" disabled>选择 Agent</option>
              {agentId && !selectedAgent && <option value={agentId} disabled>{agentId}{agents ? '（未找到）' : ''}</option>}
              {agents?.map((agent) => <option value={agent.id} key={agent.id} disabled={!agent.available}>{agent.name}{agent.available ? '' : '（不可用）'}</option>)}
            </select>
          </label>
        </div>
        <p className="agent-disclosure">本地 Agent 的模型推理可能联网。</p>
        {chat.restoreError && <Notice retry={chat.retryRestore}>{chat.restoreError}</Notice>}
        {failure?.operation === 'load' && <Notice retry={() => void chat.select(selectedSession)}>{failure.message}</Notice>}
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
                <button type="button" disabled={locked} aria-label={`移除 ${repo.full_name}`} onClick={() => void chat.mutate('context', (active) => dispatch('session_remove_context', { session_id: active.id, repo_id: repo.id }))}>
                  <X size={14} />
                </button>
              </li>
            ))}
          </ul>
        ) : <span className="muted">从仓库详情添加比较对象</span>}
      </section>
      <div className="agent-history" ref={history} role="log" aria-label="会话消息" aria-relevant="additions" onScroll={(event) => {
        const { scrollTop, scrollHeight, clientHeight } = event.currentTarget
        if (clientHeight) positions.current.set(key, { top: scrollTop, follow: scrollHeight - clientHeight - scrollTop <= bottomThreshold })
      }}>
        {loadingHistory && <Loading label="正在恢复对话…" />}
        {session?.messages.map((message) => <Message key={`${key}:${message.id}`} message={message} onSelectRepo={showCandidate} />)}
        {!loadingHistory && !chat.missingSession && !session?.messages.length && !turn && (
          <div className="agent-empty">
            <span className="empty-icon"><MessageSquare size={23} /></span>
            <h3>从星标库找候选</h3>
            <p>描述你想实现的效果，比如为桌面应用增加中文离线搜索。</p>
          </div>
        )}
        {turn && (
          <ChatMessage key={`${key}:submitted`} role="user" variant={turn.status === 'failed' ? 'chat-failed' : undefined}>
            <p className="user-text">{turn.text}</p>
            {turn.status === 'failed' && <div className="chat-error">
              <p role="alert">发送失败。{turn.error}</p>
              <button type="button" className="button small" disabled={!canSend} onClick={() => void chat.send(true, followLatest)}>重试</button>
              <button type="button" className="text-button" disabled={locked} onClick={chat.discardTurn}>放弃重试</button>
            </div>}
          </ChatMessage>
        )}
        {turn?.status === 'pending' && (
          <ChatMessage key={`${key}:pending`} role="assistant" variant="chat-pending">
            {turn.reply ? <p className="chat-stream">{turn.reply}</p> : <p className="chat-status">正在生成回复…</p>}
          </ChatMessage>
        )}
        {searching && <p className="working" role="status">正在搜索 GitHub…</p>}
      </div>
      <div className="agent-footer">
        {visibleFailure && <Notice>{failure.message}</Notice>}
        {agentIssue && (
          <div className="settings-callout">
            <Notice>{agentIssue}</Notice>
            <button type="button" className="text-button" aria-haspopup="dialog" onClick={onOpenSettings}>打开 Agent 设置</button>
          </div>
        )}
        {session?.can_expand && (
          <button type="button" className="button expand-button" disabled={locked} onClick={() => { chat.clearFailure('search'); setSearch({ key, terms: '' }) }}>
            <Github size={16} /> 扩展到 GitHub 搜索 <ArrowRight size={15} />
          </button>
        )}
        <form className="composer" onSubmit={submit}>
          <label htmlFor="agent-message" className="sr-only">对话消息</label>
          <textarea
            key={key}
            id="agent-message"
            ref={composer}
            rows={2}
            value={draft}
            disabled={loadingHistory || chat.missingSession}
            placeholder="描述需求，或继续比较候选…"
            onChange={(event) => chat.setDraft(event.currentTarget.value)}
            onCompositionStart={() => { composing.current = true }}
            onCompositionEnd={() => { composing.current = false }}
            onKeyDown={composeKeyDown}
          />
          <button type="submit" className="button primary send-button" disabled={!canSend || !!turn || !draft.trim()}>
            <Send size={15} /> 发送
          </button>
        </form>
      </div>
      {contextRepo && (
        <ContextDialog
          key={`${key}:${contextRepo.id}`}
          repo={contextRepo}
          existing={existingContext}
          busy={locked}
          error={contextFailure}
          onClose={closeContext}
          onSave={saveContext}
        />
      )}
      {searchDialog && (
        <Dialog title="扩展到 GitHub 搜索" busy={locked} onClose={closeSearch}>
          <form className="dialog-form" onSubmit={(event) => void expand(event)}>
            <p>输入与当前需求相关的公开关键词。确认后，GitHub 将收到这些关键词并执行一次搜索。</p>
            <label>公开搜索词<input autoFocus value={searchDialog.terms} disabled={locked} onChange={(event) => setSearch({ key, terms: event.currentTarget.value })} required /></label>
            {searchFailure && <Notice>{searchFailure}</Notice>}
            <div className="button-row">
              <button className="button primary" disabled={locked || !searchDialog.terms.trim()}><Github size={16} /> {searching ? '正在搜索…' : '搜索一次'}</button>
              <button className="button" type="button" disabled={locked} onClick={closeSearch}>取消</button>
            </div>
          </form>
        </Dialog>
      )}
    </aside>
  )
}

function ChatMessage({ role, variant, children }: { role: SessionMessage['role']; variant?: 'chat-pending' | 'chat-failed'; children: ReactNode }) {
  return (
    <article className={`chat-message chat-${role}${variant ? ` ${variant}` : ''}`} aria-live={variant === 'chat-pending' ? 'off' : undefined}>
      <h3 className="chat-author">{role === 'user' ? '你' : 'AI 助手'}</h3>
      <div className="chat-bubble">{children}</div>
    </article>
  )
}

const Message = memo(function Message({ message, onSelectRepo }: { message: SessionMessage; onSelectRepo: (repo: Repo) => void }) {
  return (
    <ChatMessage role={message.role}>
      {message.text && (message.role === 'user' ? <p className="user-text">{message.text}</p> : <Markdown>{message.text}</Markdown>)}
      {!!message.candidates.length && <ul className="candidates">
        {message.candidates.map((candidate) => <CandidateCard key={candidate.repo.id} candidate={candidate} onSelect={() => onSelectRepo(candidate.repo)} />)}
      </ul>}
      <Gaps gaps={message.gaps} />
    </ChatMessage>
  )
})

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
