import { ArrowLeft, ArrowUpRight, BookOpen, Pencil, Plus, Save } from 'lucide-react'
import { useEffect, useState, type FormEvent } from 'react'
import { dispatch, errorMessage, type LibraryRepo, type Repo, type RepoDetail } from './api'
import { Loading, Markdown, Notice, RepoState } from './ui'

export interface RepoSelection {
  id: string
  fallback?: Repo
  sessionId?: string
}

function normalizedTags(text: string) {
  const seen = new Set<string>()
  return text.split('\n').map((tag) => tag.trim()).filter((tag) => {
    const key = tag.replace(/[A-Z]/g, (letter) => letter.toLowerCase())
    if (!tag || seen.has(key)) return false
    seen.add(key)
    return true
  })
}

function sameTags(first: string[], second: string[]) {
  return first.length === second.length && first.every((tag, index) => tag === second[index])
}

export function RepoDetails({ selection, activeSessionId, syncTimestamp, revision, contextLocked, onBack, onContext, onSaved }: {
  selection: RepoSelection
  activeSessionId: string | null
  syncTimestamp: string | null
  revision: number
  contextLocked: boolean
  onBack: () => void
  onContext: (repo: Repo) => void
  onSaved: () => void
}) {
  const [detail, setDetail] = useState<RepoDetail | null>(null)
  const [readme, setReadme] = useState<string | null>(null)
  const [readmeChecked, setReadmeChecked] = useState(false)
  const [loading, setLoading] = useState(true)
  const [retry, setRetry] = useState(0)
  const [loadError, setLoadError] = useState('')
  const [readmeError, setReadmeError] = useState('')
  const [openError, setOpenError] = useState('')
  const [saveError, setSaveError] = useState('')
  const [fetching, setFetching] = useState(false)
  const [saving, setSaving] = useState(false)
  const [draft, setDraft] = useState<{ base: LibraryRepo; note: string; tags: string } | null>(null)
  const [conflict, setConflict] = useState<LibraryRepo | null>(null)
  const { id, fallback, sessionId } = selection
  const repo = detail?.repo ?? fallback
  const otherSession = !!sessionId && sessionId !== activeSessionId && !detail?.editable

  useEffect(() => {
    let active = true
    setLoading(true)
    setLoadError('')
    void dispatch('repo_get', { repo_id: id, session_id: sessionId ?? null }).then((result) => {
      if (!active) return
      setDetail(result)
      setReadme(result?.readme ?? null)
      if (!result && !fallback) setLoadError('本地资料库中未找到该仓库。')
    }).catch((error: unknown) => {
      if (active) setLoadError(errorMessage(error))
    }).finally(() => {
      if (active) setLoading(false)
    })
    return () => { active = false }
  }, [id, sessionId, fallback, retry, syncTimestamp, revision])

  async function save(event: FormEvent<HTMLFormElement>) {
    event.preventDefault()
    if (!draft || conflict || saving || loading) return
    const tags = normalizedTags(draft.tags)
    const noteEdit = draft.note === draft.base.note ? undefined : { expected: draft.base.note, value: draft.note }
    const tagsEdit = sameTags(tags, draft.base.tags) ? undefined : { expected: draft.base.tags, value: tags }
    if (!noteEdit && !tagsEdit) {
      setDraft(null)
      setSaveError('')
      return
    }
    setSaving(true)
    setSaveError('')
    try {
      const result = await dispatch('repo_update', { repo_id: id, note: noteEdit, tags: tagsEdit })
      setDetail((current) => current?.editable ? { ...current, repo: result.repo } : current)
      if (result.status === 'conflict') {
        setConflict(result.repo)
        return
      }
      setDraft(null)
      onSaved()
    } catch (error) {
      setSaveError(errorMessage(error))
    } finally {
      setSaving(false)
    }
  }

  function reviewConflict() {
    if (!draft || !conflict) return
    setDraft({
      base: conflict,
      note: draft.note === draft.base.note ? conflict.note : draft.note,
      tags: sameTags(normalizedTags(draft.tags), draft.base.tags) ? conflict.tags.join('\n') : draft.tags,
    })
    setConflict(null)
  }

  async function fetchReadme() {
    setFetching(true)
    setReadmeError('')
    try {
      const result = await dispatch('repo_readme', { repo_id: id, session_id: sessionId ?? null })
      if (result.readme != null) setReadme(result.readme)
      setRetry((count) => count + 1)
      setReadmeChecked(true)
    } catch (error) {
      setReadmeError(errorMessage(error))
    } finally {
      setFetching(false)
    }
  }

  async function openGitHub(url: string) {
    setOpenError('')
    try {
      await dispatch('open_github', { url })
    } catch (error) {
      setOpenError(errorMessage(error))
    }
  }

  return (
    <div className="repo-detail">
      <button className="text-button back-button" type="button" onClick={onBack}><ArrowLeft size={16} /> 返回资料库</button>
      {loading && !repo && <Loading label="正在读取仓库…" />}
      {loadError && <Notice retry={() => setRetry((count) => count + 1)}>{loadError}</Notice>}
      {repo && (
        <>
          <header className="detail-heading">
            <RepoState repo={repo} />
            <h1>{repo.full_name}</h1>
            {repo.description && <p>{repo.description}</p>}
            {repo.language && <span className="language"><i />{repo.language}</span>}
            <div className="button-row">
              <button type="button" className="button primary" disabled={contextLocked || otherSession} onClick={() => onContext(repo)}>
                <Plus size={16} /> 加入对话上下文
              </button>
              <button type="button" className="button" onClick={() => void openGitHub(repo.url)}>
                在 GitHub 打开 <ArrowUpRight size={15} />
              </button>
            </div>
            {otherSession && <p className="muted">切回候选所在的会话后可加入上下文。</p>}
            {openError && <Notice>{openError}</Notice>}
          </header>
          {detail?.editable && (
            <section className="detail-section">
              <div className="section-heading">
                <h2>个人笔记和标签</h2>
                {!draft && (
                  <button type="button" className="button small" onClick={() => setDraft({ base: detail.repo, note: detail.repo.note, tags: detail.repo.tags.join('\n') })}>
                    <Pencil size={14} /> 编辑
                  </button>
                )}
              </div>
              {draft ? (
                <form className="edit-form" onSubmit={(event) => void save(event)}>
                  <label>收藏理由<textarea rows={4} value={draft.note} disabled={saving} onChange={(event) => setDraft({ ...draft, note: event.target.value })} /></label>
                  <label>标签（每行一个）<textarea rows={3} value={draft.tags} disabled={saving} onChange={(event) => setDraft({ ...draft, tags: event.target.value })} /></label>
                  {saveError && <Notice>{saveError}</Notice>}
                  {conflict && (
                    <Notice>
                      <p>其他窗口或命令行已修改资料。草稿已保留，请核对已保存的内容。</p>
                      <p className="repo-note">已保存的收藏理由：{conflict.note || '（空）'}</p>
                      <p>已保存的标签：{conflict.tags.join('、') || '（空）'}</p>
                      <button type="button" className="button small" onClick={reviewConflict}>保留草稿并继续编辑</button>
                    </Notice>
                  )}
                  <div className="button-row">
                    <button className="button primary" disabled={saving || loading || !!conflict}><Save size={15} /> {saving ? '正在保存…' : '保存整理'}</button>
                    <button type="button" className="button" disabled={saving} onClick={() => { setDraft(null); setConflict(null); setSaveError('') }}>取消</button>
                  </div>
                </form>
              ) : (
                <>
                  {detail.repo.note ? <p className="repo-note">{detail.repo.note}</p> : <p className="muted">尚未记录收藏理由。</p>}
                  {!!detail.repo.tags.length && <div className="tags">{detail.repo.tags.map((tag) => <span className="tag" key={tag}>{tag}</span>)}</div>}
                </>
              )}
            </section>
          )}
          <section className="detail-section">
            <div className="section-heading">
              <h2><BookOpen size={17} /> README</h2>
              <button type="button" className="button small" disabled={fetching || loading} onClick={() => void fetchReadme()}>
                {fetching ? '正在获取…' : readme !== null ? '刷新 README' : '从 GitHub 获取'}
              </button>
            </div>
            {readmeError && <Notice retry={fetching || loading ? undefined : () => void fetchReadme()}>{readmeError}</Notice>}
            {readme !== null ? <Markdown>{readme}</Markdown> : (
              <p className="muted">{readmeChecked ? 'GitHub 未返回可读取的 README。' : '本地暂无 README。'}</p>
            )}
          </section>
        </>
      )}
    </div>
  )
}
