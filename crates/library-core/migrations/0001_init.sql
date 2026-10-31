-- 0001_init：plan.md 5.1 全部表。
-- 迁移只在 user_version 落后时执行，脚本本身仍需保持幂等，便于手工重放与自愈。

CREATE TABLE IF NOT EXISTS library_item (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('anime', 'game')),
    title TEXT NOT NULL,
    bangumi_subject_id INTEGER,
    folder_path TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_library_item_folder_path
    ON library_item (folder_path);

CREATE INDEX IF NOT EXISTS idx_library_item_kind
    ON library_item (kind);

CREATE INDEX IF NOT EXISTS idx_library_item_bangumi
    ON library_item (bangumi_subject_id);

CREATE TABLE IF NOT EXISTS progress (
    item_id TEXT NOT NULL REFERENCES library_item (id) ON DELETE CASCADE,
    episode INTEGER NOT NULL,
    watched_seconds INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL CHECK (source IN ('local', 'cloud')),
    updated_at TEXT NOT NULL,
    PRIMARY KEY (item_id, episode, source)
);

CREATE TABLE IF NOT EXISTS game_binding_index (
    item_id TEXT PRIMARY KEY REFERENCES library_item (id) ON DELETE CASCADE,
    exe_path TEXT NOT NULL,
    last_launched_at TEXT,
    last_pid INTEGER
);

CREATE TABLE IF NOT EXISTS sync_queue (
    id TEXT PRIMARY KEY,
    item_id TEXT REFERENCES library_item (id) ON DELETE CASCADE,
    op TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('pending', 'sending', 'failed', 'done')),
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_sync_queue_status
    ON sync_queue (status, next_retry_at);

CREATE TABLE IF NOT EXISTS conflict (
    id TEXT PRIMARY KEY,
    item_id TEXT REFERENCES library_item (id) ON DELETE CASCADE,
    field TEXT NOT NULL,
    local_value TEXT,
    cloud_value TEXT,
    local_ts TEXT,
    cloud_ts TEXT,
    resolved INTEGER NOT NULL DEFAULT 0 CHECK (resolved IN (0, 1))
);

CREATE INDEX IF NOT EXISTS idx_conflict_resolved
    ON conflict (resolved);

CREATE TABLE IF NOT EXISTS metadata_index (
    subject_id INTEGER PRIMARY KEY,
    cached_path TEXT NOT NULL,
    fetched_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS plugin_registry (
    id TEXT PRIMARY KEY,
    version TEXT NOT NULL,
    kind TEXT NOT NULL,
    manifest_path TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    installed_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
