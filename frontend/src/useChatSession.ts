import { useEffect, useRef, useState } from 'react'
import { flushSync } from 'react-dom'
import { dispatch, errorMessage, streamAgentTurn, type Session, type SessionSummary } from './api'

const activeSessionKey = 'starchive.active-session'
const newSessionPrefix = 'new:'

type Mutation = 'agent' | 'context' | 'search'
type Operation = Mutation | 'load' | 'create' | 'turn'

type Turn =
  | { status: 'pending'; text: string; reply: string }
  | { status: 'failed'; text: string; error: string; unconfirmed?: number }

type Failure = { operation: Exclude<Operation, 'turn'>; message: string; repoId?: string }

type Thread = {
  session: Session | null
  draft: string
  busy: Operation | null
  turn: Turn | null
  failure: Failure | null
  refreshedAt: string | null
}

type ChatState = {
  key: string
  threads: Map<string, Thread>
  sessions: SessionSummary[]
  restoring: boolean
  restoreError: string
}

const emptyThread: Thread = { session: null, draft: '', busy: null, turn: null, failure: null, refreshedAt: null }
const initialState: ChatState = {
  key: `${newSessionPrefix}0`, threads: new Map(), sessions: [], restoring: true, restoreError: '',
}

function rememberSession(id?: string) {
  try {
    if (id) localStorage.setItem(activeSessionKey, id)
    return localStorage.getItem(activeSessionKey)
  } catch {
    return null
  }
}

function containsNewTurn(session: Session, start: number, text: string) {
  return session.messages.slice(start).some((message, index) => (
    message.role === 'user' && message.text === text && session.messages[start + index + 1]?.role === 'assistant'
  ))
}

