import { useCallback, useEffect, useState } from 'react';

import { bangumiAccount, bangumiStatus, type AccountInfo, type BangumiStatus } from '../api';
import BangumiAuth from '../components/BangumiAuth';

interface Props {
  /** 授权或账号变化后通知外层刷新（例如同步页的账号状态）。 */
  onChanged?: () => void;
}

export default function AccountPage({ onChanged }: Props) {
  const [status, setStatus] = useState<BangumiStatus | null>(null);
  const [account, setAccount] = useState<AccountInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      // 授权状态来自本地 token，一定拿得到；账号资料要联网，失败不能影响授权展示。
      const auth = await bangumiStatus();
      setStatus(auth);
      if (!auth.authorized) {
        setAccount(null);
        return;
      }
      try {
        setAccount(await bangumiAccount());
      } catch (err) {
        setAccount(null);
        setError(err instanceof Error ? err.message : String(err));
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  function handleAuthChanged() {
    void load();
    onChanged?.();
  }

  const displayName = account?.nickname?.trim() || account?.username || null;
  const initials = displayName ? displayName.slice(0, 1).toUpperCase() : '?';
  const authorized = status?.authorized ?? false;

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1>账号</h1>
          <p className="muted">Bangumi 授权与当前登录账号</p>
        </div>
        <div className="page-actions">
          <button type="button" className="btn" onClick={() => void load()} disabled={loading}>
            {loading ? '读取中…' : '刷新'}
          </button>
        </div>
      </header>

      {error && <div className="banner banner-error">{error}</div>}

      <section className="card">
        <h2>当前账号</h2>
        {account ? (
          <div className="account-card">
            {account.avatarUrl ? (
              <img
                className="account-avatar"
                src={account.avatarUrl}
                alt={displayName ?? 'Bangumi 头像'}
                referrerPolicy="no-referrer"
              />
            ) : (
              <div className="account-avatar account-avatar-fallback">{initials}</div>
            )}
            <div className="account-meta">
              <div className="account-name">
                {displayName ?? '未命名用户'}
                {account.nickname && account.username && (
                  <span className="muted"> @{account.username}</span>
                )}
              </div>
              <div className="muted">
                UID {account.id ?? '—'}
                {account.expiresAt ? ` · token 到期 ${account.expiresAt}` : ''}
              </div>
            </div>
          </div>
        ) : loading ? (
          <p className="muted">正在读取账号资料…</p>
        ) : authorized ? (
          <p className="muted">
            已授权，但账号资料拉取失败（多为网络或代理问题）。授权本身可用，收藏与进度同步不受影响；
            网络恢复后点右上角「刷新」即可显示昵称与头像。
          </p>
        ) : (
          <p className="muted">
            尚未登录。完成下方授权后，这里会显示昵称、头像与 UID；同步功能也依赖该授权。
          </p>
        )}
      </section>

      <section className="card">
        <h2>Bangumi 授权</h2>
        <BangumiAuth onChanged={handleAuthChanged} />
      </section>
    </div>
  );
}
