import { useState } from 'react';

import { clearItems, type ItemKind } from '../api';

interface Props {
  /** 只清空当前分类；传 null 表示两类都清。 */
  kind: ItemKind | null;
  onClose: () => void;
  onCleared: (removed: number) => void;
}

/**
 * 清空条目记录的二次确认。
 *
 * 这个操作只删数据库里的条目（进度、绑定、同步队列、冲突一并级联清理），
 * 磁盘上的视频与游戏文件一概不动；因此确认文案必须把这一点写清楚，
 * 并且要求手输「清空」避免误触。
 */
export default function ClearItemsDialog({ kind, onClose, onCleared }: Props) {
  const [confirm, setConfirm] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const canConfirm = confirm.trim() === '清空';
  const label = kind === 'anime' ? '动画' : kind === 'game' ? '游戏' : '全部';

  async function handleConfirm() {
    setBusy(true);
    setError(null);
    try {
      onCleared(await clearItems(kind));
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
        aria-label="清空条目"
        onClick={(event) => event.stopPropagation()}
      >
        <header className="modal-head">
          <h2>清空{label}条目</h2>
          <button type="button" className="btn btn-ghost" onClick={onClose} disabled={busy}>
            关闭
          </button>
        </header>

        <p className="muted">
          将清除软件里的{label}条目记录：收藏状态、评分、观看进度、Bangumi 绑定、同步队列与冲突。
        </p>
        <p className="muted">
          <strong>磁盘上的视频与游戏文件不会被删除、移动或修改。</strong>
          清空后重新扫描根目录即可恢复条目，但手动绑定与进度需要重做。
        </p>
        {error && <div className="banner banner-error">{error}</div>}

        <label className="field">
          <span className="field-label">请输入「清空」确认</span>
          <input
            className="input"
            value={confirm}
            onChange={(event) => setConfirm(event.target.value)}
            disabled={busy}
            spellCheck={false}
            autoFocus
          />
        </label>

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
            {busy ? '清空中…' : '清空条目'}
          </button>
        </footer>
      </div>
    </div>
  );
}
