import { useEffect, useRef, useState } from 'react';

import { setHideNsfw } from '../api';

interface Props {
  /** true 表示当前隐藏成人内容（关闭状态）。 */
  hidden: boolean;
  onChange: (hidden: boolean) => void;
}

/**
 * 全局「显示 / 隐藏成人内容」圆钮。
 *
 * 打开（高亮）= 显示全部条目；关闭 = 隐藏 nsfw 条目。与设置页共用同一个设置键，
 * 所以从设置页改完再回来，这里的初始状态也是对的。
 */
export default function NsfwToggle({ hidden, onChange }: Props) {
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const timer = useRef<number | null>(null);

  useEffect(() => {
    return () => {
      if (timer.current !== null) window.clearTimeout(timer.current);
    };
  }, []);

  function flash(text: string) {
    setMessage(text);
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setMessage(null), 2600);
  }

  async function handleClick() {
    if (busy) return;
    const nextHidden = !hidden;
    setBusy(true);
    try {
      const settings = await setHideNsfw(nextHidden);
      onChange(settings.hideNsfw);
      flash(settings.hideNsfw ? '牡蛎牡蛎，H 是不行的！！' : 'H 是对的！');
    } catch (err) {
      // 这个气泡只承载两条固定提示，失败不在这里说话（避免变成第三个文案）。
      console.warn('切换成人内容显示失败', err);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="nsfw-toggle-wrap" onClick={(event) => event.stopPropagation()}>
      <button
        type="button"
        className={hidden ? 'nsfw-toggle' : 'nsfw-toggle is-on'}
        onClick={() => void handleClick()}
        disabled={busy}
        aria-pressed={!hidden}
        aria-label="切换成人内容显示"
      >
        H
      </button>
      {message && <span className="nsfw-toggle-tip">{message}</span>}
    </div>
  );
}
