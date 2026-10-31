-- 0002_collection：为 Bangumi 同步补充收藏状态与评分（plan.md 5.1 未定义，M3 需要）。
-- 收藏状态沿用 Bangumi 的 type 取值：1 想看 / 2 看过 / 3 在看 / 4 搁置 / 5 抛弃。
-- 评分沿用 Bangumi 的 0-10 整数；NULL 表示未评分。

ALTER TABLE library_item ADD COLUMN collection_status INTEGER;

ALTER TABLE library_item ADD COLUMN rating INTEGER;

ALTER TABLE library_item ADD COLUMN collection_updated_at TEXT;
