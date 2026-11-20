import { AlertCircle, LockKeyhole, X } from 'lucide-react'
import { useLayoutEffect, useRef, type ReactNode } from 'react'
import ReactMarkdown from 'react-markdown'
import remarkGfm from 'remark-gfm'
import type { Repo } from './api'

export function Notice({ children, retry }: { children: ReactNode; retry?: () => void }) {
  return (
    <div className="notice" role="alert">
      <AlertCircle size={16} />
      <span>{children}</span>
      {retry && <button type="button" className="text-button" onClick={retry}>重试</button>}
    </div>
  )
}

export function Loading({ label }: { label: string }) {
  return (
    <div className="loading" role="status">
      <span>{label}</span>
      <div aria-hidden="true" />
      <div aria-hidden="true" />
      <div aria-hidden="true" />
    </div>
  )
}

export function RepoState({ repo }: { repo: Repo }) {
  return (
    <span className="repo-state">
      <span className={repo.starred ? 'tag tag-accent' : 'tag'}>{repo.starred ? '已星标' : '尚未星标'}</span>
      {repo.private && <span className="tag"><LockKeyhole size={12} /> 私有</span>}
    </span>
  )
}

export function Markdown({ children }: { children: string }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        components={{ a: ({ children }) => <span>{children}</span>, img: ({ alt }) => alt || null }}
      >
        {children}
      </ReactMarkdown>
    </div>
  )
}

export function Dialog({ title, children, onClose, busy, className, showClose }: {
  title: string
  children: ReactNode
  onClose: () => void
  busy: boolean
  className?: string
  showClose?: boolean
}) {
  const ref = useRef<HTMLDialogElement>(null)

  useLayoutEffect(() => {
    const dialog = ref.current
    dialog?.showModal()
    return () => dialog?.close()
  }, [])

  return (
    <dialog
      ref={ref}
      className={className}
      aria-label={title}
      onCancel={(event) => {
        event.preventDefault()
        if (!busy) onClose()
      }}
    >
      <div className="section-heading">
        <h2>{title}</h2>
        {showClose && <button type="button" className="icon-button" aria-label={`关闭${title}`} disabled={busy} onClick={onClose}><X size={19} aria-hidden="true" /></button>}
      </div>
      {children}
    </dialog>
  )
}
