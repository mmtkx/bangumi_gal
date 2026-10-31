-- 松散文件分组：条目可以只“拥有”某个目录下的一组文件。
--
-- 场景：动画根目录里直接放着一部番的 01/02/03，没有单独的子目录。
-- 这类条目用 folder_path 指向根目录，再用 file_filter 记录它属于哪一组；
-- file_filter 为 NULL 表示“整个目录都属于这个条目”（原有的目录型条目）。
ALTER TABLE library_item ADD COLUMN file_filter TEXT;

-- 原来一个目录只能有一个条目；现在允许同目录下按 file_filter 拆出多个。
-- SQLite 的唯一索引不把 NULL 视为相等，所以用 IFNULL 归一成空串。
DROP INDEX IF EXISTS idx_library_item_folder_path;

CREATE UNIQUE INDEX IF NOT EXISTS idx_library_item_scope
    ON library_item (folder_path, IFNULL(file_filter, ''));
