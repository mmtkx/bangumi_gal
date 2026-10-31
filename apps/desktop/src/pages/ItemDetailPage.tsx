import { useCallback, useEffect, useMemo, useRef, useState } from 'react';

import {
  ApiError,
  activeSessions,
  bangumiAccount,
  bangumiSubjectPreview,
  bindBangumiSubject,
  cacheMetadata,
  formatBytes,
  formatDuration,
  gameStatus as fetchGameStatus,
  getItem,
  itemCover,
  listProgress,
  openExternal,
  openInExplorer,
  playEpisode,
  pullItemNow,
  searchBangumi,
  setCollection,
  setGameBinding,
  setProgress as saveProgress,
  startGame,
  subscribeAppEvents,
  type ActiveSession,
  type GameStatus,
  type ItemDetail,
  type ProgressRow,
  type SubjectPreview,
  type SubjectSearchHit,
} from '../api';
import DeleteItemDialog from '../components/DeleteItemDialog';
import ExePicker from '../components/ExePicker';
import { mountUiPlugins, type MountedPlugin } from '../plugins/host';

const SUBJECT_TYPE_LABEL: Record<number, string> = {
  1: '书籍',
  2: '动画',
  3: '音乐',
  4: '游戏',
  6: '三次元',
};

const COLLECTION_LABELS: Record<number, string> = {
  1: '想看',
  2: '看过',
  3: '在看',
  4: '搁置',
  5: '抛弃',
};

/** 游戏在 Bangumi 上用的是同一套编号，但文案是「想玩 / 玩过 / 在玩」。 */
const GAME_COLLECTION_LABELS: Record<number, string> = {
  1: '想玩',
  2: '玩过',
  3: '在玩',
  4: '搁置',
  5: '抛弃',
};

interface Props {
  id: string;
  onBack: () => void;
  onChanged: () => void;
}

/** 事件 payload 的字段名由后端统一转成 camelCase。 */
function payloadString(payload: Record<string, unknown>, key: string): string | null {
  const value = payload[key];
  return typeof value === 'string' ? value : null;
}

/** 支持「12:34」与「754」两种输入，返回秒数；非法返回 null。 */
function parseDuration(input: string): number | null {
  const text = input.trim();
  if (text === '') return null;
  if (!text.includes(':')) {
    const seconds = Number(text);
    return Number.isFinite(seconds) && seconds >= 0 ? Math.floor(seconds) : null;
  }
  const parts = text.split(':');
  if (parts.length !== 2) return null;
  const minutes = Number(parts[0]);
  const seconds = Number(parts[1]);
  if (!Number.isFinite(minutes) || !Number.isFinite(seconds)) return null;
  if (minutes < 0 || seconds < 0 || seconds >= 60) return null;
  return Math.floor(minutes) * 60 + Math.floor(seconds);
}

