import { ArrowDownUp, Check, ChevronLeft, ChevronRight, Database, Github, Search, Star, X } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { dispatch, errorMessage, formatTime, type Filters, type Repo, type RepoPage, type StarStatus, type SyncStatus } from './api'
import { RepoDetails, type RepoSelection } from './RepoDetails'
import { Loading, Notice, RepoState } from './ui'

const defaultFilters: Filters = { query: '', language: null, tag: null, status: 'starred' }
const starScopes: { value: StarStatus; label: string }[] = [
  { value: 'starred', label: '已星标' },
  { value: 'unstarred', label: '已取消星标' },
  { value: 'all', label: '全部仓库' },
]
const pageSize = 100

export function Library({ selection, activeSessionId, contextLocked, onSelect, onContext, onSync }: {
  selection: RepoSelection | null
  activeSessionId: string | null
  contextLocked: boolean
  onSelect: (selection: RepoSelection | null) => void
  onContext: (repo: Repo) => void
  onSync: (timestamp: string | null) => void
}) {
  const [filters, setFilters] = useState(defaultFilters)
  const [query, setQuery] = useState('')
  const [offset, setOffset] = useState(0)
  const [page, setPage] = useState<RepoPage | null>(null)
  const [loading, setLoading] = useState(true)
  const [listError, setListError] = useState('')
  const [revision, setRevision] = useState(0)
  const [sync, setSync] = useState<SyncStatus | null>(null)
  const [syncError, setSyncError] = useState<{ message: string; lastSuccess: string | null } | null>(null)
  const [statusError, setStatusError] = useState('')
  const [starting, setStarting] = useState(false)
  const syncGeneration = useRef(0)
  const lastSuccess = sync?.last_success_at ?? null
  const running = starting || sync?.running
  const filtered = !!(query.trim() || filters.language || filters.tag)
  const invocationError = syncError?.lastSuccess === lastSuccess ? syncError.message : ''
  const syncFailure = invocationError || (!running && sync?.last_result?.state === 'error' ? sync.last_result.message : '') || statusError
  const scope = starScopes.find((item) => item.value === filters.status)!

  useEffect(() => {
    const timer = window.setTimeout(() => {
      const value = query.trim()
      if (filters.query !== value) {
        setFilters((current) => ({ ...current, query: value }))
        setOffset(0)
      }
    }, 180)
    return () => window.clearTimeout(timer)
  }, [query, filters.query])

  useEffect(() => {
    let active = true
    setLoading(true)
    setListError('')
    void dispatch('repo_list', { ...filters, offset, limit: pageSize }).then((result) => {
      if (!active) return
      if (offset > 0 && offset >= result.total) {
        setOffset(Math.max(0, Math.floor((result.total - 1) / pageSize) * pageSize))
      } else {
        setPage(result)
        setLoading(false)
      }
    }).catch((error: unknown) => {
      if (!active) return
      setListError(errorMessage(error))
      setLoading(false)
    })
    return () => { active = false }
  }, [filters, offset, revision, lastSuccess])

  useEffect(() => {
    let active = true
    let timer: number
    async function poll() {
      const generation = syncGeneration.current
      try {
        const result = await dispatch('sync_status', {})
        if (active && generation === syncGeneration.current) {
          setSync(result)
          setStatusError('')
        }
      } catch (error) {
        if (active && generation === syncGeneration.current) setStatusError(errorMessage(error))
      } finally {
        if (active) timer = window.setTimeout(() => void poll(), 2500)
      }
    }
    void poll()
    return () => { active = false; window.clearTimeout(timer) }
  }, [])

  useEffect(() => {
    onSync(lastSuccess)
  }, [lastSuccess, onSync])

  useEffect(() => {
    function refresh() { setRevision((current) => current + 1) }
    window.addEventListener('focus', refresh)
    return () => window.removeEventListener('focus', refresh)
  }, [])

  async function startSync() {
    if (running) return
    ++syncGeneration.current
    setStarting(true)
    setSyncError(null)
    try {
      const result = await dispatch('sync_start', {})
      setSync(result)
      setStatusError('')
    } catch (error) {
      setSyncError({ message: errorMessage(error), lastSuccess })
    } finally {
      ++syncGeneration.current
      setStarting(false)
    }
  }

  function changeFilters(next: Partial<Filters>) {
    setOffset(0)
    setFilters((current) => ({ ...current, ...next }))
    onSelect(null)
  }

  function resetFilters() {
    setQuery('')
    changeFilters({ query: '', language: null, tag: null })
  }

  return (
    <>
      <aside className="sidebar" aria-label="资料库筛选">
        <div className="sidebar-controls">
          <div>
            <h2 className="rail-label">资料库</h2>
            <nav className="scope-nav" aria-label="星标状态">
              {starScopes.map(({ value, label }) => (
                <button key={value} type="button" aria-pressed={filters.status === value} onClick={() => changeFilters({ status: value, language: null, tag: null })}>
                  {value === 'starred' ? <Star size={16} /> : value === 'unstarred' ? <Database size={16} /> : <Github size={16} />}
                  {label}
                </button>
              ))}
            </nav>
          </div>
          <div className="filter-group">
            <div className="section-heading">
              <h2 className="rail-label">筛选</h2>
              {filtered && <button className="text-button" type="button" onClick={resetFilters}>清除</button>}
            </div>
            <FilterSelect label="语言" value={filters.language} options={page?.languages ?? []} disabled={!page} onChange={(language) => changeFilters({ language })} />
            <FilterSelect label="标签" value={filters.tag} options={page?.tags ?? []} disabled={!page} onChange={(tag) => changeFilters({ tag })} />
          </div>
        </div>
        <section className="sync" aria-label="GitHub 同步">
          <div className="section-heading">
            <h2 className="rail-label"><Github size={15} /> GitHub 同步</h2>
            {sync?.last_result?.state === 'success' && !running && <Check size={16} className="sync-check" aria-label="最近一次同步成功" />}
          </div>
          <div className="sync-status" aria-live="polite">
            <span>{lastSuccess ? `上次成功 ${formatTime(lastSuccess)}` : sync ? '尚未成功同步' : '等待同步状态'}</span>
            {running && <span>正在同步星标…</span>}
            {!running && sync?.last_result?.state === 'success' && <span>已同步 {sync.last_result.starred_count.toLocaleString('zh-CN')} 个星标</span>}
          </div>
          {syncFailure && <Notice>{syncFailure}</Notice>}
          <button type="button" className="button sync-button" disabled={!!running} onClick={() => void startSync()}>
            <ArrowDownUp size={15} /> {running ? '同步中…' : '同步星标'}
          </button>
        </section>
      </aside>
      <main className="main-pane" id="library">
        {selection ? (
          <RepoDetails
            key={JSON.stringify([selection.id, selection.sessionId])}
            selection={selection}
            activeSessionId={activeSessionId}
            syncTimestamp={lastSuccess}
            revision={revision}
            contextLocked={contextLocked}
            onBack={() => onSelect(null)}
            onContext={onContext}
            onSaved={() => setRevision((current) => current + 1)}
          />
        ) : (
          <div className="repo-list">
            <header className="list-header">
              <div className="section-heading">
                <h1>{scope.label}</h1>
                {page && !loading && !listError && <span className="result-count">{page.total.toLocaleString('zh-CN')} 个仓库</span>}
              </div>
              <div className="search-box">
                <Search size={18} />
                <input type="search" aria-label="搜索仓库" placeholder="搜索仓库名称、描述或笔记" value={query} onChange={(event) => setQuery(event.target.value)} />
                {!!query && <button type="button" className="icon-button" aria-label="清除搜索" onClick={() => setQuery('')}><X size={16} /></button>}
              </div>
            </header>
            {loading && <Loading label="正在读取资料库…" />}
            {listError && <Notice retry={() => setRevision((current) => current + 1)}>{listError}</Notice>}
            {page && !loading && !listError && (
              page.repos.length ? (
                <>
                  <ul className="repo-results">
                    {page.repos.map((repo) => (
                      <li key={repo.id}>
                        <button type="button" className="repo-row" onClick={() => onSelect({ id: repo.id, fallback: repo })}>
                          <div className="repo-row-heading"><strong>{repo.full_name}</strong><ChevronRight size={16} /></div>
                          {repo.description && <p>{repo.description}</p>}
                          <div className="row-meta">
                            {repo.language && <span className="language"><i />{repo.language}</span>}
                            <RepoState repo={repo} />
                            {repo.tags.map((tag) => <span className="tag" key={tag}>{tag}</span>)}
                          </div>
                        </button>
                      </li>
                    ))}
                  </ul>
                  {page.total > pageSize && (
                    <nav className="pagination" aria-label="仓库分页">
                      <button type="button" className="icon-button" aria-label="上一页" disabled={!offset} onClick={() => setOffset(offset - pageSize)}><ChevronLeft size={18} /></button>
                      <span>{offset + 1}–{offset + page.repos.length} / {page.total}</span>
                      <button type="button" className="icon-button" aria-label="下一页" disabled={offset + pageSize >= page.total} onClick={() => setOffset(offset + pageSize)}><ChevronRight size={18} /></button>
                    </nav>
                  )}
                </>
              ) : (
                <div className="empty-state">
                  <span className="empty-icon">{filtered ? <Search size={24} /> : <Star size={24} />}</span>
                  <h2>{filtered ? '没有匹配的仓库' : filters.status === 'unstarred' ? '没有已取消星标的仓库' : '资料库还没有仓库'}</h2>
                  {filtered ? <button type="button" className="button" onClick={resetFilters}>清除筛选条件</button> : filters.status !== 'unstarred' && (
                    <p>同步 GitHub 星标后，即可在本地搜索和整理。</p>
                  )}
                </div>
              )
            )}
          </div>
        )}
      </main>
    </>
  )
}

function FilterSelect({ label, value, options, disabled, onChange }: {
  label: string
  value: string | null
  options: string[]
  disabled: boolean
  onChange: (value: string | null) => void
}) {
  const choices = value && !options.includes(value) ? [value, ...options] : options
  return (
    <label className="filter-label">
      <span>{label}</span>
      <select disabled={disabled} value={value ?? ''} onChange={(event) => onChange(event.target.value || null)}>
        <option value="">全部{label}</option>
        {choices.map((choice) => <option key={choice}>{choice}</option>)}
      </select>
    </label>
  )
}
