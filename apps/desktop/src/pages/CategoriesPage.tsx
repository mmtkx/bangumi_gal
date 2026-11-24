import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import type { PointerEvent as ReactPointerEvent } from 'react';

import {
  assignItemsToCategory,
  categoryCoverImages,
  categoryCoverPreview,
  createCategory,
  deleteCategory,
  getSettings,
  itemCovers,
  listCategories,
  listCategoryItems,
  listItems,
  moveCategory,
  renameCategory,
  setCategoryCover,
  setCategoryCoverImage,
  swapCategoryOrder,
} from '../api';
import type { Category, ItemKind, ItemSummary } from '../api';
import NsfwToggle from '../components/NsfwToggle';

const KIND_LABEL: Record<ItemKind, string> = { anime: '动画', game: '游戏' };

type KindFilter = 'all' | ItemKind;

type DragPayload = { kind: 'items'; ids: string[] } | { kind: 'category'; id: string };

interface Props {
  onOpen: (id: string) => void;
}

function describeMedia(item: ItemSummary): string {
  return item.media.episodeHint ?? `${item.media.fileCount} 个文件`;
}

function sortSiblings(rows: Category[]): Category[] {
  return [...rows].sort(
    (left, right) => left.sortOrder - right.sortOrder || left.name.localeCompare(right.name),
  );
}

// 大图一次性展开成数组会撑爆调用栈，分块拼接。
function toBase64(buffer: ArrayBuffer): string {
  const bytes = new Uint8Array(buffer);
  let binary = '';
  const chunk = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunk) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunk));
  }
  return window.btoa(binary);
}