export default function ItemDetailPage({ id, onBack, onChanged }: Props) {
  const [detail, setDetail] = useState<ItemDetail | null>(null);
  const [game, setGame] = useState<GameStatus | null>(null);
  const [progress, setProgress] = useState<ProgressRow[]>([]);
  const [sessions, setSessions] = useState<ActiveSession[]>([]);
  const [subjectInput, setSubjectInput] = useState('');
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [savingSubject, setSavingSubject] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const [deleteOpen, setDeleteOpen] = useState(false);
  // 连播：详情页记住开关，播完自动接下一集。
  const [autoNext, setAutoNext] = useState(() => localStorage.getItem('lagm.autoNext') === '1');
  const [activeEpisode, setActiveEpisode] = useState<number | null>(null);
  const [cover, setCover] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    itemCover(id)
      .then((data) => {
        if (!cancelled) setCover(data);
      })
      .catch((err: unknown) => {
        // 封面失败不影响页面，静默记录即可。
        console.warn('读取封面失败', err);
        if (!cancelled) setCover(null);
      });
    return () => {
      cancelled = true;
    };
  }, [id, detail?.item.bangumiSubjectId]);

  useEffect(() => {
    // 开关只写偏好；真正的「播完接下一集」由 App 层全局订阅驱动。
    localStorage.setItem('lagm.autoNext', autoNext ? '1' : '0');
  }, [autoNext]);
  const [editing, setEditing] = useState<{ episode: number; value: string } | null>(null);
  const [collectionStatus, setCollectionStatus] = useState('');
  const [rating, setRating] = useState('');
  const [savingCollection, setSavingCollection] = useState(false);
  const [cachingMeta, setCachingMeta] = useState(false);
  const [pullingCloud, setPullingCloud] = useState(false);
  const [searching, setSearching] = useState(false);
  const [candidates, setCandidates] = useState<SubjectSearchHit[] | null>(null);
  // 右侧预览：loadingId 记录正在拉取哪一条，避免误显示上一条的内容。
  const [preview, setPreview] = useState<SubjectPreview | null>(null);
  const [previewLoadingId, setPreviewLoadingId] = useState<number | null>(null);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [previewTarget, setPreviewTarget] = useState<number | null>(null);
  const pluginContainerRef = useRef<HTMLDivElement | null>(null);
  const [pluginPanelCount, setPluginPanelCount] = useState(0);

  // 来源统计：同一集可能有多个压制组版本，详情页据此提示区别。
  const groups = useMemo(() => {
    const counts = new Map<string, number>();
    for (const file of detail?.files ?? []) {
      const key = file.group ?? '主目录';
      counts.set(key, (counts.get(key) ?? 0) + 1);
    }
    return Array.from(counts.entries());
  }, [detail]);

  const load = useCallback(async () => {
    setLoading(true);
    try {
      const data = await getItem(id);
      setDetail(data);
      setSubjectInput(
        data.item.bangumiSubjectId === null ? '' : String(data.item.bangumiSubjectId),
      );
      setCollectionStatus(
        data.item.collectionStatus === null ? '' : String(data.item.collectionStatus),
      );
      setRating(data.item.rating === null ? '' : String(data.item.rating));
      if (data.item.kind === 'game') {
        setGame(await fetchGameStatus(id));
      } else {
        setProgress(await listProgress(id));
        const active = await activeSessions();
        setSessions(active.filter((session) => session.itemId === id));
      }
      setError(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setLoading(false);
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  // 挂载 UI 插件面板；插件异常只影响自身容器。
  useEffect(() => {
    const container = pluginContainerRef.current;
    if (!container) return;
    let disposed = false;
    let mounted: MountedPlugin[] = [];
    container.replaceChildren();
    void (async () => {
      const result = await mountUiPlugins('library.item.detail.panel', container, { itemId: id });
      if (disposed) {
        result.forEach((item) => item.dispose());
        return;
      }
      mounted = result;
      setPluginPanelCount(result.length);
    })();
    return () => {
      disposed = true;
      mounted.forEach((item) => item.dispose());
      container.replaceChildren();
      setPluginPanelCount(0);
    };
  }, [id, loading]);

  const refreshGame = useCallback(async () => {
    try {
      setGame(await fetchGameStatus(id));
    } catch (err) {
      console.warn('刷新游戏状态失败', err);
    }
  }, [id]);

  const refreshPlayback = useCallback(async () => {
    try {
      setProgress(await listProgress(id));
      const active = await activeSessions();
      setSessions(active.filter((session) => session.itemId === id));
    } catch (err) {
      console.warn('刷新播放进度失败', err);
    }
  }, [id]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    subscribeAppEvents((envelope) => {
      if (payloadString(envelope.payload, 'itemId') !== id) return;
      if (envelope.topic.startsWith('launcher:game:')) {
        void refreshGame();
      } else if (envelope.topic.startsWith('player:session:')) {
        void refreshPlayback();
      }
    })
      .then((dispose) => {
        if (disposed) dispose();
        else unlisten = dispose;
      })
      .catch((err: unknown) => console.warn('订阅事件失败', err));

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, [id, refreshGame, refreshPlayback]);

  async function submitSubject(nextValue: number | null) {
    setSavingSubject(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await bindBangumiSubject(id, nextValue);
      setDetail((current) => (current ? { ...current, item: updated } : current));
      setSubjectInput(updated.bangumiSubjectId === null ? '' : String(updated.bangumiSubjectId));
      onChanged();

      if (updated.bangumiSubjectId === null) {
        setNotice('已解除绑定');
        return;
      }

      // 绑定后立刻把账号里已有的收藏与进度补到本地（没有的保持为空）。
      try {
        const pulled = await pullItemNow(id);
        setDetail((current) => (current ? { ...current, item: pulled } : current));
        setCollectionStatus(
          pulled.collectionStatus === null ? '' : String(pulled.collectionStatus),
        );
        setRating(pulled.rating === null ? '' : String(pulled.rating));
        setNotice(
          pulled.collectionStatus === null && pulled.rating === null
            ? `已绑定 #${updated.bangumiSubjectId}；云端没有收藏记录，保持未设置`
            : `已绑定 #${updated.bangumiSubjectId}，并已补上云端收藏与评分`,
        );
      } catch (err) {
        if (err instanceof ApiError && err.code === 'sync_not_authorized') {
          setNotice(`已绑定 #${updated.bangumiSubjectId}；授权 Bangumi 后可自动补上收藏数据`);
        } else {
          setNotice(`已绑定 #${updated.bangumiSubjectId}（拉取云端失败，可稍后重试）`);
        }
      }
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingSubject(false);
    }
  }

  function handleBind() {
    const raw = subjectInput.trim();
    if (raw === '') {
      setError('请输入 Bangumi 条目 ID');
      return;
    }
    const parsed = Number(raw);
    if (!Number.isInteger(parsed) || parsed <= 0) {
      setError('Bangumi 条目 ID 必须是正整数');
      return;
    }
    void submitSubject(parsed);
  }

  async function handlePreview(subjectId: number) {
    setPreviewLoadingId(subjectId);
    setPreviewTarget(subjectId);
    setPreviewError(null);
    try {
      setPreview(await bangumiSubjectPreview(subjectId));
    } catch (err) {
      setPreview(null);
      setPreviewError(
        err instanceof ApiError && err.code === 'sync_not_authorized'
          ? '尚未授权 Bangumi，请先到账号页完成授权。'
          : err instanceof Error
            ? err.message
            : String(err),
      );
    } finally {
      setPreviewLoadingId(null);
    }
  }

  async function handleOpenSubjectPage(url: string) {
    try {
      await openExternal(url);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleSearchBangumi() {
    setSearching(true);
    setError(null);
    setNotice(null);
    setCandidates(null);
    try {
      const hits = await searchBangumi(id);
      setCandidates(hits);
      if (hits.length === 0) setNotice('没有搜到结果，可以换个关键词或直接填 Bangumi ID。');
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSearching(false);
    }
  }

  async function handleBindCandidate(subjectId: number) {
    setCandidates(null);
    await submitSubject(subjectId);
  }

  async function handleStart() {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const running = await startGame(id);
      setNotice(`已启动「${running.title}」，pid ${running.pid}`);
      await refreshGame();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'binding_missing') {
        setNotice('该游戏还没有绑定可执行文件，请先选择。');
        setPickerOpen(true);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setBusy(false);
    }
  }

  async function handlePickedExe(exePath: string) {
    setPickerOpen(false);
    setBusy(true);
    setError(null);
    try {
      await setGameBinding(id, exePath);
      setNotice('已绑定可执行文件。');
      await refreshGame();
      // plan.md 6.3：首次启动时选完 exe 直接启动。
      await handleStart();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setBusy(false);
    }
  }

  async function handleOpenFolder() {
    setError(null);
    try {
      await openInExplorer(id);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleRevealFile(fileName: string) {
    setError(null);
    try {
      await openInExplorer(id, fileName);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleCopyPath() {
    const path = detail?.item.folderPath ?? '';
    if (path === '') return;
    setError(null);
    try {
      await navigator.clipboard.writeText(path);
      setNotice('目录路径已复制');
    } catch {
      // 某些 WebView 环境不提供 clipboard API，退回到文本框复制。
      const helper = document.createElement('textarea');
      helper.value = path;
      document.body.appendChild(helper);
      helper.select();
      const ok = document.execCommand('copy');
      helper.remove();
      if (ok) setNotice('目录路径已复制');
      else setError('复制失败，请手动选中路径复制');
    }
  }

  async function handlePlay(fileName: string) {
    setBusy(true);
    setError(null);
    setNotice(null);
    try {
      const session = await playEpisode(id, fileName);
      // 起点不一定真的生效：播放器不支持跳转时会从头播，这里必须如实说。
      let resumeHint = '';
      if (session.startSeconds > 0) {
        resumeHint = session.resumeApplied
          ? `，从 ${formatDuration(session.startSeconds)} 继续`
          : `（该播放器不支持跳转，已从头播放；上次进度 ${formatDuration(session.startSeconds)}）`;
      }
      setNotice(`已开始播放第 ${session.episode} 集${resumeHint}（pid ${session.pid}）`);
      setActiveEpisode(session.episode);
      await refreshPlayback();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'player_not_configured') {
        setError('尚未配置播放器，请到设置页填写播放器路径。');
      } else if (err instanceof ApiError && err.code === 'player_default_handler_not_found') {
        setError(
          '系统没有为这种视频设置默认播放器。请到设置页指定一个播放器，或在系统里设置默认应用。',
        );
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setBusy(false);
    }
  }

  async function handleSaveProgress(episode: number, raw: string) {
    const seconds = parseDuration(raw);
    if (seconds === null) {
      setError('请输入秒数或 mm:ss 格式，例如 754 或 12:34');
      return;
    }
    try {
      await saveProgress(id, episode, seconds);
      setEditing(null);
      setNotice(`第 ${episode} 集进度已校正为 ${formatDuration(seconds)}`);
      await refreshPlayback();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    }
  }

  async function handleSaveCollection() {
    setSavingCollection(true);
    setError(null);
    setNotice(null);
    try {
      const statusValue = collectionStatus === '' ? null : Number(collectionStatus);
      const ratingValue = rating === '' ? null : Number(rating);
      const info = await setCollection(id, statusValue, ratingValue);
      setDetail((current) =>
        current
          ? {
              ...current,
              item: {
                ...current.item,
                collectionStatus: info.collectionStatus,
                rating: info.rating,
                collectionUpdatedAt: info.collectionUpdatedAt,
              },
            }
          : current,
      );
      setNotice('收藏与评分已保存，并已进入同步队列（推送需在「同步」页手动触发）。');
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setSavingCollection(false);
    }
  }

  function handleDeleted() {
    setDeleteOpen(false);
    // 条目已不存在，先刷新列表再退回，避免详情页停在空数据上。
    onChanged();
    onBack();
  }

  async function handlePullCloud() {
    setPullingCloud(true);
    setError(null);
    setNotice(null);
    try {
      const updated = await pullItemNow(id);
      setDetail((current) => (current ? { ...current, item: updated } : current));
      setCollectionStatus(
        updated.collectionStatus === null ? '' : String(updated.collectionStatus),
      );
      setRating(updated.rating === null ? '' : String(updated.rating));
      if (updated.collectionStatus === null && updated.rating === null) {
        // 带上当前授权账号：网页上有数据却提示为空时，多半是授权了另一个账号。
        let who = '';
        try {
          const account = await bangumiAccount();
          const name = account.nickname || account.username;
          if (account.authorized && name) who = `（当前账号 ${name}）`;
        } catch {
          // 账号信息拿不到就只给通用提示。
        }
        setNotice(
          `云端没有这条目的收藏记录${who}，本地保持为空。若网页上确实已收藏，请确认这里授权的是同一个账号。`,
        );
      } else {
        setNotice('已拉取云端收藏与评分。');
      }
      onChanged();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'sync_not_authorized') {
        setError('尚未授权 Bangumi，请先到设置页完成授权后再拉取。');
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    } finally {
      setPullingCloud(false);
    }
  }

  async function handleCacheMetadata() {
    setCachingMeta(true);
    setError(null);
    setNotice(null);
    try {
      const result = await cacheMetadata(id);
      setNotice(`元数据已缓存到 ${result.cachedPath}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setCachingMeta(false);
    }
  }

  if (loading) {
    return (
      <div className="page">
        <div className="placeholder">正在读取条目详情…</div>
      </div>
    );
  }

  if (!detail) {
    return (
      <div className="page">
        <button type="button" className="btn btn-ghost" onClick={onBack}>
          ← 返回条目库
        </button>
        <div className="banner banner-error">{error ?? '条目不存在'}</div>
      </div>
    );
  }

  const { item, files } = detail;
  const isGame = item.kind === 'game';
  // 游戏与动画共用 Bangumi 的 1-5 编号，只是文案不同。
  const collectionLabels = isGame ? GAME_COLLECTION_LABELS : COLLECTION_LABELS;
  const progressByEpisode = new Map(progress.map((row) => [row.episode, row]));
  const activeSession = sessions[0];

  return (
    <div className="page">
      <header className="page-head detail-head">
        {cover && (
          <img className="detail-cover" src={cover} alt={`${item.title} 封面`} loading="lazy" />
        )}
        <div className="detail-head-main">
          <button type="button" className="btn btn-ghost back" onClick={onBack}>
            ← 返回条目库
          </button>
          <h1>{item.title}</h1>
          <p className="muted">
            <span className={`badge badge-${item.kind}`}>{isGame ? '游戏' : '动画'}</span>
            {isGame && game?.runningPid != null && (
              <span className="badge badge-running">运行中 · pid {game.runningPid}</span>
            )}
            {!isGame && activeSession && activeEpisode !== null && (
              <span className="badge badge-running">第 {activeEpisode} 集播放中</span>
            )}
            <span className="path-text" title={item.folderPath}>
              {item.folderPath}
            </span>
          </p>
          <div className="row-actions">
            <button type="button" className="btn btn-tiny" onClick={() => void handleOpenFolder()}>
              打开目录
            </button>
            <button type="button" className="btn btn-tiny" onClick={() => void handleCopyPath()}>
              复制路径
            </button>
            {!isGame && detail.files.some((file) => file.episode !== null) && (
              <label className="checkbox-inline" title="播完自动播放下一集">
                <input
                  type="checkbox"
                  checked={autoNext}
                  onChange={(event) => setAutoNext(event.target.checked)}
                />
                连播
              </label>
            )}
          </div>
        </div>
      </header>

      {error && <div className="banner banner-error">{error}</div>}
      {notice && <div className="banner banner-ok">{notice}</div>}

      {isGame && (
        <section className="card">
          <h2>启动</h2>
          {game?.binding ? (
            <dl className="meta-list">
              <div>
                <dt>可执行文件</dt>
                <dd className="mono">
                  {game.binding.resolvedExePath}
                  {!game.binding.exeExists && (
                    <span className="warn-text">（文件不存在，请重新绑定）</span>
                  )}
                </dd>
              </div>
              <div>
                <dt>绑定记录</dt>
                <dd className="mono">{game.binding.exePath}</dd>
              </div>
              <div>
                <dt>上次启动</dt>
                <dd>{game.binding.lastLaunchedAt ?? '未启动过'}</dd>
              </div>
            </dl>
          ) : (
            <p className="muted">还没有绑定可执行文件，首次启动会引导你选择。</p>
          )}
          <div className="inline-form">
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => void handleStart()}
              disabled={busy || game?.runningPid != null}
            >
              {game?.runningPid != null ? '运行中' : busy ? '处理中…' : '启动游戏'}
            </button>
            <button
              type="button"
              className="btn"
              onClick={() => setPickerOpen(true)}
              disabled={busy}
            >
              {game?.binding ? '更改可执行文件' : '选择可执行文件'}
            </button>
          </div>
          <p className="muted">
            运行状态由进程树轮询判定（每 2 秒）。退出游戏窗口后，状态会自动变为未运行。
          </p>
        </section>
      )}

      {!isGame && (
        <section className="card">
          <h2>播放</h2>
          <p className="muted">
            进度优先读取播放器上报的真实位置（目前支持
            PotPlayer，拖动进度条也会同步）；其他播放器退化为按 进程时长估算，可用「校正」修正。
            {activeSession && (
              <span className="warn-text">
                正在播放第 {activeSession.episode} 集 · 已{' '}
                {formatDuration(activeSession.positionSeconds)}
              </span>
            )}
          </p>
          {groups.length > 1 && (
            <div className="chip-row">
              <span className="muted">来源：</span>
              {groups.map(([group, count]) => (
                <span key={group} className="chip" title={group}>
                  {group} · {count}
                </span>
              ))}
            </div>
          )}
        </section>
      )}

      <section className="card">
        <h2>Bangumi 绑定</h2>
        <p className="muted">
          保存条目 ID；收藏、评分与播放进度会进入本地同步队列，联网推送需完成 OAuth
          授权后在「同步」页手动触发。
        </p>
        <div className="inline-form">
          <input
            className="input"
            inputMode="numeric"
            placeholder="Bangumi subject id，例如 400602"
            value={subjectInput}
            onChange={(event) => setSubjectInput(event.target.value)}
          />
          <button
            type="button"
            className="btn btn-primary"
            onClick={handleBind}
            disabled={savingSubject}
          >
            {savingSubject ? '保存中…' : '绑定'}
          </button>
          <button
            type="button"
            className="btn"
            onClick={() => void submitSubject(null)}
            disabled={savingSubject || item.bangumiSubjectId === null}
          >
            解除绑定
          </button>
        </div>
        <div className="row-actions">
          <button
            type="button"
            className="btn"
            onClick={() => void handleSearchBangumi()}
            disabled={searching}
            title="按条目标题搜索 Bangumi，手动选择正确条目"
          >
            {searching ? '搜索中…' : '搜索 Bangumi'}
          </button>
          {candidates && (
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => {
                setCandidates(null);
                setPreview(null);
                setPreviewError(null);
              }}
            >
              收起结果
            </button>
          )}
        </div>

        {candidates && candidates.length > 0 && (
          <div className="bangumi-picker">
            <div className="candidate-list">
              {candidates.map((hit) => (
                <div
                  key={hit.id}
                  className={preview?.id === hit.id ? 'candidate is-active' : 'candidate'}
                  onClick={() => void handlePreview(hit.id)}
                >
                  <div className="candidate-title">
                    <strong>{hit.nameCn ?? hit.name}</strong>
                    {hit.nameCn && hit.name !== hit.nameCn && (
                      <span className="muted"> · {hit.name}</span>
                    )}
                    <span className="badge badge-bound">#{hit.id}</span>
                    {hit.score !== null && <span className="muted"> · {hit.score.toFixed(1)}</span>}
                  </div>
                  <div className="row-actions">
                    {previewLoadingId === hit.id && <span className="muted">读取中…</span>}
                    <button
                      type="button"
                      className="btn btn-tiny"
                      onClick={(event) => {
                        event.stopPropagation();
                        void handleBindCandidate(hit.id);
                      }}
                    >
                      绑定
                    </button>
                  </div>
                </div>
              ))}
            </div>

            <aside className="subject-preview">
              {previewLoadingId !== null ? (
                <div className="placeholder small">正在读取条目概览…</div>
              ) : previewError ? (
                <div className="preview-error">
                  <p className="muted">{previewError}</p>
                  {previewTarget !== null && (
                    <button
                      type="button"
                      className="btn btn-tiny"
                      onClick={() =>
                        void handleOpenSubjectPage(`https://bgm.tv/subject/${previewTarget}`)
                      }
                    >
                      在浏览器打开
                    </button>
                  )}
                </div>
              ) : !preview ? (
                <div className="placeholder small">点左侧任意结果查看概览</div>
              ) : (
                <>
                  <div className="preview-head">
                    {preview.cover ? (
                      <img className="preview-cover" src={preview.cover} alt={preview.name} />
                    ) : (
                      <div className="preview-cover preview-cover-empty">无封面</div>
                    )}
                    <div className="preview-meta">
                      <strong>{preview.nameCn || preview.name}</strong>
                      {preview.nameCn !== '' && preview.name !== preview.nameCn && (
                        <span className="muted">{preview.name}</span>
                      )}
                      <div className="preview-facts muted">
                        {SUBJECT_TYPE_LABEL[preview.subjectType] ?? '其他'}
                        {preview.eps !== null ? ` · ${preview.eps} 话` : ''}
                        {preview.date ? ` · ${preview.date}` : ''}
                      </div>
                      <div className="preview-rating">
                        {preview.score !== null ? `★ ${preview.score.toFixed(1)}` : '暂无评分'}
                        {preview.rank !== null ? ` · Rank #${preview.rank}` : ''}
                        {preview.ratingTotal > 0 ? ` · ${preview.ratingTotal} 人` : ''}
                      </div>
                    </div>
                  </div>

                  {preview.tags.length > 0 && (
                    <div className="preview-tags">
                      {preview.tags.map((tag) => (
                        <span key={tag.name} className="tag-chip">
                          {tag.name}
                        </span>
                      ))}
                    </div>
                  )}

                  {preview.summary !== '' && <p className="preview-summary">{preview.summary}</p>}

                  {preview.related.length > 0 && (
                    <div className="preview-related">
                      <h4>关联条目</h4>
                      {preview.related.slice(0, 10).map((row) => (
                        <button
                          key={row.id}
                          type="button"
                          className="related-row"
                          onClick={() => void handlePreview(row.id)}
                        >
                          <span className="muted">{row.relation || '关联'}</span>
                          <span>{row.nameCn || row.name}</span>
                        </button>
                      ))}
                    </div>
                  )}

                  <div className="row-actions">
                    <button
                      type="button"
                      className="btn btn-tiny"
                      onClick={() => void handleOpenSubjectPage(preview.url)}
                    >
                      在浏览器打开
                    </button>
                    <button
                      type="button"
                      className="btn btn-tiny btn-primary"
                      onClick={() => void handleBindCandidate(preview.id)}
                    >
                      绑定此条目
                    </button>
                  </div>
                </>
              )}
            </aside>
          </div>
        )}

        <p className="muted">
          当前：{item.bangumiSubjectId === null ? '未绑定' : `#${item.bangumiSubjectId}`}
        </p>
      </section>

      <section className="card">
        <h2>收藏与评分</h2>
        {item.bangumiSubjectId === null ? (
          <p className="muted">先绑定 Bangumi 条目 ID，才能记录收藏状态与评分。</p>
        ) : (
          <>
            <div className="inline-form">
              <label className="field-inline">
                <span className="field-label">收藏状态</span>
                <select
                  className="input select"
                  value={collectionStatus}
                  onChange={(event) => setCollectionStatus(event.target.value)}
                >
                  <option value="">未设置</option>
                  {[1, 2, 3, 4, 5].map((value) => (
                    <option key={value} value={value}>
                      {collectionLabels[value]}
                    </option>
                  ))}
                </select>
              </label>
              <label className="field-inline">
                <span className="field-label">评分</span>
                <select
                  className="input select"
                  value={rating}
                  onChange={(event) => setRating(event.target.value)}
                >
                  <option value="">未评分</option>
                  {Array.from({ length: 11 }, (_, index) => index).map((value) => (
                    <option key={value} value={value}>
                      {value} 分
                    </option>
                  ))}
                </select>
              </label>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => void handleSaveCollection()}
                disabled={savingCollection}
              >
                {savingCollection ? '保存中…' : '保存'}
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => void handlePullCloud()}
                disabled={pullingCloud}
                title="从当前登录的 Bangumi 账号拉取这条目的收藏状态与评分"
              >
                {pullingCloud ? '拉取中…' : '从 Bangumi 拉取'}
              </button>
              <button
                type="button"
                className="btn"
                onClick={() => void handleCacheMetadata()}
                disabled={cachingMeta}
              >
                {cachingMeta ? '缓存中…' : '缓存元数据'}
              </button>
            </div>
            <p className="muted">
              当前：
              {item.collectionStatus === null
                ? '未设置收藏'
                : (collectionLabels[item.collectionStatus] ?? `#${item.collectionStatus}`)}
              {' · '}
              {item.rating === null ? '未评分' : `${item.rating} 分`}
              {item.collectionUpdatedAt ? ` · 上次同步 ${item.collectionUpdatedAt}` : ''}
            </p>
          </>
        )}
      </section>

      <section className="card">
        <h2>文件（{files.length}）</h2>
        {files.length === 0 ? (
          <p className="muted">目录内没有可识别的文件，可能已被移动或删除。</p>
        ) : (
          <div className="table-wrap">
            <table className="table">
              <thead>
                <tr>
                  <th>名称</th>
                  {!isGame && <th className="col-source">来源</th>}
                  <th className="col-size">大小</th>
                  <th className="col-episode">{isGame ? '可执行' : '集号'}</th>
                  {!isGame && <th className="col-progress">进度</th>}
                  {!isGame && <th className="col-actions">操作</th>}
                </tr>
              </thead>
              <tbody>
                {files.map((file, index) => {
                  const episode = file.episode ?? index + 1;
                  const row = progressByEpisode.get(episode);
                  const isEditing = editing?.episode === episode;
                  return (
                    <tr key={file.name}>
                      <td className="cell-title" title={file.name}>
                        {file.name.split('/').pop() ?? file.name}
                      </td>
                      {!isGame && (
                        <td className="muted cell-group" title={file.group ?? '主目录'}>
                          {file.group ?? '主目录'}
                        </td>
                      )}
                      <td className="muted">{formatBytes(file.sizeBytes)}</td>
                      <td className="muted">
                        {isGame
                          ? '可启动'
                          : file.episode === null
                            ? `第 ${episode} 集（按序号）`
                            : `第 ${episode} 集`}
                      </td>
                      {!isGame && (
                        <td className="muted">
                          {row
                            ? row.source === 'cloud' && row.watchedSeconds <= 1
                              ? '看过（云端）'
                              : formatDuration(row.watchedSeconds)
                            : '—'}
                        </td>
                      )}
                      {!isGame && (
                        <td className="col-actions">
                          <div className="row-actions">
                            {isEditing ? (
                              <>
                                <input
                                  className="input tiny"
                                  placeholder="12:34 或 754"
                                  value={editing.value}
                                  onChange={(event) =>
                                    setEditing({ episode, value: event.target.value })
                                  }
                                  onKeyDown={(event) => {
                                    if (event.key === 'Enter') {
                                      void handleSaveProgress(episode, editing.value);
                                    }
                                    if (event.key === 'Escape') setEditing(null);
                                  }}
                                />
                                <button
                                  type="button"
                                  className="btn btn-primary btn-tiny"
                                  onClick={() => void handleSaveProgress(episode, editing.value)}
                                >
                                  保存
                                </button>
                                <button
                                  type="button"
                                  className="btn btn-tiny"
                                  onClick={() => setEditing(null)}
                                >
                                  取消
                                </button>
                              </>
                            ) : (
                              <>
                                <button
                                  type="button"
                                  className="btn btn-tiny"
                                  onClick={() => void handlePlay(file.name)}
                                  disabled={busy}
                                  title={
                                    row && row.watchedSeconds > 0
                                      ? `从 ${formatDuration(row.watchedSeconds)} 继续播放`
                                      : '从头播放'
                                  }
                                >
                                  {row && row.watchedSeconds > 0 ? '继续' : '播放'}
                                </button>
                                <button
                                  type="button"
                                  className="btn btn-tiny"
                                  onClick={() =>
                                    setEditing({
                                      episode,
                                      value: row ? String(row.watchedSeconds) : '',
                                    })
                                  }
                                >
                                  校正
                                </button>
                                <button
                                  type="button"
                                  className="btn btn-tiny"
                                  onClick={() => void handleRevealFile(file.name)}
                                  title="在资源管理器中定位该文件"
                                >
                                  定位
                                </button>
                              </>
                            )}
                          </div>
                        </td>
                      )}
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}
      </section>

      <section className="card" hidden={pluginPanelCount === 0}>
        <h2>插件面板</h2>
        <div ref={pluginContainerRef} />
      </section>

      <section className="card">
        <h2>记录</h2>
        <dl className="meta-list">
          <div>
            <dt>条目 ID</dt>
            <dd className="mono">{item.id}</dd>
          </div>
          <div>
            <dt>创建时间</dt>
            <dd>{item.createdAt}</dd>
          </div>
          <div>
            <dt>更新时间</dt>
            <dd>{item.updatedAt}</dd>
          </div>
        </dl>
      </section>

      <section className="card">
        <h2>危险操作</h2>
        <p className="muted">
          删除会移除整个条目目录（含所有视频文件）并清除本地记录；默认移入系统回收站，永久删除不可恢复。
        </p>
        <button
          type="button"
          className="btn btn-danger"
          onClick={() => setDeleteOpen(true)}
          disabled={busy}
        >
          删除资源…
        </button>
      </section>

      {pickerOpen && (
        <ExePicker
          title="选择可执行文件"
          initialPath={item.folderPath}
          onPick={(path) => void handlePickedExe(path)}
          onClose={() => setPickerOpen(false)}
        />
      )}

      {deleteOpen && (
        <DeleteItemDialog
          itemId={item.id}
          title={item.title}
          folderPath={item.folderPath}
          onClose={() => setDeleteOpen(false)}
          onDeleted={() => handleDeleted()}
        />
      )}
    </div>
  );
}
