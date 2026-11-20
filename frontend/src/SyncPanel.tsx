import { ArrowDownUp, Check, Github } from 'lucide-react'
import { useEffect, useRef, useState } from 'react'
import { dispatch, errorMessage, formatTime, type SyncStatus } from './api'
import { Notice } from './ui'

const activePollDelay = 650
const idlePollDelay = 5000
const initialState = {
  status: null as SyncStatus | null,
  startedAt: null as number | null,
  startError: '',
  statusError: '',
}

export function useLibrarySync(onSettled: () => void) {
  const [sync, setSync] = useState(initialState)
  const start = useRef<(() => Promise<void>) | null>(null)

  useEffect(() => {
    let current = initialState
    let mounted = true
    let starting = false
    let polling = false
    let generation = 0
    let timer: number

    function update(changes: Partial<typeof initialState>) {
      current = { ...current, ...changes }
      setSync(current)
    }

    function acceptStatus(status: SyncStatus, localCompletion = false) {
      const previous = current.status
      const newOutcome = status.last_success_at !== previous?.last_success_at ||
        (status.last_result?.state === 'error' &&
          (previous?.last_result?.state !== 'error' || previous.last_result.message !== status.last_result.message))
      update({
        status,
        startedAt: status.running ? current.startedAt ?? performance.now() : null,
        startError: (status.running && !previous?.running) || newOutcome ? '' : current.startError,
        statusError: '',
      })
      if (!status.running && (localCompletion || previous?.running)) onSettled()
    }

    function pollSoon() {
      window.clearTimeout(timer)
      if (!polling) timer = window.setTimeout(() => void poll(), 0)
    }

    async function poll() {
      polling = true
      const requestedAt = generation
      try {
        const status = await dispatch('sync_status', {})
        if (mounted && requestedAt === generation) {
          // An idle read can precede the local call acquiring its sync lease.
          if (!starting || status.running) acceptStatus(status)
        }
      } catch (error) {
        if (mounted && requestedAt === generation) update({ statusError: errorMessage(error) })
      } finally {
        polling = false
        if (mounted) {
          const delay = current.startedAt === null ? idlePollDelay : activePollDelay
          timer = window.setTimeout(() => void poll(), requestedAt === generation ? delay : 0)
        }
      }
    }

    start.current = async () => {
      if (starting || current.status?.running) return
      starting = true
      generation++
      update({ startedAt: performance.now(), startError: '', statusError: '' })
      try {
        const pending = dispatch('sync_start', {})
        pollSoon()
        const status = await pending
        if (mounted) acceptStatus(status, true)
      } catch (error) {
        if (mounted) {
          update({ startError: errorMessage(error), startedAt: current.status?.running ? current.startedAt : null })
        }
      } finally {
        starting = false
        generation++
        if (mounted) pollSoon()
      }
    }

    void poll()
    return () => {
      mounted = false
      start.current = null
      window.clearTimeout(timer)
    }
  }, [onSettled])

  return { ...sync, startSync: () => void start.current?.() }
}

const phaseLabels = {
  preparing: '正在准备同步',
  fetching: '正在获取星标',
  retrying: '连接中断，正在重试',
  saving: '正在保存资料库',
}

export function SyncPanel({ sync, onOpenSettings }: {
  sync: ReturnType<typeof useLibrarySync>
  onOpenSettings: () => void
}) {
  const { status, startedAt, startError, statusError, startSync } = sync
  const running = startedAt !== null
  const result = status?.last_result
  const failure = running ? '' : startError || (result?.state === 'error' ? result.message : '')
  const success = !running && !failure && result?.state === 'success' ? result : null
  const progress = status?.running ? status.progress : null
  const showCounts = running && progress && progress.phase !== 'preparing'
  const phase = progress?.phase === 'retrying'
    ? `${phaseLabels.retrying}第 ${progress.pages + 1} 页`
    : running && (progress ? phaseLabels[progress.phase] : status?.running ? '正在同步星标' : phaseLabels.preparing)

  return (
    <section className="sync" aria-labelledby="sync-heading">
      <h2 className="rail-label" id="sync-heading"><Github size={15} aria-hidden="true" /> GitHub 同步</h2>
      <div className="sync-status">
        <div className="sync-phase">
          {running && <progress className="sync-spinner" aria-labelledby="sync-phase" aria-describedby={showCounts ? 'sync-counts' : undefined} />}
          {success && <Check size={14} aria-hidden="true" />}
          <strong id="sync-phase" role="status">
            {phase || (failure ? '同步失败' : success ? `已同步 ${success.starred_count.toLocaleString('zh-CN')} 个星标` : '')}
          </strong>
        </div>
        {showCounts && (
          <div className="sync-counts" id="sync-counts">
            <span>已获取 {progress.pages.toLocaleString('zh-CN')} 页</span>
            <span>{progress.repositories.toLocaleString('zh-CN')} 个仓库</span>
          </div>
        )}
        {running ? <SyncElapsed startedAt={startedAt} /> : (
          <span>{status?.last_success_at ? `上次成功 ${formatTime(status.last_success_at)}` : status ? '尚未成功同步' : '等待读取同步状态'}</span>
        )}
      </div>
      {failure && (
        <div className="settings-callout">
          <Notice>{failure}</Notice>
          <button type="button" className="text-button" aria-haspopup="dialog" onClick={onOpenSettings}>打开 GitHub 设置</button>
        </div>
      )}
      {!running && !failure && statusError && <div className="sync-read-error"><Notice>无法读取同步状态：{statusError}</Notice></div>}
      <button type="button" className="button sync-button" aria-disabled={running} onClick={startSync}>
        <ArrowDownUp size={15} aria-hidden="true" /> {running ? '同步中…' : failure ? '重试同步' : '同步星标'}
      </button>
    </section>
  )
}

function SyncElapsed({ startedAt }: { startedAt: number }) {
  const [now, setNow] = useState(() => performance.now())
  useEffect(() => {
    const timer = window.setInterval(() => setNow(performance.now()), 1000)
    return () => window.clearInterval(timer)
  }, [])
  const seconds = Math.max(0, Math.floor((now - startedAt) / 1000))
  return <span className="sync-elapsed">已用时 {Math.floor(seconds / 60)}:{String(seconds % 60).padStart(2, '0')}</span>
}
