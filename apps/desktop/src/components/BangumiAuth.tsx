import { useEffect, useState } from 'react';

import {
  bangumiAuthorize,
  bangumiCompleteManual,
  bangumiLogout,
  bangumiStatus,
  getSettings,
  saveBangumiConfig,
  type BangumiStatus,
  type Settings,
} from '../api';

interface Props {
  /** 授权状态变化后通知外层刷新账号信息。 */
  onChanged?: () => void;
}

/** 把 RFC3339 到期时间转成「剩余 N 天」：原始时间戳对用户没有意义。 */
function describeExpiry(expiresAt: string | null | undefined): string | null {
  if (!expiresAt) return null;
  const at = Date.parse(expiresAt);
  if (Number.isNaN(at)) return null;
  const days = Math.ceil((at - Date.now()) / 86_400_000);
  if (days <= 0) return '已过期';
  if (days === 1) return '剩余不足 1 天';
  return `剩余 ${days} 天`;
}

/**
 * Bangumi 授权区块（原在设置页，现归到账号页）。
 *
 * 发行版内置了 Bangumi 应用：凭据由软件固定，界面不显示 App ID / Secret / 回调地址，
 * 只保留「授权 / 重新授权 / 退出登录」和代理设置。
 * 开发版没有内置凭据时才显示三项输入框，供自行填写。
 * 凭据只落本机数据库，token 存 Windows 凭据管理器。
 */
