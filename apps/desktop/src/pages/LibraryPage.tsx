import { useEffect, useMemo, useState } from 'react';

import {
  ApiError,
  autoMatch,
  listItems,
  listProgress,
  playNextEpisode,
  refreshMetadataBatch,
  runningGames,
  scanLibrary,
  startGame,
  itemCover,
  itemCovers,
  subscribeAppEvents,
  trashItems,
  type ItemKind,
  type ItemSummary,
} from '../api';
import ClearItemsDialog from '../components/ClearItemsDialog';
import EpisodePicker from '../components/EpisodePicker';
import IssueBanner from '../components/IssueBanner';

interface Props {
  version: number;
  onOpen: (id: string) => void;
  onConfigure: () => void;
}

const KIND_LABEL: Record<ItemKind, string> = { anime: '动画', game: '游戏' };

/** 「未分类」在筛选里用 0 表示：Bangumi 的状态码是 1~5，不会冲突。 */
const UNCLASSIFIED = 0;

/**
 * Bangumi 收藏状态码：1 想看/想玩、2 看过/玩过、3 在看/在玩、4 搁置、5 抛弃。
 *
 * 动画与游戏的措辞不同（看 vs 玩），跟 Bangumi 网页端保持一致；
 * 「全部」分类下按动画措辞显示。
 */
const STATUS_ANIME: [number, string][] = [
  [1, '想看'],
  [2, '看过'],
  [3, '在看'],
  [4, '搁置'],
  [5, '抛弃'],
];
const STATUS_GAME: [number, string][] = [
  [1, '想玩'],
  [2, '玩过'],
  [3, '在玩'],
  [4, '搁置'],
  [5, '抛弃'],
];
/** 不区分类型时（全部条目）：动画用「看」、游戏用「玩」，标签里两者并列。 */
const STATUS_MIXED: [number, string][] = [
  [1, '想看/想玩'],
  [2, '看过/玩过'],
  [3, '在看/在玩'],
  [4, '搁置'],
  [5, '抛弃'],
];

function statusOptions(kind: ItemKind | null): [number, string][] {
  if (kind === 'game') return STATUS_GAME;
  if (kind === 'anime') return STATUS_ANIME;
  return STATUS_MIXED;
}

function statusLabel(kind: ItemKind, code: number | null): string | null {
  if (code === null) return null;
  const table = kind === 'game' ? STATUS_GAME : STATUS_ANIME;
  return table.find(([value]) => value === code)?.[1] ?? null;
}

type SortKey = 'title' | 'updated' | 'files';
type BoundFilter = 'all' | 'bound' | 'unbound';

function describeMedia(item: ItemSummary): string {
  const { fileCount, episodeHint } = item.media;
  if (fileCount === 0) return '未发现文件';
  const unit = item.kind === 'anime' ? '个视频' : '个可执行';
  return episodeHint ? `${fileCount} ${unit} · ${episodeHint}` : `${fileCount} ${unit}`;
}

