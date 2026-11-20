import { useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type PointerEvent, type ReactNode } from 'react'

type Side = 'left' | 'right'
type Widths = Record<Side, number>
type Preferences = Partial<Widths>
const storageKey = 'starchive.workspace-widths.v1'
const clamp = (value: number, min: number, max: number) => Math.min(Math.max(value, min), Math.max(min, max))

function readPreferences(): Preferences {
  try {
    const stored = JSON.parse(localStorage.getItem(storageKey) ?? '{}')
    const prefs: Preferences = {}
    for (const side of ['left', 'right'] as const) {
      if (Number.isFinite(stored?.[side]) && stored[side] > 0) prefs[side] = stored[side]
    }
    return prefs
  } catch { return {} }
}

function measure(width: number) {
  return { width, rem: parseFloat(getComputedStyle(document.documentElement).fontSize), compact: matchMedia('(max-width: 53.75rem)').matches }
}

function layout(prefs: Preferences, { width, rem, compact }: ReturnType<typeof measure>) {
  const leftMin = 9.5 * rem
  const rightMin = compact ? 0 : 18 * rem
  const budget = Math.max(0, width - 18 * rem - leftMin - rightMin)
  const left = prefs.left ?? (compact ? 10.75 * rem : clamp(width * .15, 10.75 * rem, 14 * rem))
  const right = compact ? 0 : prefs.right ?? clamp(width * .29, 20 * rem, 28 * rem)
  // Shrink only the space above each minimum when restoring a wider window's layout.
  const extraLeft = clamp(left - leftMin, 0, budget)
  const extraRight = clamp(right - rightMin, 0, budget)
  const scale = Math.min(1, budget / Math.max(1, extraLeft + extraRight))
  return {
    sizes: { left: leftMin + extraLeft * scale, right: rightMin + extraRight * scale },
    minimum: { left: leftMin, right: rightMin },
    available: leftMin + rightMin + budget,
  }
}

export function ResizableWorkspace({ children }: { children: ReactNode }) {
  const workspace = useRef<HTMLDivElement>(null)
  const [viewport, setViewport] = useState(() => measure(window.innerWidth))
  const [prefs, setPrefs] = useState(readPreferences)
  const [preview, setPreview] = useState<Preferences | null>(null)
  const drag = useRef<{ side: Side; id: number; x: number; sizes: Widths } | null>(null)
  const { sizes, minimum, available } = layout(preview ?? prefs, viewport)

  useLayoutEffect(() => {
    const element = workspace.current!
    let previousWidth = -1
    function update() {
      const width = element.clientWidth
      if (previousWidth === width) return
      previousWidth = width
      drag.current = null
      setPreview(null)
      setViewport(measure(width))
    }
    update()
    const observer = new ResizeObserver(update)
    observer.observe(element)
    return () => observer.disconnect()
  }, [])

  function save(next: Preferences) {
    setPrefs(next)
    try { localStorage.setItem(storageKey, JSON.stringify(next)) } catch { /* Keep the in-window layout. */ }
  }

  function adjusted(side: Side, from: Widths, requested: number): Preferences {
    const other = side === 'left' ? 'right' : 'left'
    const next = { ...prefs, [side]: clamp(requested, minimum[side], available - from[other]) }
    // Keep the opposite divider stationary even when a smaller window has compressed both panes.
    if (!viewport.compact && Math.abs(layout(next, viewport).sizes[other] - from[other]) > .01) next[other] = from[other]
    return next
  }

  function move(event: PointerEvent<HTMLDivElement>, finish: boolean) {
    const active = drag.current
    if (!active || event.pointerId !== active.id) return
    const delta = (event.clientX - active.x) * (active.side === 'left' ? 1 : -1)
    const next = adjusted(active.side, active.sizes, active.sizes[active.side] + delta)
    if (finish) {
      drag.current = null
      setPreview(null)
      if (delta !== 0) save(next)
    } else setPreview(next)
  }

  function keyDown(event: KeyboardEvent, side: Side) {
    const step = event.shiftKey ? 48 : 12
    const sign = side === 'left' ? 1 : -1
    let target: number
    switch (event.key) {
      case 'ArrowLeft': target = sizes[side] - step * sign; break
      case 'ArrowRight': target = sizes[side] + step * sign; break
      case 'Home': target = minimum[side]; break
      case 'End': target = available; break
      default: return
    }
    event.preventDefault()
    save(adjusted(side, sizes, target))
  }

  function separator(side: Side) {
    const label = side === 'left' ? '调整侧栏宽度' : '调整 Agent 会话宽度'
    return <div className={`pane-divider divider-${side}`} role="separator" tabIndex={0}
      aria-label={label} aria-orientation="vertical" aria-controls={side === 'left' ? 'library-sidebar' : 'agent'}
      aria-valuemin={Math.round(minimum[side])} aria-valuemax={Math.round(available - sizes[side === 'left' ? 'right' : 'left'])}
      aria-valuenow={Math.round(sizes[side])} aria-valuetext={`${Math.round(sizes[side])} 像素`}
      title={`${label}，拖动或使用左右方向键，双击恢复默认`}
      onPointerDown={(event) => {
        if (event.button !== 0 || !event.isPrimary || drag.current) return
        event.currentTarget.setPointerCapture(event.pointerId)
        drag.current = { side, id: event.pointerId, x: event.clientX, sizes }
        setPreview(prefs)
      }}
      onPointerMove={(event) => move(event, false)} onPointerUp={(event) => move(event, true)}
      onLostPointerCapture={() => { drag.current = null; setPreview(null) }}
      onDoubleClick={() => { const next = { ...prefs }; delete next[side]; save(next) }}
      onKeyDown={(event) => keyDown(event, side)} />
  }

  return <div className="workspace" ref={workspace} data-resizing={preview !== null || undefined}
    style={{ '--left-width': `${sizes.left}px`, '--right-width': `${sizes.right}px` } as CSSProperties}>
    {children}
    {separator('left')}
    {separator('right')}
  </div>
}
