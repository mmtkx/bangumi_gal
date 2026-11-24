-- 用户自定义分类：树形（parent_id 自引用），条目与分类多对多。
--
-- 归属按「条目身份」(folder_path + file_filter) 记录，而不是 library_item.id：
-- 扫描会 prune 并重建条目（换根目录、多季拆分、目录改名），id 会变、路径不会。
-- 键的格式固定为 `folder_path || '|' || IFNULL(file_filter, '')`，与 Rust 侧
-- `library_core::category::item_key` 保持一致。
CREATE TABLE IF NOT EXISTS category (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  parent_id TEXT REFERENCES category(id) ON DELETE CASCADE,
  sort_order INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_category_parent ON category (parent_id, sort_order);

-- 同一父级下不允许重名。根分类的 parent_id 是 NULL，而 UNIQUE 索引对 NULL 不生效，
-- 因此用 IFNULL 建成表达式唯一索引。
CREATE UNIQUE INDEX IF NOT EXISTS idx_category_sibling_name
  ON category (IFNULL(parent_id, ''), name);

CREATE TABLE IF NOT EXISTS item_category (
  category_id TEXT NOT NULL REFERENCES category(id) ON DELETE CASCADE,
  item_key TEXT NOT NULL,
  assigned_at TEXT NOT NULL,
  PRIMARY KEY (category_id, item_key)
);

CREATE INDEX IF NOT EXISTS idx_item_category_item ON item_category (item_key);
