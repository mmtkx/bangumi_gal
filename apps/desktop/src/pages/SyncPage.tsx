import { useCallback, useEffect, useState } from 'react';

import {
  ApiError,
  formatDuration,
  listConflicts,
  discardSyncEntry,
  listSyncQueue,
  pushPending,
  resolveConflicts,
  subscribeAppEvents,
  syncPull,
  syncStats,
  type ConflictItem,
  type SyncQueueItem,
  type SyncStats,
} from '../api';

const COLLECTION_LABELS: Record<number, string> = {
  1: '想看',
  2: '看过',
  3: '在看',
  4: '搁置',
  5: '抛弃',
};

const OP_LABELS: Record<string, string> = {
  collection: '收藏 / 评分',
  progress: '单集进度',
};

const STATUS_LABELS: Record<string, string> = {
  pending: '待发送',
  sending: '发送中',
  failed: '失败',
  done: '已完成',
};

function fieldLabel(field: string): string {
  if (field === 'collection_status') return '收藏状态';
  if (field === 'rating') return '评分';
  if (field.startsWith('progress:')) {
    return `进度 · 第 ${field.slice('progress:'.length)} 集`;
  }
  return field;
}

function displayValue(field: string, raw: string | null): string {
  if (raw === null || raw === '') return '—';
  if (field === 'collection_status') {
    const value = Number(raw);
    return COLLECTION_LABELS[value] ?? raw;
  }
  if (field === 'rating') return `${raw} 分`;
  if (field.startsWith('progress:')) {
    // 进度冲突值是「看过 / 未看」；兼容历史数据里的秒数。
    if (raw === '看过' || raw === '未看') return raw;
    const seconds = Number(raw);
    return Number.isFinite(seconds) ? formatDuration(seconds) : raw;
  }
  return raw;
}

function friendlyError(err: unknown): string {
  if (err instanceof ApiError) {
    if (err.code === 'sync_not_authorized') {
      return '尚未授权 Bangumi，请先到「设置 → Bangumi 授权」填写凭据并完成授权。';
    }
    return err.message;
  }
  return err instanceof Error ? err.message : String(err);
}