export function useChatSession(preferredAgent: string, syncTimestamp: string | null) {
  const [state, setState] = useState(initialState)
  const [restoreAttempt, setRestoreAttempt] = useState(0)
  const latest = useRef(state)
  const nextLocalId = useRef(0)
  const { key } = state
  const thread = state.threads.get(key) ?? emptyThread
  const { session, busy } = thread

  function publish(next: ChatState) {
    // Event handlers and IPC callbacks must see changes before React commits a render.
    latest.current = next
    setState(next)
  }

  function read(key: string) {
    return latest.current.threads.get(key) ?? emptyThread
  }

  function patch(key: string, fields: Partial<Thread>) {
    const current = latest.current
    const next = { ...read(key), ...fields }
    const threads = new Map(current.threads).set(key, next)
    let sessions = current.sessions
    if (fields.session) {
      const { id, title, updated_at } = fields.session
      sessions = [{ id, title, updated_at }, ...sessions.filter((summary) => summary.id !== id)]
        .sort((a, b) => b.updated_at.localeCompare(a.updated_at))
    }
    publish({ ...current, threads, sessions })
  }

  function selectedId(key: string) {
    return read(key).session?.id ?? (key.startsWith(newSessionPrefix) ? null : key)
  }

  async function select(value: string) {
    const current = latest.current
    if (current.restoring) return
    const key = [...current.threads].find(([, item]) => item.session?.id === value)?.[0] ?? value
    publish({ ...current, key })
    const id = selectedId(key)
    if (!id || read(key).busy) return
    patch(key, { busy: 'load', failure: null })
    try {
      patch(key, { session: await dispatch('session_get', { session_id: id }), busy: null, refreshedAt: syncTimestamp })
    } catch (error) {
      patch(key, { busy: null, failure: { operation: 'load', message: errorMessage(error) } })
    }
  }

  useEffect(() => {
    let active = true
    publish({ ...latest.current, restoring: true, restoreError: '' })
    void dispatch('session_list', {}).then((saved) => {
      if (!active) return
      const remembered = rememberSession()
      const target = saved.find((item) => item.id === remembered) ?? saved[0]
      const current = latest.current
      const known = new Set(current.sessions.map((summary) => summary.id))
      const sessions = [...current.sessions, ...saved.filter((summary) => !known.has(summary.id))]
      publish({ ...current, sessions, restoring: false })
      if (target && read(current.key) === emptyThread) void select(target.id)
    }).catch((error: unknown) => {
      if (active) publish({ ...latest.current, restoring: false, restoreError: errorMessage(error) })
    })
    return () => { active = false }
  }, [restoreAttempt])

  useEffect(() => {
    if (session) rememberSession(session.id)
  }, [session?.id])

  useEffect(() => {
    if (!session || !syncTimestamp || busy || thread.refreshedAt === syncTimestamp) return
    let active = true
    void dispatch('session_get', { session_id: session.id }).then((updated) => {
      const current = read(key)
      if (active && latest.current.key === key && !current.busy && current.session === session) {
        patch(key, { session: updated, refreshedAt: syncTimestamp })
      }
    }).catch(() => {
      // Keep the open conversation readable if a library refresh fails.
    })
    return () => { active = false }
  }, [key, session?.id, busy, syncTimestamp])

  async function ensureSession(key: string, agentId: string) {
    const existing = read(key).session
    if (existing) return existing
    const created = await dispatch('session_create', { agent_id: agentId || null })
    patch(key, { session: created })
    return created
  }

  async function create(agentId: string) {
    const current = latest.current
    if (current.restoring || current.key !== key || read(key).busy === 'create') return
    const newKey = `${newSessionPrefix}${++nextLocalId.current}`
    publish({ ...current, key: newKey })
    patch(newKey, { busy: 'create' })
    try {
      await ensureSession(newKey, agentId)
      patch(newKey, { busy: null })
    } catch (error) {
      patch(newKey, { busy: null, failure: { operation: 'create', message: errorMessage(error) } })
    }
  }

  function canStart() {
    const current = read(key)
    return latest.current.key === key && !latest.current.restoring && !current.busy
      && (current.session !== null || !selectedId(key))
  }

  async function mutate(operation: Mutation, execute: (session: Session) => Promise<Session>, repoId?: string) {
    if (!canStart()) return false
    patch(key, { busy: operation, failure: null })
    try {
      const active = await ensureSession(key, preferredAgent)
      patch(key, { session: await execute(active), busy: null })
      return latest.current.key === key
    } catch (error) {
      const id = read(key).session?.id
      let recovered: Session | undefined
      if (id) recovered = await dispatch('session_get', { session_id: id }).catch(() => undefined)
      patch(key, { session: recovered ?? read(key).session, busy: null, failure: { operation, message: errorMessage(error), repoId } })
      return false
    }
  }

  async function send(retrying: boolean, followLatest: () => void) {
    const current = read(key)
    const failed = current.turn?.status === 'failed' ? current.turn : null
    const text = retrying ? failed?.text : !failed ? current.draft.trim() : ''
    if (!canStart() || !text) return

    flushSync(() => {
      followLatest()
      patch(key, { draft: retrying ? current.draft : '', turn: { status: 'pending', text, reply: '' }, busy: 'turn', failure: null })
    })

    let unconfirmed = failed?.unconfirmed
    try {
      let active = await ensureSession(key, preferredAgent)
      if (unconfirmed !== undefined) {
        active = await dispatch('session_get', { session_id: active.id })
        if (containsNewTurn(active, unconfirmed, text)) {
          patch(key, { session: active, turn: null, busy: null })
          return
        }
        patch(key, { session: active })
      }
      const start = active.messages.length
      try {
        const saved = await streamAgentTurn(active.id, text, (delta) => {
          const turn = read(key).turn
          if (turn?.status === 'pending' && delta) patch(key, { turn: { ...turn, reply: turn.reply + delta } })
        })
        patch(key, { session: saved, turn: null, busy: null })
      } catch (error) {
        // A failed IPC response can follow a successful save. Check before offering retry.
        const recovered = await dispatch('session_get', { session_id: active.id }).catch(() => undefined)
        unconfirmed = recovered ? undefined : start
        if (recovered && containsNewTurn(recovered, start, text)) {
          patch(key, { session: recovered, turn: null, busy: null })
          return
        }
        if (recovered) patch(key, { session: recovered })
        throw error
      }
    } catch (error) {
      patch(key, { turn: { status: 'failed', text, error: errorMessage(error), unconfirmed }, busy: null })
    }
  }

  const options: { id: string; title: string; updated_at: string | null }[] = [...state.sessions]
  for (const [threadKey, item] of state.threads) {
    if (!item.session && threadKey.startsWith(newSessionPrefix) && (item.draft || item.busy || item.turn || item.failure)) {
      options.push({ id: threadKey, title: '新会话（未保存）', updated_at: null })
    }
  }

  return {
    ...thread, key, options, restoring: state.restoring, restoreError: state.restoreError,
    loadingSession: !session && busy === 'load', missingSession: !session && !key.startsWith(newSessionPrefix),
    retryRestore: () => setRestoreAttempt((value) => value + 1),
    select, create, mutate, send,
    setDraft: (draft: string) => { if (latest.current.key === key) patch(key, { draft }) },
    clearFailure: (operation: Failure['operation'], repoId?: string) => {
      const failure = read(key).failure
      if (failure?.operation === operation && failure.repoId === repoId) patch(key, { failure: null })
    },
    discardTurn: () => {
      if (canStart() && read(key).turn?.status === 'failed') patch(key, { turn: null })
    },
  }
}