export default function BangumiAuth({ onChanged }: Props) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [auth, setAuth] = useState<BangumiStatus | null>(null);
  const [clientId, setClientId] = useState('');
  const [clientSecret, setClientSecret] = useState('');
  const [redirectUri, setRedirectUri] = useState('');
  const [proxy, setProxy] = useState('');
  const [manualCallback, setManualCallback] = useState('');
  const [saving, setSaving] = useState(false);
  const [authorizing, setAuthorizing] = useState(false);
  const [completing, setCompleting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  /** 内置应用：凭据由软件固定，界面不出现任何可编辑入口。 */
  const builtin = auth?.usingBuiltinApp ?? settings?.bangumiBuiltin ?? false;
  const expiryText = describeExpiry(auth?.expiresAt);

  useEffect(() => {
    let cancelled = false;
    getSettings()
      .then((data) => {
        if (cancelled) return;
        setSettings(data);
        setClientId(data.bangumiClientId ?? '');
        setRedirectUri(data.bangumiRedirectUri ?? data.bangumiDefaultRedirectUri);
        setProxy(data.bangumiProxy ?? '');
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    bangumiStatus()
      .then((status) => {
        if (!cancelled) setAuth(status);
      })
      .catch((err: unknown) => {
        if (!cancelled) console.warn('读取 Bangumi 授权状态失败', err);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  async function handleSave() {
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      const status = await saveBangumiConfig({
        // 内置应用只提交代理，凭据一律留空（后端也会拒绝覆盖）。
        clientId: builtin ? '' : clientId.trim(),
        clientSecret: builtin ? '' : clientSecret.trim(),
        redirectUri: builtin ? '' : redirectUri.trim(),
        proxy: proxy.trim(),
      });
      setAuth(status);
      setClientSecret('');
      setNotice(
        builtin
          ? '代理设置已保存。'
          : status.hasCredentials
            ? 'Bangumi 凭据已保存。'
            : 'Bangumi 凭据已清除。',
      );
      onChanged?.();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleAuthorize() {
    setAuthorizing(true);
    setError(null);
    setNotice('已打开浏览器，请在页面中完成授权（最长等待 3 分钟）…');
    try {
      const status = await bangumiAuthorize();
      setAuth(status);
      setNotice(status.authorized ? 'Bangumi 授权成功。' : '授权未完成，请重试。');
      onChanged?.();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setNotice(null);
    } finally {
      setAuthorizing(false);
    }
  }

  async function handleManualComplete() {
    setCompleting(true);
    setError(null);
    setNotice(null);
    try {
      const status = await bangumiCompleteManual(manualCallback.trim());
      setAuth(status);
      setManualCallback('');
      setNotice(status.authorized ? 'Bangumi 授权成功。' : '授权未完成，请重试。');
      onChanged?.();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCompleting(false);
    }
  }

  async function handleLogout() {
    setError(null);
    setNotice(null);
    try {
      setAuth(await bangumiLogout());
      setNotice('已退出 Bangumi 授权。');
      onChanged?.();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  return (
    <>
      {error && <div className="banner banner-error">{error}</div>}
      {notice && <div className="banner banner-ok">{notice}</div>}

      {builtin ? (
        <p className="muted">
          已内置 Bangumi 应用：App ID、App Secret 与回调地址由软件固定，界面上不显示也无法修改。
          直接点「授权」即可；如需更换账号，先「退出登录」再重新授权。
        </p>
      ) : (
        <p className="muted">
          在 Bangumi 开发者中心创建应用后，把 App ID、App Secret 与「回调地址」填到这里。
          回调地址必须与应用后台填写值完全一致，默认使用{' '}
          <span className="mono">{settings?.bangumiDefaultRedirectUri}</span>。
        </p>
      )}

      {!builtin && (
        <>
          <label className="field">
            <span className="field-label">App ID（client_id）</span>
            <input
              className="input"
              placeholder="例如 bgmxxxxxxxx"
              value={clientId}
              onChange={(event) => setClientId(event.target.value)}
            />
          </label>

          <label className="field">
            <span className="field-label">App Secret（client_secret）</span>
            <input
              className="input"
              type="password"
              placeholder={settings?.bangumiHasSecret ? '已保存，留空则不修改' : '填入 App Secret'}
              value={clientSecret}
              onChange={(event) => setClientSecret(event.target.value)}
            />
          </label>

          <label className="field">
            <span className="field-label">回调地址</span>
            <input
              className="input"
              placeholder={settings?.bangumiDefaultRedirectUri}
              value={redirectUri}
              onChange={(event) => setRedirectUri(event.target.value)}
            />
          </label>
        </>
      )}

      <label className="field">
        <span className="field-label">
          代理（可选，留空自动探测系统代理；国内访问 bgm.tv 通常需要）
        </span>
        <input
          className="input"
          placeholder="例如 http://127.0.0.1:7890"
          value={proxy}
          onChange={(event) => setProxy(event.target.value)}
        />
      </label>

      <div className="row-actions">
        <button type="button" className="btn" onClick={() => void handleSave()} disabled={saving}>
          {saving ? '保存中…' : builtin ? '保存代理' : '保存凭据'}
        </button>
        <button
          type="button"
          className="btn btn-primary"
          onClick={() => void handleAuthorize()}
          disabled={authorizing || !auth?.hasCredentials}
          title={auth?.hasCredentials ? '打开浏览器授权' : '请先保存凭据'}
        >
          {authorizing ? '等待授权…' : auth?.authorized ? '重新授权' : '授权'}
        </button>
        <button
          type="button"
          className="btn"
          onClick={() => void handleLogout()}
          disabled={!auth?.authorized}
        >
          退出登录
        </button>
      </div>

      <label className="field">
        <span className="field-label">
          授权没自动完成？把浏览器地址栏里的回调地址（含 code）粘贴到这里，授权码 60 秒内有效
        </span>
        <div className="inline-form">
          <input
            className="input"
            placeholder="http://127.0.0.1:27432/callback?code=..."
            value={manualCallback}
            onChange={(event) => setManualCallback(event.target.value)}
          />
          <button
            type="button"
            className="btn"
            onClick={() => void handleManualComplete()}
            disabled={completing || manualCallback.trim() === ''}
          >
            {completing ? '处理中…' : '手动完成'}
          </button>
        </div>
      </label>

      <p className="muted">
        状态：{auth?.authorized ? '已授权' : '未授权'}
        {auth?.authorized && expiryText ? ` · token ${expiryText}（启动时自动续期）` : ''}
        {builtin
          ? ' · 使用内置应用（不可修改）'
          : auth?.clientId
            ? ` · 当前 App ID ${auth.clientId}`
            : ''}
      </p>
      {auth && !auth.authorized && auth.hasCredentials && (
        <p className="muted">
          尚未登录，或授权已失效（超过 7 天未打开应用会失效）。点「授权」重新登录即可。
        </p>
      )}
      <p className="muted">
        {builtin
          ? 'token 存于 Windows 凭据管理器；退出登录会删除本机 token，不影响 Bangumi 账号数据。'
          : 'App Secret 仅保存在本机数据库、不写入日志；token 存于 Windows 凭据管理器。'}
      </p>
    </>
  );
}
