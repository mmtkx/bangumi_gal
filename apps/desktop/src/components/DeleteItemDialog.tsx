import { useState } from 'react';

import { deleteItem, type DeleteReport } from '../api';

interface Props {
  itemId: string;
  title: string;
  folderPath: string;
  onClose: () => void;
  onDeleted: (report: DeleteReport) => void;
}

type Mode = 'trash' | 'permanent';

/**
 * 删除资源的二次确认（plan.md 6.5 / ADR-005）：
 * 默认移入回收站；永久删除必须手工输入条目标题，避免误触。
 */
export default function DeleteItemDialog({ itemId, title, folderPath, onClose, onDeleted }: Props) {
  const [mode, setMode] = useState<Mode>('trash');
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canConfirm = mode === 'trash' || confirm.trim() === title.trim();

  async function handleConfirm() {
    setBusy(true);
    setError(null);
    try {
      const report = await deleteItem(
        itemId,
        mode,
        mode === 'permanent' ? confirm.trim() : undefined,
      );
      onDeleted(report);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="modal-backdrop" role="presentation" onClick={onClose}>
      <div
        className="modal"
        role="dialog"
        aria-label="删除资源"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="modal-head">
          <h2>删除资源</h2>
          <button type="button" className="btn btn-ghost" onClick={onClose} disabled={busy}>
            关闭
          </button>
        </header>

        <p className="muted">
          将删除条目「{title}
          」的整个目录（含其中所有视频文件）并清除本地记录（进度、绑定、同步队列、冲突）。
        </p>
        <div className="modal-path mono">{folderPath}</div>
        {error && <div className="banner banner-error">{error}</div>}

        <div className="delete-modes">
          <label className="delete-mode">
            <input
              type="radio"
              name="delete-mode"
              checked={mode === 'trash'}
              onChange={() => setMode('trash')}
              disabled={busy}
            />
            <span>
              移入回收站（默认）
              <span className="muted"> · 可在系统回收站中还原</span>
            </span>
          </label>
          <label className="delete-mode">
            <input
              type="radio"
              name="delete-mode"
              checked={mode === 'permanent'}
              onChange={() => setMode('permanent')}
              disabled={busy}
            />
            <span>
              永久删除
              <span className="muted"> · 不可恢复</span>
            </span>
          </label>
        </div>

        {mode === 'permanent' && (
          <label className="field">
            <span className="field-label">请输入条目标题「{title}」确认永久删除</span>
            <input
              className="input"
              value={confirm}
              onChange={(event) => setConfirm(event.target.value)}
              disabled={busy}
              spellCheck={false}
              autoFocus
            />
          </label>
        )}

        <footer className="modal-foot">
          <button type="button" className="btn" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button
            type="button"
            className="btn btn-danger"
            onClick={() => void handleConfirm()}
            disabled={busy || !canConfirm}
          >
            {busy ? '删除中…' : mode === 'permanent' ? '永久删除' : '移入回收站'}
          </button>
        </footer>
      </div>
    </div>
  );
}