/** 分类浏览页：分类节点就是封面卡片，逐层下钻；条目卡拖到卡片上完成归类。 */
export default function CategoriesPage({ onOpen }: Props) {
  const [categories, setCategories] = useState<Category[]>([]);
  const [items, setItems] = useState<ItemSummary[]>([]);
  // 从根到当前层的分类 id，最后一个是当前分类（空表示根层）。
  const [path, setPath] = useState<string[]>([]);
  const [levelItems, setLevelItems] = useState<ItemSummary[]>([]);
  const [covers, setCovers] = useState<Record<string, string>>({});
  const [categoryCovers, setCategoryCovers] = useState<Record<string, string>>({});
  const [carousel, setCarousel] = useState<Record<string, string[]>>({});
  const [tick, setTick] = useState(0);

  const [drawerOpen, setDrawerOpen] = useState(false);
  const [drawerWidth, setDrawerWidth] = useState(() => {
    const saved = Number(localStorage.getItem('lagm.catDrawerWidth'));
    return Number.isFinite(saved) && saved >= 240 ? saved : 320;
  });
  const [drawerKind, setDrawerKind] = useState<KindFilter>('all');
  const [drawerKeyword, setDrawerKeyword] = useState('');
  const [drawerSelected, setDrawerSelected] = useState<string[]>([]);

  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const [coverMenuFor, setCoverMenuFor] = useState<string | null>(null);
  const [pendingCoverId, setPendingCoverId] = useState<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [creating, setCreating] = useState(false);
  const [draftName, setDraftName] = useState('');
  const [renaming, setRenaming] = useState<{ id: string; name: string } | null>(null);
  const [dropTarget, setDropTarget] = useState<string | null>(null);
  const [dragPreview, setDragPreview] = useState<{
    label: string;
    kind: DragPayload['kind'];
    x: number;
    y: number;
  } | null>(null);
  const suppressClick = useRef(false);
  const [hideNsfw, setHideNsfw] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const currentId = path.length > 0 ? path[path.length - 1] : null;
  const current = categories.find((row) => row.id === currentId) ?? null;

  const reload = useCallback(async () => {
    const [categoryRows, itemRows] = await Promise.all([listCategories(), listItems(null)]);
    setCategories(categoryRows);
    setItems(itemRows);
  }, []);

  const reloadCovers = useCallback(async () => {
    const [images, preview] = await Promise.all([categoryCoverImages(), categoryCoverPreview(5)]);
    setCategoryCovers(images);
    setCarousel(preview);
  }, []);

  useEffect(() => {
    Promise.all([reload(), reloadCovers(), getSettings()])
      .then(([, , settings]) => setHideNsfw(settings.hideNsfw))
      .catch((err: unknown) => setError(err instanceof Error ? err.message : String(err)));
  }, [reload, reloadCovers]);

  useEffect(() => {
    if (!currentId) {
      setLevelItems([]);
      return;
    }
    let cancelled = false;
    listCategoryItems(currentId)
      .then((rows) => {
        if (!cancelled) setLevelItems(rows);
      })
      .catch((err: unknown) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      });
    return () => {
      cancelled = true;
    };
  }, [currentId]);

  // 开关变化后重新取轮播数据：后端是按当前的隐藏设置筛的。
  useEffect(() => {
    reloadCovers().catch((err: unknown) => console.warn('刷新分类封面失败', err));
  }, [hideNsfw, reloadCovers]);

  // 轮播每 4 秒换一张；没有轮播卡时也只是空转一次 setState。
  useEffect(() => {
    const timer = window.setInterval(() => setTick((value) => value + 1), 4000);
    return () => window.clearInterval(timer);
  }, []);

  const childrenOf = useMemo(() => {
    const map = new Map<string | null, Category[]>();
    for (const category of categories) {
      const bucket = map.get(category.parentId) ?? [];
      bucket.push(category);
      map.set(category.parentId, bucket);
    }
    for (const [key, bucket] of map) map.set(key, sortSiblings(bucket));
    return map;
  }, [categories]);

  const levelChildren = childrenOf.get(currentId) ?? [];

  // 层内条目同样受全局开关约束：之前只过滤了抽屉，已归类的成人条目会漏出来。
  const visibleLevelItems = useMemo(
    () => (hideNsfw ? levelItems.filter((item) => item.nsfw !== 'adult') : levelItems),
    [levelItems, hideNsfw],
  );

  // 抽屉候选：当前分类未直接收录的条目（根层时列出全部）。
  const candidates = useMemo(() => {
    const memberSet = new Set(levelItems.map((item) => item.id));
    const lowered = drawerKeyword.trim().toLowerCase();
    // nsfw 的显示与否只由右上角圆钮（全局设置）决定，不再保留搜索暗门。
    return items.filter((item) => {
      if (currentId && memberSet.has(item.id)) return false;
      if (hideNsfw && item.nsfw === 'adult') return false;
      if (lowered !== '') {
        const hit =
          item.title.toLowerCase().includes(lowered) ||
          item.folderPath.toLowerCase().includes(lowered);
        if (!hit) return false;
      }
      if (drawerKind !== 'all' && item.kind !== drawerKind) return false;
      return true;
    });
  }, [items, levelItems, currentId, hideNsfw, drawerKind, drawerKeyword]);

  // 要取图的条目：层内 + 抽屉候选（抽屉打开时）+ 轮播来源，去重后一次请求。
  const coverIdsKey = useMemo(() => {
    const ids = new Set<string>();
    for (const item of levelItems) ids.add(item.id);
    if (drawerOpen) for (const item of candidates) ids.add(item.id);
    for (const list of Object.values(carousel)) for (const id of list) ids.add(id);
    return [...ids].sort().join(',');
  }, [levelItems, candidates, carousel, drawerOpen]);

  useEffect(() => {
    if (coverIdsKey === '') return;
    let cancelled = false;
    itemCovers(coverIdsKey.split(','))
      .then((map) => {
        if (!cancelled) setCovers((prev) => ({ ...prev, ...map }));
      })
      .catch((err: unknown) => console.warn('读取封面失败', err));
    return () => {
      cancelled = true;
    };
  }, [coverIdsKey]);

  function flash(message: string) {
    setNotice(message);
    setError(null);
  }

  async function run(work: () => Promise<void>) {
    setBusy(true);
    try {
      await work();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setNotice(null);
    } finally {
      setBusy(false);
    }
  }

  async function applyCategories(next: Promise<Category[]>) {
    setCategories(await next);
    await reloadCovers();
  }

  function enter(category: Category) {
    if (suppressClick.current) return;
    setPath((prev) => [...prev, category.id]);
    setDrawerSelected([]);
    setDrawerKeyword('');
    setAddMenuOpen(false);
    setCoverMenuFor(null);
  }

  function goTo(depth: number) {
    setPath((prev) => prev.slice(0, depth));
    setDrawerSelected([]);
    setAddMenuOpen(false);
    setCoverMenuFor(null);
  }

  async function submitCreate() {
    const name = draftName.trim();
    if (name === '') {
      setCreating(false);
      return;
    }
    await run(async () => {
      await applyCategories(createCategory(name, currentId));
      setCreating(false);
      setDraftName('');
      flash(`已新建「${name}」`);
    });
  }

  async function submitRename() {
    if (!renaming) return;
    const value = renaming.name.trim();
    const target = renaming.id;
    setRenaming(null);
    if (value === '') return;
    await run(async () => {
      await applyCategories(renameCategory(target, value));
      flash(`已改名为「${value}」`);
    });
  }

  async function handleDelete(category: Category) {
    const childCount = categories.filter((row) => row.parentId === category.id).length;
    const detail = childCount > 0 ? `连同 ${childCount} 个子分类一起删除，` : '';
    if (!window.confirm(`删除分类「${category.name}」？${detail}条目与磁盘文件不受影响。`)) return;
    await run(async () => {
      await applyCategories(deleteCategory(category.id));
      if (path.includes(category.id)) {
        setPath(path.slice(0, path.indexOf(category.id)));
      }
      flash(`已删除「${category.name}」`);
    });
  }

  async function handleSwap(category: Category, delta: number) {
    await run(async () => {
      setCategories(await swapCategoryOrder(category.id, delta));
    });
  }

  async function assignTo(categoryId: string, itemIds: string[]) {
    if (itemIds.length === 0) return;
    await run(async () => {
      await applyCategories(assignItemsToCategory(categoryId, itemIds));
      if (categoryId === currentId) setLevelItems(await listCategoryItems(categoryId));
      setDrawerSelected([]);
      const name = categories.find((row) => row.id === categoryId)?.name ?? '分类';
      flash(`已把 ${itemIds.length} 个条目加入「${name}」`);
    });
  }

  async function reparent(draggedId: string, parentId: string | null) {
    if (draggedId === parentId) return;
    await run(async () => {
      await applyCategories(moveCategory(draggedId, parentId, null));
      flash('已调整分类层级');
    });
  }

  async function chooseCover(category: Category, kind: 'title' | 'carousel') {
    setCoverMenuFor(null);
    await run(async () => {
      await applyCategories(setCategoryCover(category.id, kind));
      flash(kind === 'title' ? '封面已改回名称' : '封面改为轮播条目');
    });
  }

  function pickCoverFile(category: Category) {
    setCoverMenuFor(null);
    setPendingCoverId(category.id);
    fileInputRef.current?.click();
  }

  async function handleCoverFile(event: React.ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    // 清空 value，否则同一个文件再选一次不会触发 change。
    event.target.value = '';
    const targetId = pendingCoverId;
    setPendingCoverId(null);
    if (!file || !targetId) return;
    const data = toBase64(await file.arrayBuffer());
    await run(async () => {
      await applyCategories(setCategoryCoverImage(targetId, file.name, data));
      flash('封面已更新');
    });
  }

  // 指针拖拽：不用 HTML5 DnD——WebView2 上它会被系统级文件拖放截走，
  // 表现成「拖条目时弹出导入遮罩」且 drop 收不到。这里用 pointer + elementFromPoint 命中。
  function beginDrag(event: ReactPointerEvent, payload: DragPayload) {
    if (event.button !== 0) return;
    // 按钮、输入框上的按下不当作拖拽起点（否则拖选文字会把分类搬走）。
    const origin = event.target;
    if (origin instanceof HTMLElement && origin.closest('input, button, select, textarea')) return;
    const startX = event.clientX;
    const startY = event.clientY;
    let active = false;
    let hovered: string | null = null;
    const label = payload.kind === 'category' ? '移动分类' : `移动 ${payload.ids.length} 个条目`;

    const onMove = (move: PointerEvent) => {
      if (!active && Math.hypot(move.clientX - startX, move.clientY - startY) < 5) return;
      if (!active) {
        active = true;
        suppressClick.current = true;
        document.body.classList.add('is-cat-dragging');
      }
      setDragPreview({ label, kind: payload.kind, x: move.clientX, y: move.clientY });
      const element = document.elementFromPoint(move.clientX, move.clientY);
      const target =
        element instanceof HTMLElement ? element.closest<HTMLElement>('[data-drop]') : null;
      hovered = target?.dataset.drop ?? null;
      setDropTarget(hovered);
    };

    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      document.body.classList.remove('is-cat-dragging');
      setDragPreview(null);
      setDropTarget(null);
      if (!active || hovered === null) {
        suppressClick.current = false;
        return;
      }
      if (payload.kind === 'items') {
        if (hovered !== 'root') void assignTo(hovered, payload.ids);
      } else {
        void reparent(payload.id, hovered === 'root' ? null : hovered);
      }
      // click 在 pointerup 之后立刻触发，稍后再放开拦截。
      window.setTimeout(() => {
        suppressClick.current = false;
      }, 0);
    };

    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function beginDrawerResize(event: ReactPointerEvent) {
    event.preventDefault();
    const startX = event.clientX;
    const startWidth = drawerWidth;
    const onMove = (move: PointerEvent) => {
      const next = Math.min(720, Math.max(240, startWidth - (move.clientX - startX)));
      setDrawerWidth(next);
    };
    const onUp = () => {
      window.removeEventListener('pointermove', onMove);
      window.removeEventListener('pointerup', onUp);
      setDrawerWidth((value) => {
        localStorage.setItem('lagm.catDrawerWidth', String(value));
        return value;
      });
    };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  }

  function categoryCoverContent(category: Category, index: number) {
    if (category.coverKind === 'image' && categoryCovers[category.id]) {
      return <img src={categoryCovers[category.id]} alt={`${category.name} 封面`} />;
    }
    if (category.coverKind === 'carousel') {
      const ids = carousel[category.id] ?? [];
      const id = ids.length > 0 ? ids[(tick + index) % ids.length] : null;
      if (id && covers[id]) {
        return <img src={covers[id]} alt={`${category.name} 封面`} />;
      }
    }
    return <div className="cat-cover-name">{category.name}</div>;
  }

  return (
    <div
      className="page categories-page"
      onClick={() => {
        setAddMenuOpen(false);
        setCoverMenuFor(null);
      }}
    >
      <input
        ref={fileInputRef}
        type="file"
        accept="image/png,image/jpeg,image/webp,image/bmp,image/gif"
        className="cat-file-input"
        onChange={(event) => void handleCoverFile(event)}
      />

      <header className="page-head">
        <div>
          <h1>分类</h1>
          <p className="muted">
            分类卡片可逐层下钻；把条目拖到卡片上即可归类。最多 10000 个分类、最多 5 层。
          </p>
        </div>
        <div className="page-head-actions">
          <button
            type="button"
            className="btn"
            onClick={(event) => {
              event.stopPropagation();
              setCreating(true);
            }}
          >
            ＋ 新建分类
          </button>
          <button
            type="button"
            className="btn"
            onClick={(event) => {
              event.stopPropagation();
              setDrawerOpen((prev) => !prev);
            }}
          >
            {drawerOpen ? '收起待归类' : '待归类条目'}
          </button>
          <NsfwToggle hidden={hideNsfw} onChange={setHideNsfw} />
        </div>
      </header>

      {(error || notice) && (
        <div className={error ? 'banner banner-error' : 'banner banner-ok'}>
          <span>{error ?? notice}</span>
          <button
            type="button"
            className="btn btn-ghost btn-tiny"
            onClick={() => {
              setError(null);
              setNotice(null);
            }}
          >
            知道了
          </button>
        </div>
      )}

      <nav className="cat-crumbs" aria-label="分类路径">
        <button
          type="button"
          className={
            dropTarget === 'root' ? 'cat-crumb cat-crumb-root is-drop' : 'cat-crumb cat-crumb-root'
          }
          data-drop="root"
          onClick={() => goTo(0)}
        >
          全部分类
        </button>
        {path.map((id, depth) => {
          const node = categories.find((row) => row.id === id);
          if (!node) return null;
          const isLast = depth === path.length - 1;
          return (
            <span key={id} className="cat-crumb-group">
              <span className="cat-crumb-sep">›</span>
              <button
                type="button"
                className={
                  dropTarget === id
                    ? 'cat-crumb is-current is-drop'
                    : isLast
                      ? 'cat-crumb is-current'
                      : 'cat-crumb'
                }
                data-drop={id}
                onClick={() => goTo(depth + 1)}
              >
                {node.name}
              </button>
            </span>
          );
        })}
      </nav>

      {creating && (
        <div className="cat-create" onClick={(event) => event.stopPropagation()}>
          <input
            className="input"
            placeholder={current ? `在「${current.name}」下新建子分类` : '分类名称'}
            value={draftName}
            autoFocus
            onChange={(event) => setDraftName(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter') void submitCreate();
              if (event.key === 'Escape') setCreating(false);
            }}
          />
          <button
            type="button"
            className="btn btn-primary btn-tiny"
            onClick={() => void submitCreate()}
          >
            创建
          </button>
          <button
            type="button"
            className="btn btn-ghost btn-tiny"
            onClick={() => setCreating(false)}
          >
            取消
          </button>
        </div>
      )}

      <div className="cat-body">
        <div className="cat-main" data-drop={currentId ?? 'root'}>
          {dragPreview && (dragPreview.kind === 'category' || current !== null) && (
            <div className="cat-dropzone" data-drop={currentId ?? 'root'}>
              {dragPreview.kind === 'category'
                ? '松开：移动到这一层'
                : `松开：加入「${current?.name ?? '当前分类'}」`}
            </div>
          )}
          <div className="cover-grid">
            {levelChildren.map((category, index) => {
              const classes = ['cover-card', 'cat-card'];
              if (dropTarget === category.id) classes.push('is-selected');
              return (
                <div
                  key={category.id}
                  className={classes.join(' ')}
                  data-drop={category.id}
                  onClick={() => enter(category)}
                  onPointerDown={(event) => beginDrag(event, { kind: 'category', id: category.id })}
                >
                  <div className="cover-frame">
                    {categoryCoverContent(category, index)}
                    {category.coverKind === 'carousel' && (
                      <span className="cat-dots">
                        {(carousel[category.id] ?? []).map((id, dot, list) => (
                          <span
                            key={id}
                            className={
                              dot === tick % Math.max(1, list.length) ? 'cat-dot is-on' : 'cat-dot'
                            }
                          />
                        ))}
                      </span>
                    )}
                    <div className="cover-hover" onClick={(event) => event.stopPropagation()}>
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => setCoverMenuFor(category.id)}
                      >
                        封面
                      </button>
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => {
                          setCreating(false);
                          setRenaming({ id: category.id, name: category.name });
                        }}
                      >
                        改名
                      </button>
                      <button
                        type="button"
                        className="btn btn-tiny"
                        onClick={() => void handleDelete(category)}
                      >
                        删除
                      </button>
                    </div>
                  </div>
                  {coverMenuFor === category.id && (
                    <div className="cat-menu" onClick={(event) => event.stopPropagation()}>
                      <button type="button" onClick={() => void chooseCover(category, 'title')}>
                        名称黑底
                      </button>
                      <button type="button" onClick={() => pickCoverFile(category)}>
                        本地图片…
                      </button>
                      <button type="button" onClick={() => void chooseCover(category, 'carousel')}>
                        轮播条目封面
                      </button>
                    </div>
                  )}
                  {renaming?.id === category.id ? (
                    <input
                      className="input cat-rename"
                      value={renaming.name}
                      autoFocus
                      onClick={(event) => event.stopPropagation()}
                      onChange={(event) =>
                        setRenaming({ id: category.id, name: event.target.value })
                      }
                      onKeyDown={(event) => {
                        if (event.key === 'Enter') void submitRename();
                        if (event.key === 'Escape') setRenaming(null);
                      }}
                      onBlur={() => void submitRename()}
                    />
                  ) : (
                    <div className="cover-title" title={category.name}>
                      {category.coverKind === 'title'
                        ? `共 ${category.itemCount} 条`
                        : category.name}
                    </div>
                  )}
                  <div className="cover-meta muted">
                    <span className="badge badge-folder">分类</span>
                    <span>{category.itemCount} 条</span>
                    <span className="cat-node-actions">
                      <button
                        type="button"
                        className="icon-btn"
                        title="上移"
                        onClick={(event) => {
                          event.stopPropagation();
                          void handleSwap(category, -1);
                        }}
                      >
                        ↑
                      </button>
                      <button
                        type="button"
                        className="icon-btn"
                        title="下移"
                        onClick={(event) => {
                          event.stopPropagation();
                          void handleSwap(category, 1);
                        }}
                      >
                        ↓
                      </button>
                    </span>
                  </div>
                </div>
              );
            })}

            {visibleLevelItems.map((item) => (
              <div
                key={item.id}
                className="cover-card"
                onClick={() => {
                  if (suppressClick.current) return;
                  onOpen(item.id);
                }}
                onPointerDown={(event) => beginDrag(event, { kind: 'items', ids: [item.id] })}
                title="拖到分类卡片上可移入该分类"
              >
                <div className="cover-frame">
                  {covers[item.id] ? (
                    <img src={covers[item.id]} alt={`${item.title} 封面`} />
                  ) : (
                    <div className="cover-empty">{item.title.slice(0, 2)}</div>
                  )}
                </div>
                <div className="cover-title" title={item.title}>
                  {item.title}
                </div>
                <div className="cover-meta muted">
                  <span className={`badge badge-${item.kind}`}>{KIND_LABEL[item.kind]}</span>
                  <span>{describeMedia(item)}</span>
                </div>
              </div>
            ))}

            <div
              className="cover-card cat-add"
              onClick={(event) => {
                event.stopPropagation();
                setAddMenuOpen((prev) => !prev);
              }}
            >
              <div className="cover-frame cat-add-frame">＋</div>
              <div className="cover-title">新增 / 添加</div>
              <div className="cover-meta muted">
                <span>{current ? `在「${current.name}」下` : '根层'}</span>
              </div>
              {addMenuOpen && (
                <div className="cat-menu cat-add-menu" onClick={(event) => event.stopPropagation()}>
                  <button
                    type="button"
                    className="cat-menu-item"
                    onClick={() => {
                      setAddMenuOpen(false);
                      setCreating(true);
                    }}
                  >
                    <span className="cat-menu-icon">＋</span>
                    <span className="cat-menu-text">
                      <strong>新建分类</strong>
                      <span className="muted">
                        {current ? `在「${current.name}」下建子分类` : '在根层建一个分类'}
                      </span>
                    </span>
                  </button>
                  <button
                    type="button"
                    className="cat-menu-item"
                    onClick={() => {
                      setAddMenuOpen(false);
                      setDrawerOpen(true);
                    }}
                  >
                    <span className="cat-menu-icon">▤</span>
                    <span className="cat-menu-text">
                      <strong>添加条目</strong>
                      <span className="muted">从待归类列表把条目拖进分类</span>
                    </span>
                  </button>
                </div>
              )}
            </div>
          </div>

          {levelChildren.length === 0 && visibleLevelItems.length === 0 && (
            <p className="muted cat-empty-hint">
              {current
                ? '这个分类还是空的：点「＋」新建子分类，或把条目拖进来。'
                : '还没有分类：点「＋」新建第一个分类。'}
            </p>
          )}
        </div>

        {drawerOpen && (
          <aside
            className="cat-drawer"
            style={{ width: `${drawerWidth}px` }}
            onClick={(event) => event.stopPropagation()}
          >
            <div className="cat-resize" onPointerDown={beginDrawerResize} title="拖动调整宽度" />
            <div className="cat-drawer-head">
              <strong>待归类条目</strong>
              <button
                type="button"
                className="icon-btn"
                title="收起"
                onClick={() => setDrawerOpen(false)}
              >
                ✕
              </button>
            </div>
            <div className="segmented">
              {(['all', 'anime', 'game'] as KindFilter[]).map((value) => (
                <button
                  key={value}
                  type="button"
                  className={drawerKind === value ? 'segment is-active' : 'segment'}
                  onClick={() => setDrawerKind(value)}
                >
                  {value === 'all' ? '全部' : KIND_LABEL[value]}
                </button>
              ))}
            </div>
            <input
              className="input"
              placeholder="搜索标题或路径"
              value={drawerKeyword}
              onChange={(event) => setDrawerKeyword(event.target.value)}
            />
            <p className="muted cat-drawer-hint">
              {current
                ? `拖到左侧分类卡片即可加入；或勾选后加入「${current.name}」`
                : '进入一个分类后，才能把勾选的条目直接加入'}
            </p>
            <div className="cat-drawer-list">
              {candidates.map((item) => {
                const picked = drawerSelected.includes(item.id);
                return (
                  <div
                    key={item.id}
                    className={picked ? 'cat-drawer-row is-picked' : 'cat-drawer-row'}
                    onPointerDown={(event) =>
                      beginDrag(event, {
                        kind: 'items',
                        ids: picked ? drawerSelected : [item.id],
                      })
                    }
                    onClick={() => {
                      if (suppressClick.current) return;
                      setDrawerSelected((prev) =>
                        prev.includes(item.id)
                          ? prev.filter((value) => value !== item.id)
                          : [...prev, item.id],
                      );
                    }}
                  >
                    <input
                      type="checkbox"
                      checked={picked}
                      readOnly
                      aria-label={`选择 ${item.title}`}
                    />
                    <div className="cat-drawer-thumb">
                      {covers[item.id] ? (
                        <img src={covers[item.id]} alt="" />
                      ) : (
                        <span>{item.title.slice(0, 1)}</span>
                      )}
                    </div>
                    <div className="cat-drawer-text">
                      <span className="cat-drawer-title" title={item.title}>
                        {item.title}
                      </span>
                      <span className="muted">
                        {KIND_LABEL[item.kind]} · {describeMedia(item)}
                      </span>
                    </div>
                  </div>
                );
              })}
              {candidates.length === 0 && <p className="muted">没有待归类的条目。</p>}
            </div>
            <div className="cat-drawer-foot">
              <span className="muted">已选 {drawerSelected.length} 个</span>
              <button
                type="button"
                className="btn btn-primary btn-tiny"
                disabled={busy || currentId === null || drawerSelected.length === 0}
                onClick={() => currentId && void assignTo(currentId, drawerSelected)}
              >
                加入当前分类
              </button>
            </div>
          </aside>
        )}
      </div>

      {dragPreview && (
        <div
          className="cat-drag-ghost"
          style={{ left: dragPreview.x + 14, top: dragPreview.y + 14 }}
        >
          {dragPreview.label}
        </div>
      )}
    </div>
  );
}
