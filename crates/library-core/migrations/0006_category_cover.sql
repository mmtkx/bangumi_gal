-- 分类卡封面：三种来源。
--
-- title    ：默认，黑底 + 分类名，不占磁盘。
-- image    ：用户本地选图，落库时复制到应用数据目录，只记副本路径。
-- carousel ：轮播该分类子树里的条目封面，前端按需取图，库里不存图。
ALTER TABLE category ADD COLUMN cover_kind TEXT NOT NULL DEFAULT 'title';

-- 仅 cover_kind = 'image' 时有值，指向复制后的封面文件。
ALTER TABLE category ADD COLUMN cover_path TEXT;
