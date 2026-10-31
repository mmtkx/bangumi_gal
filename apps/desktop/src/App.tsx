import { getCurrentWindow } from '@tauri-apps/api/window';
import { useCallback, useEffect, useState } from 'react';

import { bangumiAccount, importDrop, playNextEpisode, subscribeAppEvents, uiReady } from './api';
import type { AccountInfo } from './api';
import IssueBanner from './components/IssueBanner';
import AccountPage from './pages/AccountPage';
import ItemDetailPage from './pages/ItemDetailPage';
import LibraryPage from './pages/LibraryPage';
import SettingsPage from './pages/SettingsPage';
import SyncPage from './pages/SyncPage';

type View =
  | { name: 'library' }
  | { name: 'detail'; id: string }
  | { name: 'sync' }
  | { name: 'account' }
  | { name: 'settings' };

export default function App() {
  const [view, setView] = useState<View>({ name: 'library' });
  const [libraryVersion, setLibraryVersion] = useState(0);
  // 拖入导入：拖拽中显示遮罩，导入中显示进度，结束后弹出结果。
  const [dragging, setDragging] = useState(false);
  const [importing, setImporting] = useState(false);
  const [importNotice, setImportNotice] = useState<string | null>(null);
  const [importIssues, setImportIssues] = useState<string[]>([]);
  // 侧栏顶部的账号卡：未授权时退回应用品牌。
  const [account, setAccount] = useState<AccountInfo | null>(null);
  // 头像加载失败（外链被 CSP 拦或图片 404）时退回字母块，避免出现破图。
  const [avatarFailed, setAvatarFailed] = useState(false);

  const bumpLibrary = useCallback(() => setLibraryVersion((value) => value + 1), []);

  const reloadAccount = useCallback(() => {
    bangumiAccount()
      .then((info) => {
        setAccount(info);
        // 换了账号后允许重新尝试加载头像。
        setAvatarFailed(false);
      })
      .catch((error: unknown) => console.warn('读取 Bangumi 账号失败', error));
  }, []);

  useEffect(() => {
    reloadAccount();
  }, [reloadAccount]);

  const handleDrop = useCallback(
    async (paths: string[]) => {
      if (paths.length === 0) return;
      setImporting(true);
      setImportNotice(null);
      setImportIssues([]);
      try {
        const report = await importDrop(paths);
        const lines = report.imported.map(
          (item) => `${item.name} → ${item.kind === 'anime' ? '动画' : '游戏'}根目录`,
        );
        if (report.imported.length > 0) {
          setImportNotice(`已导入 ${report.imported.length} 项，正在刷新目录…`);
        }
        setImportIssues([...lines, ...report.skipped, ...report.failed]);
        if (
          report.imported.length === 0 &&
          report.skipped.length === 0 &&
          report.failed.length === 0
        ) {
          setImportNotice('没有识别到可导入的内容。');
        }
        if (report.imported.length > 0) bumpLibrary();
      } catch (err) {
        setImportNotice(null);
        setImportIssues([err instanceof Error ? err.message : String(err)]);
      } finally {
        setImporting(false);
      }
    },
    [bumpLibrary],
  );

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    let disposed = false;

    getCurrentWindow()
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === 'enter' || payload.type === 'over') {
          setDragging(true);
        } else if (payload.type === 'drop') {
          setDragging(false);
          void handleDrop(payload.paths);
        } else {
          setDragging(false);
        }
      })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((error: unknown) => console.warn('注册拖放监听失败', error));

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [handleDrop]);

  useEffect(() => {
    // 打点：CSP 收紧后若界面白屏，日志里会缺少这一行。
    uiReady().catch((error: unknown) => console.warn('界面就绪打点失败', error));
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    subscribeAppEvents((envelope) => {
      // 条目增删改与扫描完成都会影响列表。
      if (envelope.topic.startsWith('library:item:') || envelope.topic === 'scanner:job:finished') {
        bumpLibrary();
      }
      // 授权状态变化：侧栏头像与昵称要跟着换。
      if (envelope.topic === 'bangumi:auth:changed') {
        reloadAccount();
      }
      // 连播放在这里而不是详情页：换到别的页面也继续播下一集。
      if (envelope.topic === 'player:session:finished') {
        if (localStorage.getItem('lagm.autoNext') !== '1') return;
        const itemId = String(envelope.payload['itemId'] ?? '');
        const episode = Number(envelope.payload['episode']);
        if (itemId === '' || !Number.isFinite(episode)) return;
        playNextEpisode(itemId, episode)
          .then((session) => {
            if (!session) console.warn('已是最后一集，连播结束');
          })
          .catch((error: unknown) => console.warn('连播失败', error));
      }
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((error: unknown) => {
        console.warn('订阅主干道事件失败', error);
      });

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [bumpLibrary, reloadAccount]);

  return (
    <div className="app">
      <aside className="sidebar">
        <div
          className="brand brand-account"
          onClick={() => setView({ name: 'account' })}
          role="button"
          tabIndex={0}
          onKeyDown={(event) => {
            if (event.key === 'Enter' || event.key === ' ') setView({ name: 'account' });
          }}
          title={account?.authorized ? '打开账号页' : '打开账号页并登录 Bangumi'}
        >
          {account?.authorized && account.avatarUrl && !avatarFailed ? (
            <img
              className="brand-avatar"
              src={account.avatarUrl}
              alt={account.nickname ?? account.username ?? '头像'}
              onError={() => setAvatarFailed(true)}
            />
          ) : (
            <span className="brand-mark">L</span>
          )}
          <div className="brand-text">
            {account?.authorized ? (
              <>
                <strong>{account.nickname || account.username || `UID ${account.id ?? ''}`}</strong>
                <span className="brand-sub">
                  {account.username ? `@${account.username}` : `UID ${account.id ?? ''}`}
                </span>
              </>
            ) : (
              <>
                <strong>LAGM</strong>
                <span className="brand-sub">本地动画 / 游戏库</span>
              </>
            )}
          </div>
        </div>

        <nav className="nav">
          <button
            type="button"
            className={
              view.name === 'library' || view.name === 'detail' ? 'nav-item is-active' : 'nav-item'
            }
            onClick={() => setView({ name: 'library' })}
          >
            <span className="nav-icon">▤</span>
            条目库
          </button>
          <button
            type="button"
            className={view.name === 'sync' ? 'nav-item is-active' : 'nav-item'}
            onClick={() => setView({ name: 'sync' })}
          >
            <span className="nav-icon">⇄</span>
            同步
          </button>
          <button
            type="button"
            className={view.name === 'account' ? 'nav-item is-active' : 'nav-item'}
            onClick={() => setView({ name: 'account' })}
          >
            <span className="nav-icon">◎</span>
            账号
          </button>
          <button
            type="button"
            className={view.name === 'settings' ? 'nav-item is-active' : 'nav-item'}
            onClick={() => setView({ name: 'settings' })}
          >
            <span className="nav-icon">⚙</span>
            设置
          </button>
        </nav>

        <footer className="sidebar-foot">
          <span>本地资源管理</span>
          <span className="muted">数据仅存本地</span>
        </footer>
      </aside>

      <main className="main">
        {view.name === 'library' && (
          <LibraryPage
            version={libraryVersion}
            onOpen={(id) => setView({ name: 'detail', id })}
            onConfigure={() => setView({ name: 'settings' })}
          />
        )}
        {view.name === 'detail' && (
          <ItemDetailPage
            id={view.id}
            onBack={() => setView({ name: 'library' })}
            onChanged={bumpLibrary}
          />
        )}
        {view.name === 'sync' && <SyncPage />}
        {view.name === 'account' && <AccountPage />}
        {view.name === 'settings' && <SettingsPage onSaved={() => setView({ name: 'library' })} />}
      </main>

      {(dragging || importing) && (
        <div className="drop-overlay">
          <div className="drop-hint">
            {importing
              ? '正在识别并搬运…'
              : '松开即可导入：自动识别动画 / 游戏，并剪切到对应根目录'}
          </div>
        </div>
      )}

      {(importNotice || importIssues.length > 0) && (
        <div className="drop-report">
          {importNotice && (
            <div className="banner banner-ok">
              {importNotice}
              <button type="button" className="btn btn-tiny" onClick={() => setImportNotice(null)}>
                知道了
              </button>
            </div>
          )}
          <IssueBanner
            title="导入结果"
            items={importIssues}
            onDismiss={() => setImportIssues([])}
          />
        </div>
      )}
    </div>
  );
}