export default function LibraryPage({ version, onOpen, onConfigure }: Props) {
  const [items, setItems] = useState<ItemSummary[]>([]);
  const [kind, setKind] = useState<ItemKind | null>(null);
  const [keyword, setKeyword] = useState('');
  const [sort, setSort] = useState<SortKey>('title');
  const [bound, setBound] = useState<BoundFilter>('all');
  // 收藏状态筛选：null 表示不限。
  const [status, setStatus] = useState<number | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [batchBusy, setBatchBusy] = useState<'refresh' | 'trash' | null>(null);
  const [batchProgress, setBatchProgress] = useState<{ done: number; total: number } | null>(null);
  // 表格 / 封面网格两种视图，选择记在本地。
  const [viewMode, setViewMode] = useState<'table' | 'grid'>(() =>
    localStorage.getItem('lagm.viewMode') === 'grid' ? 'grid' : 'table',
  );
  const [covers, setCovers] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(true);
  const [scanning, setScanning] = useState(false);
  const [matching, setMatching] = useState(false);
  const [clearing, setClearing] = useState(false);
  // 选集面板：记录当前要展开的条目。
  const [picking, setPicking] = useState<{ id: string; title: string } | null>(null);
  const [runningIds, setRunningIds] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [issues, setIssues] = useState<string[]>([]);
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    runningGames()
      .then((games) => {
        if (!cancelled) setRunningIds(games.map((game) => game.itemId));
      })
      .catch((err: unknown) => console.warn('读取运行状态失败', err));
    return () => {
      cancelled = true;
    };
  }, [version]);

  useEffect(() => {
    localStorage.setItem('lagm.viewMode', viewMode);
  }, [viewMode]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    subscribeAppEvents((envelope) => {
      if (envelope.topic === 'metadata:cover:ready') {
        const itemId = String(envelope.payload['itemId'] ?? '');
        if (itemId !== '') {
          itemCover(itemId)
            .then((url) => {
              if (url) setCovers((prev) => ({ ...prev, [itemId]: url }));
            })
            .catch(() => {
              // 封面是装饰，失败不提示。
            });
        }
        return;
      }
      if (envelope.topic === 'metadata:batch:progress') {
        const done = Number(envelope.payload['done']);
        const total = Number(envelope.payload['total']);
        if (Number.isFinite(done) && Number.isFinite(total)) setBatchProgress({ done, total });
        return;
      }
      if (!envelope.topic.startsWith('launcher:game:')) return;
      runningGames()
        .then((games) => {
          if (!disposed) setRunningIds(games.map((game) => game.itemId));
        })
        .catch((err: unknown) => console.warn('刷新运行状态失败', err));
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((err: unknown) => console.warn('订阅运行状态失败', err));

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    listItems(kind)
      .then((data) => {
        if (cancelled) return;
        setItems(data);
        // 列表变化后已选中的条目可能已经不存在，顺手清掉。
        setSelected((current) => current.filter((id) => data.some((item) => item.id === id)));
        setError(null);
      })
      .catch((err: unknown) => {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [kind, version]);

  const visible = useMemo(() => {
    const trimmed = keyword.trim().toLowerCase();
    const filtered = items.filter((item) => {
      if (trimmed) {
        const hit =
          item.title.toLowerCase().includes(trimmed) ||
          item.folderPath.toLowerCase().includes(trimmed);
        if (!hit) return false;
      }
      if (bound === 'bound' && item.bangumiSubjectId === null) return false;
      if (bound === 'unbound' && item.bangumiSubjectId !== null) return false;
      if (status === UNCLASSIFIED) {
        if (item.collectionStatus !== null) return false;
      } else if (status !== null && item.collectionStatus !== status) {
        return false;
      }
      return true;
    });
    const sorted = [...filtered];
    if (sort === 'title') {
      sorted.sort((left, right) => left.title.localeCompare(right.title, 'zh-Hans-CN'));
    } else if (sort === 'updated') {
      sorted.sort((left, right) => right.updatedAt.localeCompare(left.updatedAt));
    } else {
      sorted.sort((left, right) => right.media.fileCount - left.media.fileCount);
    }
    return sorted;
  }, [items, keyword, bound, sort, status]);

  /** 各收藏状态的条目数：不受搜索与状态筛选影响，否则数字会随点击跳动。 */
  const statusCounts = useMemo(() => {
    const counts = new Map<number, number>();
    for (const item of items) {
      if (item.collectionStatus === null) continue;
      counts.set(item.collectionStatus, (counts.get(item.collectionStatus) ?? 0) + 1);
    }
    return counts;
  }, [items]);

  const classifiedTotal = useMemo(
    () => [...statusCounts.values()].reduce((sum, value) => sum + value, 0),
    [statusCounts],
  );
  const unclassified = items.length - classifiedTotal;

  // 封面视图才去取图；idsKey 稳定，避免 items 每次新建数组导致循环请求。
  const idsKey = visible.map((item) => item.id).join(',');
  useEffect(() => {
    if (viewMode !== 'grid' || idsKey === '') return;
    let cancelled = false;
    itemCovers(idsKey.split(','))
      .then((map) => {
        if (!cancelled) setCovers((prev) => ({ ...prev, ...map }));
      })
      .catch((err: unknown) => console.warn('读取封面失败', err));
    return () => {
      cancelled = true;
    };
  }, [viewMode, idsKey]);

  const allVisibleSelected =
    visible.length > 0 && visible.every((item) => selected.includes(item.id));

  async function handleScan() {
    setScanning(true);
    setError(null);
    setIssues([]);
    setNotice(null);
    try {
      const summary = await scanLibrary();
      let message = `扫描完成：新增 ${summary.totalCreated} · 更新 ${summary.totalUpdated} · 未变 ${summary.totalUnchanged}`;
      setIssues(summary.messages);
      setItems(await listItems(kind));
      // 扫描后自动匹配并拉取云端（未授权时静默跳过）。
      const matched = await tryAutoMatch();
      if (matched) {
        message += `；${matched}`;
        setItems(await listItems(kind));
      }
      setNotice(message);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setScanning(false);
    }
  }

  /** 返回一句结果描述；未授权或失败返回 null（不打断扫描流程）。 */
  async function tryAutoMatch(): Promise<string | null> {
    try {
      const report = await autoMatch();
      if (report.unboundScanned === 0) return null;
      let text = `自动匹配：新绑定 ${report.bound} 个，缓存元数据 ${report.metadataCached} 个，拉取 ${report.pulled} 条`;
      if (report.conflicts > 0) text += `，冲突 ${report.conflicts} 条`;
      if (report.unmatched.length > 0) text += `，未绑定 ${report.unmatched.length} 个`;
      const problems: string[] = [];
      if (report.unmatched.length > 0) {
        problems.push(`未自动匹配：${report.unmatched.join('；')}`);
      }
      problems.push(...report.messages);
      setIssues(problems);
      return text;
    } catch (err) {
      if (err instanceof ApiError && err.code === 'sync_not_authorized') return null;
      console.warn('自动匹配失败', err);
      return null;
    }
  }

  async function handleAutoMatch() {
    setMatching(true);
    setError(null);
    setIssues([]);
    setNotice(null);
    try {
      const report = await autoMatch();
      setNotice(
        `自动匹配：检查 ${report.unboundScanned} 个未绑定，新绑定 ${report.bound} 个，缓存元数据 ${report.metadataCached} 个，拉取 ${report.pulled} 条，冲突 ${report.conflicts} 条`,
      );
      const problems: string[] = [];
      if (report.unmatched.length > 0) {
        problems.push(`未自动匹配：${report.unmatched.join('；')}`);
      }
      problems.push(...report.messages);
      setIssues(problems);
      setItems(await listItems(kind));
    } catch (err) {
      if (err instanceof ApiError && err.code === 'sync_not_authorized') {
        setError('尚未授权 Bangumi，请先到设置页完成授权。');
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setMatching(false);
    }
  }

  function toggleOne(id: string) {
    setSelected((current) =>
      current.includes(id) ? current.filter((item) => item !== id) : [...current, id],
    );
  }

  function toggleAllVisible() {
    if (allVisibleSelected) {
      setSelected((current) => current.filter((id) => !visible.some((item) => item.id === id)));
    } else {
      setSelected((current) => {
        const next = new Set(current);
        for (const item of visible) next.add(item.id);
        return Array.from(next);
      });
    }
  }

  async function handleBatchRefresh() {
    if (selected.length === 0) return;
    setBatchBusy('refresh');
    setBatchProgress({ done: 0, total: selected.length });
    setError(null);
    setIssues([]);
    setNotice(null);
    try {
      const report = await refreshMetadataBatch(selected);
      let message = `刷新完成：成功 ${report.refreshed} 个`;
      if (report.skipped > 0) message += `，跳过未绑定 ${report.skipped} 个`;
      if (report.failed > 0) message += `，失败 ${report.failed} 个`;
      setNotice(message);
      setIssues(report.messages);
      setItems(await listItems(kind));
    } catch (err) {
      if (err instanceof ApiError && err.code === 'sync_not_authorized') {
        setError('尚未授权 Bangumi，请先到设置页完成授权。');
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setBatchBusy(null);
      setBatchProgress(null);
    }
  }

  async function handleBatchTrash() {
    if (selected.length === 0) return;
    // 破坏性操作：先确认要删多少条，逐条永久删除仍必须去详情页输入标题。
    if (!window.confirm(`将把 ${selected.length} 个条目移入系统回收站（可还原），继续？`)) return;
    setBatchBusy('trash');
    setError(null);
    setIssues([]);
    setNotice(null);
    try {
      const report = await trashItems(selected);
      let message = `已移入回收站 ${report.deleted} 个`;
      if (report.failed > 0) message += `，失败 ${report.failed} 个`;
      setNotice(message);
      setIssues(report.messages);
      setSelected([]);
      setItems(await listItems(kind));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBatchBusy(null);
    }
  }

  /** 开始观看：从「已看的最大集号」之后继续；没有进度就从第一集开始。 */
  async function handleStartWatching(item: ItemSummary) {
    setError(null);
    setNotice(null);
    try {
      const rows = await listProgress(item.id);
      const lastWatched = rows
        .filter((row) => row.watchedSeconds > 0)
        .reduce((max, row) => Math.max(max, row.episode), 0);
      const session = await playNextEpisode(item.id, lastWatched);
      setNotice(
        session
          ? `已开始播放《${item.title}》第 ${session.episode} 集`
          : `《${item.title}》已是最后一集`,
      );
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleLaunchGame(item: ItemSummary) {
    setError(null);
    setNotice(null);
    try {
      const running = await startGame(item.id);
      setNotice(`已启动《${item.title}》（pid ${running.pid}）`);
      setRunningIds((ids) => (ids.includes(item.id) ? ids : [...ids, item.id]));
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  const filteredOut = items.length - visible.length;

  return (
    <div className="page">
      <header className="page-head">
        <div>
          <h1>条目库</h1>
          <p className="muted">
            共 {items.length} 个条目
            {filteredOut > 0 ? ` · 已筛选 ${visible.length} 个` : ''}
          </p>
        </div>
        <div className="page-actions">
          <button type="button" className="btn" onClick={onConfigure}>
            根目录设置
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => setClearing(true)}
            title="只清除软件里的条目记录，不动磁盘文件"
          >
            清空条目
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => void handleAutoMatch()}
            disabled={matching}
            title="搜索 Bangumi 并自动绑定当前未绑定条目"
          >
            {matching ? '匹配中…' : '自动匹配'}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={handleScan}
            disabled={scanning}
          >
            {scanning ? '扫描中…' : '扫描目录'}
          </button>
        </div>
      </header>

      <div className="toolbar">
        <div className="segmented">
          {[
            { key: null, label: '全部' },
            { key: 'anime' as ItemKind, label: '动画' },
            { key: 'game' as ItemKind, label: '游戏' },
          ].map((option) => (
            <button
              key={option.label}
              type="button"
              className={kind === option.key ? 'segment is-active' : 'segment'}
              onClick={() => setKind(option.key)}
            >
              {option.label}
            </button>
          ))}
        </div>
        <div className="toolbar-right">
          <select
            className="select"
            value={bound}
            onChange={(event) => setBound(event.target.value as BoundFilter)}
            title="按 Bangumi 绑定状态筛选"
          >
            <option value="all">全部绑定状态</option>
            <option value="bound">只看已绑定</option>
            <option value="unbound">只看未绑定</option>
          </select>
          <select
            className="select"
            value={sort}
            onChange={(event) => setSort(event.target.value as SortKey)}
            title="排序方式"
          >
            <option value="title">按标题</option>
            <option value="updated">按最近更新</option>
            <option value="files">按文件数</option>
          </select>
          <input
            className="input search"
            placeholder="按标题或路径筛选"
            value={keyword}
            onChange={(event) => setKeyword(event.target.value)}
          />
          <div className="segmented" title="切换显示方式">
            <button
              type="button"
              className={viewMode === 'table' ? 'segment is-active' : 'segment'}
              onClick={() => setViewMode('table')}
            >
              列表
            </button>
            <button
              type="button"
              className={viewMode === 'grid' ? 'segment is-active' : 'segment'}
              onClick={() => setViewMode('grid')}
            >
              封面
            </button>
          </div>
        </div>
      </div>

      <div className="status-tabs">
        <button
          type="button"
          className={status === null ? 'status-tab is-active' : 'status-tab'}
          onClick={() => setStatus(null)}
        >
          全部
          <span className="status-count">{items.length}</span>
        </button>
        {statusOptions(kind).map(([value, label]) => (
          <button
            key={value}
            type="button"
            className={status === value ? 'status-tab is-active' : 'status-tab'}
            onClick={() => setStatus(status === value ? null : value)}
          >
            {label}
            <span className="status-count">{statusCounts.get(value) ?? 0}</span>
          </button>
        ))}
        {unclassified > 0 && (
          <button
            type="button"
            className={status === UNCLASSIFIED ? 'status-tab is-active' : 'status-tab'}
            onClick={() => setStatus(status === UNCLASSIFIED ? null : UNCLASSIFIED)}
            title="未绑定 Bangumi，或云端没有收藏状态"
          >
            未分类
            <span className="status-count">{unclassified}</span>
          </button>
        )}
      </div>

      {selected.length > 0 && (
        <div className="batch-bar">
          <span>已选 {selected.length} 个</span>
          <div className="row-actions">
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => void handleBatchRefresh()}
              disabled={batchBusy !== null}
            >
              {batchBusy === 'refresh'
                ? batchProgress
                  ? `刷新中 ${batchProgress.done}/${batchProgress.total}`
                  : '刷新中…'
                : '刷新元数据'}
            </button>
            <button
              type="button"
              className="btn btn-tiny btn-danger"
              onClick={() => void handleBatchTrash()}
              disabled={batchBusy !== null}
            >
              {batchBusy === 'trash' ? '删除中…' : '移入回收站'}
            </button>
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => setSelected([])}
              disabled={batchBusy !== null}
            >
              清除选择
            </button>
          </div>
        </div>
      )}

      {error && <div className="banner banner-error">{error}</div>}
      <IssueBanner title="自动匹配提示" items={issues} onDismiss={() => setIssues([])} />
      {notice && <div className="banner banner-ok">{notice}</div>}

      {loading ? (
        <div className="placeholder">正在读取条目…</div>
      ) : visible.length === 0 ? (
        <div className="placeholder">
          <p>还没有条目。</p>
          <p className="muted">
            先在设置里配置动画根目录与游戏根目录（二者不能互为父子目录），再点击「扫描目录」。
          </p>
          <button type="button" className="btn" onClick={onConfigure}>
            去配置
          </button>
        </div>
      ) : viewMode === 'grid' ? (
        <div className="cover-grid">
          {visible.map((item) => (
            <div
              key={item.id}
              className={selected.includes(item.id) ? 'cover-card is-selected' : 'cover-card'}
              onClick={() => onOpen(item.id)}
            >
              <div className="cover-frame">
                {covers[item.id] ? (
                  <img src={covers[item.id]} alt={`${item.title} 封面`} loading="lazy" />
                ) : (
                  <div className="cover-empty">{item.title.slice(0, 2)}</div>
                )}
                <label className="cover-check" onClick={(event) => event.stopPropagation()}>
                  <input
                    type="checkbox"
                    checked={selected.includes(item.id)}
                    onChange={() => toggleOne(item.id)}
                    aria-label={`选择 ${item.title}`}
                  />
                </label>
                {runningIds.includes(item.id) && (
                  <span className="badge badge-running cover-flag">运行中</span>
                )}
                {/* 悬停显形：直接盖在封面上，避免常驻按钮占位把卡片撑高。 */}
                <div className="cover-hover" onClick={(event) => event.stopPropagation()}>
                  {item.kind === 'anime' ? (
                    <>
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => setPicking({ id: item.id, title: item.title })}
                        title="列出所有集，点一集直接播放"
                      >
                        选集
                      </button>
                      <button
                        type="button"
                        className="btn btn-tiny btn-primary"
                        onClick={() => void handleStartWatching(item)}
                        title="从上次看到的下一集继续"
                      >
                        开始观看
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      className="btn btn-tiny btn-primary"
                      onClick={() => void handleLaunchGame(item)}
                      title="启动游戏"
                    >
                      启动
                    </button>
                  )}
                </div>
              </div>
              <div className="cover-title" title={item.title}>
                {item.title}
              </div>
              <div className="cover-meta muted">
                <span className={`badge badge-${item.kind}`}>{KIND_LABEL[item.kind]}</span>
                {statusLabel(item.kind, item.collectionStatus) ? (
                  <span className="badge badge-status">
                    {statusLabel(item.kind, item.collectionStatus)}
                  </span>
                ) : null}
                <span>{describeMedia(item)}</span>
              </div>
            </div>
          ))}
        </div>
      ) : (
        <div className="table-wrap">
          <table className="table">
            <thead>
              <tr>
                <th className="col-check">
                  <input
                    type="checkbox"
                    checked={allVisibleSelected}
                    onChange={toggleAllVisible}
                    aria-label="全选当前列表"
                  />
                </th>
                <th>标题</th>
                <th className="col-kind">类型</th>
                <th className="col-media">文件</th>
                <th className="col-bangumi">Bangumi</th>
                <th className="col-status">状态</th>
                <th>文件夹</th>
                <th className="col-actions">操作</th>
              </tr>
            </thead>
            <tbody>
              {visible.map((item) => (
                <tr key={item.id} onClick={() => onOpen(item.id)} className="row">
                  <td className="col-check" onClick={(event) => event.stopPropagation()}>
                    <input
                      type="checkbox"
                      checked={selected.includes(item.id)}
                      onChange={() => toggleOne(item.id)}
                      aria-label={`选择 ${item.title}`}
                    />
                  </td>
                  <td className="cell-title">
                    {item.title}
                    {runningIds.includes(item.id) && (
                      <span className="badge badge-running">运行中</span>
                    )}
                  </td>
                  <td>
                    <span className={`badge badge-${item.kind}`}>{KIND_LABEL[item.kind]}</span>
                  </td>
                  <td className="muted">{describeMedia(item)}</td>
                  <td>
                    {item.bangumiSubjectId === null ? (
                      <span className="muted">未绑定</span>
                    ) : (
                      <span className="badge badge-bound">#{item.bangumiSubjectId}</span>
                    )}
                  </td>
                  <td className="col-status">
                    {statusLabel(item.kind, item.collectionStatus) ? (
                      <span className="badge badge-status">
                        {statusLabel(item.kind, item.collectionStatus)}
                      </span>
                    ) : (
                      <span className="muted">—</span>
                    )}
                  </td>
                  <td className="cell-path" title={item.folderPath}>
                    {item.folderPath}
                  </td>
                  <td className="col-actions" onClick={(event) => event.stopPropagation()}>
                    <div className="row-quick">
                      {item.kind === 'anime' ? (
                        <>
                          <button
                            type="button"
                            className="btn btn-tiny"
                            onClick={() => setPicking({ id: item.id, title: item.title })}
                            title="列出所有集，点一集直接播放"
                          >
                            选集
                          </button>
                          <button
                            type="button"
                            className="btn btn-tiny"
                            onClick={() => void handleStartWatching(item)}
                            title="从上次看到的下一集继续"
                          >
                            观看
                          </button>
                        </>
                      ) : (
                        <button
                          type="button"
                          className="btn btn-tiny"
                          onClick={() => void handleLaunchGame(item)}
                          title="启动游戏"
                        >
                          启动
                        </button>
                      )}
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {picking && (
        <EpisodePicker
          itemId={picking.id}
          title={picking.title}
          onClose={() => setPicking(null)}
          onPlayed={() => setNotice(`已开始播放《${picking.title}》`)}
        />
      )}

      {clearing && (
        <ClearItemsDialog
          kind={kind}
          onClose={() => setClearing(false)}
          onCleared={(removed) => {
            setClearing(false);
            setSelected([]);
            setNotice(`已清空 ${removed} 个条目记录（磁盘文件已保留）。`);
            listItems(kind)
              .then(setItems)
              .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)));
          }}
        />
      )}
    </div>
  );
}
