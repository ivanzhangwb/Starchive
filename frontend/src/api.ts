import { invoke } from '@tauri-apps/api/core'

export interface Repo {
  id: string
  full_name: string
  description: string | null
  language: string | null
  url: string
  starred: boolean
  private: boolean
}

export interface LibraryRepo extends Repo {
  note: string
  tags: string[]
}

export interface RepoDetail {
  repo: LibraryRepo
  readme: string | null
}

export type StarStatus = 'starred' | 'unstarred' | 'all'

export interface Filters {
  query: string
  language: string | null
  tag: string | null
  status: StarStatus
}

export interface RepoPage {
  repos: LibraryRepo[]
  total: number
  languages: string[]
  tags: string[]
}

export interface SyncStatus {
  running: boolean
  last_success_at: string | null
  last_result: { state: 'success'; starred_count: number } | { state: 'error'; message: string } | null
}

export interface Agent {
  id: string
  name: string
  available: boolean
}

export interface SessionSummary {
  id: string
  title: string
  updated_at: string
}

export interface ContextRepo {
  repo: Repo
  include_private: boolean
  include_notes: boolean
}

export interface Candidate {
  repo: Repo
  fit: string
  evidence: { source: string; excerpt: string }[]
  gaps: string[]
}

export interface SessionMessage {
  id: string
  role: 'user' | 'assistant'
  text: string
  gaps: string[]
  candidates: Candidate[]
}

export interface Session extends SessionSummary {
  agent_id: string | null
  context: ContextRepo[]
  messages: SessionMessage[]
  can_expand: boolean
}

type SessionId = { session_id: string }
type RepoId = { repo_id: string }
type Empty = Record<string, never>

type Dispatch = {
  repo_list: [Filters & { offset: number; limit: number }, RepoPage]
  repo_get: [RepoId, RepoDetail | null]
  repo_update: [RepoId & Pick<LibraryRepo, 'note' | 'tags'>, LibraryRepo]
  repo_readme: [RepoId & { session_id: string | null }, { readme: string | null }]
  sync_status: [Empty, SyncStatus]
  sync_start: [Empty, SyncStatus]
  agent_list: [Empty, Agent[]]
  session_list: [Empty, SessionSummary[]]
  session_create: [{ agent_id: string | null }, Session]
  session_get: [SessionId, Session]
  session_set_agent: [SessionId & { agent_id: string }, Session]
  session_set_context: [SessionId & RepoId & Pick<ContextRepo, 'include_private' | 'include_notes'>, Session]
  session_remove_context: [SessionId & RepoId, Session]
  agent_send: [SessionId & { text: string }, Session]
  agent_expand: [SessionId & { public_query: string }, Session]
  open_github: [{ url: string }, null]
}

export function dispatch<Action extends keyof Dispatch>(action: Action, payload: Dispatch[Action][0]) {
  return invoke<Dispatch[Action][1]>('dispatch', { action, payload })
}

export function errorMessage(error: unknown) {
  if (typeof error === 'string' && error.trim()) return error
  if (error && typeof error === 'object' && 'message' in error && typeof error.message === 'string') {
    return error.message
  }
  return '操作失败，请重试。'
}

export function formatTime(value: string) {
  return new Intl.DateTimeFormat('zh-CN', { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(value))
}
