import { useState } from 'react';

interface Props {
  title: string;
  items: string[];
  onDismiss?: () => void;
}

/**
 * 批量操作的问题汇总（自动匹配、批量刷新等）。
 *
 * 这些操作一次可能产出十几条原因，全量铺开会把页面顶掉；
 * 这里默认只显示前两条与总数，其余折叠，需要时再展开。
 */
export default function IssueBanner({ title, items, onDismiss }: Props) {
  const [expanded, setExpanded] = useState(false);
  if (items.length === 0) return null;

  const preview = items.slice(0, 2);
  const rest = items.length - preview.length;

  return (
    <div className="banner banner-warn">
      <div className="issue-head">
        <span>
          {title}（{items.length} 项）
        </span>
        <div className="row-actions">
          {items.length > preview.length && (
            <button
              type="button"
              className="btn btn-tiny"
              onClick={() => setExpanded((value) => !value)}
            >
              {expanded ? '收起' : `展开全部（${items.length}）`}
            </button>
          )}
          {onDismiss && (
            <button type="button" className="btn btn-tiny" onClick={onDismiss}>
              知道了
            </button>
          )}
        </div>
      </div>
      {expanded ? (
        <ul className="issue-list">
          {items.map((item, index) => (
            <li key={`${index}-${item.slice(0, 12)}`}>{item}</li>
          ))}
        </ul>
      ) : (
        <ul className="issue-list">
          {preview.map((item, index) => (
            <li key={`${index}-${item.slice(0, 12)}`}>{item}</li>
          ))}
          {rest > 0 && <li className="muted">还有 {rest} 项…</li>}
        </ul>
      )}
    </div>
  );
}