export default function SyncPage() {
  const [stats, setStats] = useState<SyncStats | null>(null);
  const [queue, setQueue] = useState<SyncQueueItem[]>([]);
  const [discarding, setDiscarding] = useState<string | null>(null);
  const [conflicts, setConflicts] = useState<ConflictItem[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<null | 'pull' | 'push' | 'resolve'>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [nextStats, nextQueue, nextConflicts] = await Promise.all([
        syncStats(),
        listSyncQueue(),
        listConflicts(),
      ]);
      setStats(nextStats);
      setQueue(nextQueue);
      setConflicts(nextConflicts);
      setSelected((prev) => {
        const valid = new Set(nextConflicts.map((item) => item.id));
        return new Set([...prev].filter((id) => valid.has(id)));
      });
      setError(null);
    } catch (err) {
      setError(friendlyError(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    subscribeAppEvents((envelope) => {
      if (envelope.topic.startsWith('sync:') || envelope.topic.startsWith('library:item:')) {
        void refresh();
      }
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((err: unknown) => console.warn('订阅同步事件失败', err));

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [refresh]);

  async function handlePull() {
    setBusy('pull');
    setError(null);
    setNotice(null);
    try {
      const report = await syncPull();
      setNotice(
        `拉取完成：扫描 ${report.scannedItems} 个条目，新增冲突 ${report.conflicts}，` +
          `补齐收藏 ${report.importedCollection}，导入进度 ${report.importedProgress} 条`,
      );
      if (report.messages.length > 0) setError(report.messages.join('；'));
      await refresh();
    } catch (err) {
      setError(friendlyError(err));
    } finally {
      setBusy(null);
    }
  }

  async function handleDiscard(id: string) {
    setDiscarding(id);
    setError(null);
    setNotice(null);
    try {
      await discardSyncEntry(id);
      setNotice('已从推送队列移除该记录（云端不受影响）。');
      setQueue(await listSyncQueue());
      setStats(await syncStats());
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setDiscarding(null);
    }
  }

  async function handlePush() {
    setBusy('push');
    setError(null);
    setNotice(null);
    try {
      const report = await pushPending();
      setNotice(
        `推送完成：成功 ${report.succeeded} · 失败 ${report.failed} · 共尝试 ${report.attempted}`,
      );
      await refresh();
    } catch (err) {
      setError(friendlyError(err));
    } finally {
      setBusy(null);
    }
  }

  async function handleResolve(choice: 'local' | 'cloud') {
    if (selected.size === 0) {
      setError('请先勾选要裁决的冲突');
      return;
    }
    setBusy('resolve');
    setError(null);
    setNotice(null);
    try {
      const report = await resolveConflicts([...selected], choice);
      setNotice(
        `已裁决 ${report.resolved} 条冲突（${choice === 'local' ? '采用本地' : '采用云端'}）`,
      );
      setSelected(new Set());
      await refresh();
    } catch (err) {
      setError(friendlyError(err));
    } finally {
      setBusy(null);
    }
  }

  function toggleOne(id: string) {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleAll() {
    setSelected((prev) =>
      prev.size === conflicts.length ? new Set() : new Set(conflicts.map((item) => item.id)),
    );
  }

  if (loading) {
    return (
      <div className="page">
        <div className="placeholder">正在读取同步状态…</div>
      </div>
    );
  }

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1>Bangumi 同步</h1>
          <p className="muted">本地变更先入队，手动触发后按限速推送；云端拉取在启动时自动执行。</p>
        </div>
        <div className="page-actions">
          <button type="button" className="btn" onClick={() => void refresh()}>
            刷新
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => void handlePull()}
            disabled={busy !== null}
          >
            {busy === 'pull' ? '拉取中…' : '拉取云端'}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => void handlePush()}
            disabled={busy !== null || (stats?.pending ?? 0) === 0}
          >
            {busy === 'push' ? '推送中…' : `推送待发${stats ? `（${stats.pending}）` : ''}`}
          </button>
        </div>
      </header>

      {error && <div className="banner banner-error">{error}</div>}
      {notice && <div className="banner banner-ok">{notice}</div>}

      {stats && !stats.authorized && (
        <div className="banner banner-warn">
          尚未授权 Bangumi。请到「设置 → Bangumi 授权」填写 App ID / App Secret
          并点击授权；在此之前「拉取/推送」会提示未授权。
        </div>
      )}

      {stats && (
        <div className="stat-row">
          <div className="stat">
            <span className="stat-value">{stats.pending}</span>
            <span className="stat-label">待发送</span>
          </div>
          <div className="stat">
            <span className="stat-value">{stats.failed}</span>
            <span className="stat-label">失败待重试</span>
          </div>
          <div className="stat">
            <span className="stat-value">{stats.done}</span>
            <span className="stat-label">已完成</span>
          </div>
          <div className="stat">
            <span className="stat-value">{stats.unresolvedConflicts}</span>
            <span className="stat-label">未解决冲突</span>
          </div>
          <div className="stat">
            <span className={stats.authorized ? 'stat-value ok' : 'stat-value warn'}>
              {stats.authorized ? '已授权' : '未授权'}
            </span>
            <span className="stat-label">账号状态</span>
          </div>
        </div>
      )}

      <section className="card">
        <div className="card-head">
          <h2>冲突（{conflicts.length}）</h2>
          <div className="row-actions">
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => void handleResolve('local')}
              disabled={busy !== null || selected.size === 0}
            >
              采用本地
            </button>
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => void handleResolve('cloud')}
              disabled={busy !== null || selected.size === 0}
            >
              采用云端
            </button>
          </div>
        </div>
        {conflicts.length === 0 ? (
          <div className="placeholder small">
            暂无未解决冲突。拉取云端后，两侧不一致的字段会出现在这里。
          </div>
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th className="col-check">
                    <input
                      type="checkbox"
                      checked={selected.size === conflicts.length && conflicts.length > 0}
                      onChange={toggleAll}
                      aria-label="全选冲突"
                    />
                  </th>
                  <th>条目</th>
                  <th className="col-field">字段</th>
                  <th>本地</th>
                  <th>云端</th>
                </tr>
              </thead>
              <tbody>
                {conflicts.map((row) => (
                  <tr key={row.id}>
                    <td className="col-check">
                      <input
                        type="checkbox"
                        checked={selected.has(row.id)}
                        onChange={() => toggleOne(row.id)}
                        aria-label={`选择冲突 ${row.id}`}
                      />
                    </td>
                    <td className="cell-title">{row.itemTitle ?? row.itemId ?? '—'}</td>
                    <td className="muted">{fieldLabel(row.field)}</td>
                    <td>
                      <span className="diff diff-local">
                        {displayValue(row.field, row.localValue)}
                      </span>
                      {row.localTs && <span className="diff-ts">{row.localTs}</span>}
                    </td>
                    <td>
                      <span className="diff diff-cloud">
                        {displayValue(row.field, row.cloudValue)}
                      </span>
                      {row.cloudTs && <span className="diff-ts">{row.cloudTs}</span>}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="card">
        <h2>推送队列（{queue.length}）</h2>
        {queue.length === 0 ? (
          <div className="placeholder small">
            队列为空。修改已绑定条目的收藏、评分或进度后会出现在这里。
          </div>
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>条目</th>
                  <th className="col-op">操作</th>
                  <th className="col-status">状态</th>
                  <th className="col-attempts">尝试</th>
                  <th className="col-retry">下次重试</th>
                  <th className="col-time">入队时间</th>
                  <th className="col-queue-actions">操作</th>
                </tr>
              </thead>
              <tbody>
                {queue.map((row) => (
                  <tr key={row.id}>
                    <td className="cell-title">{row.itemTitle ?? row.itemId ?? '—'}</td>
                    <td className="muted">{OP_LABELS[row.op] ?? row.op}</td>
                    <td>
                      <span className={`badge badge-sync-${row.status}`}>
                        {STATUS_LABELS[row.status] ?? row.status}
                      </span>
                    </td>
                    <td className="muted">{row.attempts}</td>
                    <td className="muted">{row.nextRetryAt ?? '—'}</td>
                    <td className="muted">{row.createdAt}</td>
                    <td>
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => void handleDiscard(row.id)}
                        disabled={discarding !== null || row.status === 'sending'}
                        title={
                          row.status === 'sending'
                            ? '正在发送中，稍后再试'
                            : '只删除本地队列记录，不影响云端数据'
                        }
                      >
                        {discarding === row.id ? '删除中…' : '删除'}
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
      </section>
    </div>
  );
}
