import { useEffect, useState } from 'react';

import { getItem, listProgress, playEpisode, type MediaFile } from '../api';

interface Props {
  itemId: string;
  title: string;
  onClose: () => void;
  /** 播放成功后通知列表刷新「运行中 / 正在播放」状态。 */
  onPlayed?: () => void;
}

/**
 * 选集面板：列出条目内的视频文件与已看状态，点一行直接播放。
 *
 * 集号来自后端的解析结果（可能与文件名不完全一致），文件名只作为辅助信息展示，
 * 因此点击时传的是 `file.name`（后端用来定位文件），而不是集号。
 */
export default function EpisodePicker({ itemId, title, onClose, onPlayed }: Props) {
  const [files, setFiles] = useState<MediaFile[]>([]);
  const [watched, setWatched] = useState<Map<number, number>>(new Map());
  const [loading, setLoading] = useState(true);
  const [busyFile, setBusyFile] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    Promise.all([getItem(itemId), listProgress(itemId)])
      .then(([detail, rows]) => {
        if (cancelled) return;
        setFiles(detail.files);
        setWatched(
          new Map(
            rows
              .filter((row) => row.watchedSeconds > 0)
              .map((row) => [row.episode, row.watchedSeconds] as const),
          ),
        );
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [itemId]);

  async function handlePlay(file: MediaFile) {
    setBusyFile(file.name);
    setError(null);
    try {
      await playEpisode(itemId, file.name);
      onPlayed?.();
      onClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusyFile(null);
    }
  }

  return (
    <div className="modal-backdrop" role="presentation" onClick={onClose}>
      <div
        className="modal modal-wide"
        role="dialog"
        aria-label="选集"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="modal-head">
          <h2>选集 · {title}</h2>
          <button type="button" className="btn btn-ghost" onClick={onClose}>
            关闭
          </button>
        </header>

        {loading ? (
          <p className="muted modal-body">读取剧集…</p>
        ) : files.length === 0 ? (
          <p className="muted modal-body">这个条目下没有找到视频文件。</p>
        ) : (
          <ul className="episode-list">
            {files.map((file) => {
              const seen = file.episode !== null && watched.has(file.episode);
              return (
                <li key={file.name}>
                  <button
                    type="button"
                    className="episode-row"
                    onClick={() => void handlePlay(file)}
                    disabled={busyFile !== null}
                    title={file.name}
                  >
                    <span className="episode-no">
                      {file.episode === null ? '—' : `第 ${file.episode} 集`}
                    </span>
                    <span className="episode-name mono">{file.name}</span>
                    {seen ? <span className="badge badge-status">已看</span> : null}
                    {busyFile === file.name ? <span className="muted">启动中…</span> : null}
                  </button>
                </li>
              );
            })}
          </ul>
        )}

        {error && <div className="banner banner-error modal-body">{error}</div>}
      </div>
    </div>
  );
}
