import { useEffect, useState } from 'react';

import { listSubdirs, type DirListing } from '../api';

interface Props {
  title: string;
  initialPath: string | null;
  onPick: (path: string) => void;
  onClose: () => void;
}

/** 目录浏览弹窗。前端不直接读文件系统，全部经 `list_subdirs` 命令。 */
export default function DirectoryPicker({ title, initialPath, onPick, onClose }: Props) {
  const [listing, setListing] = useState<DirListing | null>(null);
  const [pathInput, setPathInput] = useState(initialPath ?? '');
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  function load(path: string | null) {
    setLoading(true);
    listSubdirs(path)
      .then((data) => {
        setListing(data);
        setPathInput(data.path ?? '');
        setError(null);
      })
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => setLoading(false));
  }

  useEffect(() => {
    setLoading(true);
    listSubdirs(initialPath && initialPath.trim() !== '' ? initialPath : null)
      .then((data) => {
        setListing(data);
        setPathInput(data.path ?? '');
        setError(null);
      })
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
        // 初始路径不可用时退回驱动器列表，避免弹窗卡死在错误态。
        listSubdirs(null)
          .then((data) => {
            setListing(data);
            setPathInput('');
          })
          .catch(() => undefined);
      })
      .finally(() => setLoading(false));
  }, [initialPath]);

  function go(path: string) {
    const trimmed = path.trim();
    if (trimmed === '') return;
    load(trimmed);
  }

  function goComputer() {
    load(null);
  }

  return (
    <div className="modal-backdrop" role="presentation" onClick={onClose}>
      <div
        className="modal"
        role="dialog"
        aria-label={title}
        onClick={(event) => event.stopPropagation()}
      >
        <header className="modal-head">
          <h2>{title}</h2>
          <button type="button" className="btn btn-ghost" onClick={onClose}>
            关闭
          </button>
        </header>

        <div className="modal-path">
          <div className="inline-form">
            <input
              className="input mono"
              placeholder="输入或粘贴目录路径，例如 D:\\Plugins\\demo-panel"
              value={pathInput}
              onChange={(event) => setPathInput(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter') go(pathInput);
              }}
            />
            <button
              type="button"
              className="btn"
              onClick={() => go(pathInput)}
              disabled={pathInput.trim() === '' || loading}
            >
              前往
            </button>
            <button type="button" className="btn" onClick={goComputer} disabled={loading}>
              此电脑
            </button>
          </div>
          <div className="muted picker-current">当前：{listing?.path ?? '此电脑'}</div>
        </div>

        {error && <div className="banner banner-error">{error}</div>}

        <div className="dir-list">
          {loading ? (
            <div className="placeholder small">读取中…</div>
          ) : listing && listing.entries.length > 0 ? (
            listing.entries.map((entry) => (
              <button
                key={entry.path}
                type="button"
                className="dir-item"
                onDoubleClick={() => go(entry.path)}
                onClick={() => go(entry.path)}
              >
                <span className="dir-icon">▸</span>
                {entry.name}
              </button>
            ))
          ) : (
            <div className="placeholder small">没有子目录，可直接选择当前目录。</div>
          )}
        </div>

        <footer className="modal-foot">
          <button
            type="button"
            className="btn"
            disabled={!listing?.parent}
            onClick={() => listing?.parent && go(listing.parent)}
          >
            上一级
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={!listing?.path}
            onClick={() => listing?.path && onPick(listing.path)}
          >
            选择当前目录
          </button>
        </footer>
      </div>
    </div>
  );
}
