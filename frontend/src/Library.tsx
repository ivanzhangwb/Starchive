import { ChevronLeft, ChevronRight, Database, Github, Notebook, Search, Star, X } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { dispatch, errorMessage, type Filters, type Repo, type RepoPage, type StarStatus } from './api'
import { RepoDetails, type RepoSelection } from './RepoDetails'
import { SyncPanel, useLibrarySync } from './SyncPanel'
import { Loading, Notice, RepoState } from './ui'

const defaultFilters: Filters = { query: '', language: null, tag: null, status: 'starred' }
const starScopes: { value: StarStatus; label: string }[] = [
  { value: 'starred', label: '已星标' },
  { value: 'unstarred', label: '已取消星标' },
  { value: 'all', label: '全部仓库' },
]
const pageSize = 100

export function Library({ selection, activeSessionId, contextLocked, onSelect, onContext, onSync, onOpenSettings }: {
  selection: RepoSelection | null
  activeSessionId: string | null
  contextLocked: boolean
  onSelect: (selection: RepoSelection | null) => void
  onContext: (repo: Repo) => void
  onSync: (timestamp: string | null) => void
  onOpenSettings: () => void
}) {
  const [filters, setFilters] = useState(defaultFilters)
  const [query, setQuery] = useState('')
  const [offset, setOffset] = useState(0)
  const [page, setPage] = useState<RepoPage | null>(null)
  const [loading, setLoading] = useState(true)
  const [listError, setListError] = useState('')
  const [revision, setRevision] = useState(0)
  const refreshLibrary = useCallback(() => setRevision((current) => current + 1), [])
  const sync = useLibrarySync(refreshLibrary)
  const lastSuccess = sync.status?.last_success_at ?? null
  const filtered = !!(query.trim() || filters.language || filters.tag)
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
    onSync(lastSuccess)
  }, [lastSuccess, onSync])

  useEffect(() => {
    window.addEventListener('focus', refreshLibrary)
    return () => window.removeEventListener('focus', refreshLibrary)
  }, [refreshLibrary])

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
      <aside className="sidebar" aria-label="资料库控制">
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
        <SyncPanel sync={sync} onOpenSettings={onOpenSettings} />
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
            onSaved={refreshLibrary}
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
            {listError && <Notice retry={refreshLibrary}>{listError}</Notice>}
            {page && !loading && !listError && (
              page.repos.length ? (
                <>
                  <ul className="repo-results" role="list">
                    {page.repos.map((repo) => (
                      <li key={repo.id}>
                        <button type="button" className="repo-row" onClick={() => onSelect({ id: repo.id, fallback: repo })}>
                          <span className="repo-mark" aria-hidden="true"><Notebook size={17} /></span>
                          <span className="repo-row-content">
                            <strong className="repo-row-name">{repo.full_name}</strong>
                            {repo.description && <span className="repo-description">{repo.description}</span>}
                            <span className="row-meta">
                              {repo.language && <span className="language"><i />{repo.language}</span>}
                              <RepoState repo={repo} />
                              {repo.tags.map((tag) => <span className="tag" key={tag}>{tag}</span>)}
                            </span>
                          </span>
                          <ChevronRight className="repo-chevron" size={17} aria-hidden="true" />
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
