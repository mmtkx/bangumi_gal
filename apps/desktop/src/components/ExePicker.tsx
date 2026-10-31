import { useEffect, useState } from 'react';

import { listExeFiles, listSubdirs, type DirListing, type FileEntry } from '../api';

interface Props {
  title: string;
  initialPath: string | null;
  onPick: (path: string) => void;
  onClose: () => void;
}

/** 可执行文件选择器：目录浏览 + 当前目录内 .exe 列表，前端不直接读文件系统。 */
export default function ExePicker({ title, initialPath, onPick, onClose }: Props) {
  const [listing, setListing] = useState<DirListing | null>(null);
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;

    async function load(path: string | null) {
      setLoading(true);
      try {
        const dir = await listSubdirs(path);
        if (cancelled) return;
        setListing(dir);
        setFiles(dir.path ? await listExeFiles(dir.path) : []);
        setError(null);
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
        // 初始路径不可用时退回驱动器列表，避免弹窗卡在错误态。
        try {
          const drives = await listSubdirs(null);
          if (!cancelled) {
            setListing(drives);
            setFiles([]);
          }
        } catch {
          // 列举驱动器也失败时保持错误提示即可。
        }
      } finally {
        if (!cancelled) setLoading(false);
      }
    }

    void load(initialPath && initialPath.trim() !== '' ? initialPath : null);
    return () => {
      cancelled = true;
    };
  }, [initialPath]);

  function go(path: string) {
    setLoading(true);
    listSubdirs(path)
      .then(async (dir) => {
        setListing(dir);
        setFiles(dir.path ? await listExeFiles(dir.path) : []);
        setError(null);
      })
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)))
      .finally(() => setLoading(false));
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

        <div className="modal-path mono">{listing?.path ?? '此电脑'}</div>
        {error && <div className="banner banner-error">{error}</div>}

        <div className="dir-list">
          {loading ? (
            <div className="placeholder small">读取中…</div>
          ) : (
            <>
              {listing?.entries.map((entry) => (
                <button
                  key={entry.path}
                  type="button"
                  className="dir-item"
                  onClick={() => go(entry.path)}
                >
                  <span className="dir-icon">▸</span>
                  {entry.name}
                </button>
              ))}
              {files.map((file) => (
                <button
                  key={file.path}
                  type="button"
                  className="dir-item file-item"
                  onClick={() => onPick(file.path)}
                >
                  <span className="dir-icon exe">EXE</span>
                  {file.name}
                </button>
              ))}
              {listing && listing.entries.length === 0 && files.length === 0 && (
                <div className="placeholder small">此目录没有子目录或 .exe 文件。</div>
              )}
            </>
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
        </footer>
      </div>
    </div>
  );
}
