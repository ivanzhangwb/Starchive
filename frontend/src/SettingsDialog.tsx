import { Github, LockKeyhole, RefreshCw, Terminal } from 'lucide-react'
import { useCallback, useEffect, useState } from 'react'
import { dispatch, type SettingsStatus } from './api'
import { Dialog, Notice } from './ui'

function sourceLabel(source: string, savedLabel: string) {
  switch (source) {
    case 'saved': return savedLabel
    case 'environment': return '环境变量'
    case 'gh': return 'GitHub CLI 登录'
    case 'auto': return '自动检测'
    case 'missing': return '无'
    default: return '未知'
  }
}

export function SettingsDialog({ onClose, onDetected }: {
  onClose: () => void
  onDetected: () => void
}) {
  const [status, setStatus] = useState<SettingsStatus | null>(null)
  const [pending, setPending] = useState<'detect' | 'save' | null>('detect')
  const [token, setToken] = useState('')
  const [path, setPath] = useState('')
  const [error, setError] = useState('')
  const [feedback, setFeedback] = useState('')
  const savedToken = status?.github.source === 'saved'
  const detectedPath = status?.pi.path
  const busy = pending !== null

  const update = useCallback(async (
    request: () => Promise<SettingsStatus>,
    failure: string,
    success?: string,
  ) => {
    setPending(success ? 'save' : 'detect')
    setError('')
    setFeedback('')
    try {
      const next = await request()
      setStatus(next)
      onDetected()
      setFeedback(success ?? '检测已完成。')
      return true
    } catch {
      setError(failure)
      return false
    } finally {
      setPending(null)
    }
  }, [onDetected])

  const detect = useCallback(() => update(
    () => dispatch('settings_get', {}),
    '无法检测本机设置，请重新检测。',
  ), [update])

  useEffect(() => { void detect() }, [detect])

  function saveToken(value: string | null) {
    setToken('')
    void update(
      () => dispatch('settings_save_github', { token: value }),
      '无法更新 GitHub 令牌。请检查系统钥匙串是否可用，然后重试。',
      value === null ? '已删除保存的 GitHub 令牌。' : 'GitHub 令牌已保存到系统钥匙串。',
    )
  }

  async function savePath(value: string | null) {
    if (await update(
      () => dispatch('settings_save_pi', { path: value }),
      value === null ? '无法重置 pi 路径，请重试。' : '无法保存 pi 路径。请检查路径是否指向可执行文件，然后重试。',
      value === null ? '已重置 pi 路径。' : '已保存 pi 路径。',
    )) setPath('')
  }

  return (
    <Dialog title="设置" className="settings-dialog" busy={pending === 'save'} showClose onClose={onClose}>
      <p className="settings-intro">配置 GitHub 星标同步和本地 Agent。可随时离线打开资料库，无需令牌。</p>
      <section className="settings-section" aria-labelledby="settings-github-title">
        <div className="section-heading">
          <h3 id="settings-github-title"><Github size={17} aria-hidden="true" /> GitHub 同步</h3>
          {status && <span className={status.github.available ? 'tag tag-accent' : 'tag'}>{status.github.available ? '已检测到凭证' : '未检测到凭证'}</span>}
        </div>
        {status && <p>凭证来源 <strong>{sourceLabel(status.github.source, '系统钥匙串')}</strong></p>}
        {!status?.github.available && (
          <p>星笺会检测 GitHub CLI（<code>gh</code>）已有的登录。也可以在终端运行 <code>gh auth login</code>，再回到这里重新检测，或在下方粘贴个人访问令牌（PAT）。</p>
        )}
        <form className="dialog-form settings-form" onSubmit={(event) => {
          event.preventDefault()
          if (!busy && status && token.trim()) saveToken(token.trim())
        }}>
          <label htmlFor="settings-token">{savedToken ? '新的 GitHub 令牌' : 'GitHub 个人访问令牌'}</label>
          <input
            id="settings-token"
            type="password"
            autoComplete="new-password"
            autoCapitalize="off"
            spellCheck={false}
            aria-describedby="settings-token-help"
            placeholder={savedToken ? '粘贴新令牌以替换' : '粘贴令牌'}
            value={token}
            disabled={busy || !status}
            onChange={(event) => setToken(event.target.value)}
          />
          <p id="settings-token-help" className="settings-hint">使用细粒度令牌，给「Starring」只读权限。读取私有仓库的 README 时，再为相应仓库授予「Contents」只读权限。</p>
          <div className="button-row">
            <button className="button" disabled={busy || !status || !token.trim()}><LockKeyhole size={15} aria-hidden="true" /> {savedToken ? '替换令牌' : '保存令牌'}</button>
            {savedToken && <button type="button" className="text-button settings-delete" disabled={busy} onClick={() => saveToken(null)}>删除保存的令牌</button>}
          </div>
        </form>
      </section>
      <section className="settings-section" aria-labelledby="settings-pi-title">
        <div className="section-heading">
          <h3 id="settings-pi-title"><Terminal size={17} aria-hidden="true" /> 本地 Agent</h3>
          {status && <span className={status.pi.available ? 'tag tag-accent' : 'tag'}>{status.pi.available ? 'pi 可用' : '未找到 pi'}</span>}
        </div>
        {status && <p>路径来源 <strong>{sourceLabel(status.pi.source, '手动设置')}</strong></p>}
        {detectedPath && <p className="settings-path"><code dir="auto">{detectedPath}</code></p>}
        <form className="dialog-form settings-form" onSubmit={(event) => {
          event.preventDefault()
          if (!busy && status && path.trim()) void savePath(path.trim())
        }}>
          <label htmlFor="settings-path">pi 可执行文件路径</label>
          <input
            id="settings-path"
            type="text"
            autoComplete="off"
            autoCapitalize="off"
            spellCheck={false}
            aria-describedby="settings-pi-help"
            placeholder="例如 /opt/homebrew/bin/pi"
            value={path}
            disabled={busy || !status}
            onChange={(event) => setPath(event.target.value)}
          />
          <p id="settings-pi-help" className="settings-hint">可填入检测到的路径，或输入已安装的 pi 可执行文件的绝对路径。重置后恢复本机检测。</p>
          <div className="button-row">
            <button className="button" disabled={busy || !status || !path.trim()}>保存路径</button>
            {detectedPath && <button type="button" className="text-button" disabled={busy || path === detectedPath} onClick={() => setPath(detectedPath)}>填入检测路径</button>}
            <button type="button" className="text-button" disabled={busy || !status} onClick={() => void savePath(null)}>重置路径</button>
          </div>
        </form>
      </section>
      <footer className="settings-footer">
        {error && <Notice>{error}</Notice>}
        <p className="settings-feedback" role="status">{pending === 'detect' ? '正在检测本机设置…' : pending === 'save' ? '正在更新设置…' : feedback}</p>
        <button type="button" className="button" disabled={busy} onClick={() => void detect()}><RefreshCw size={15} aria-hidden="true" /> 重新检测</button>
      </footer>
    </Dialog>
  )
}
