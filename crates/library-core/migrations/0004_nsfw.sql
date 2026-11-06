-- 成人内容标记：unknown（未判定）/ safe（正常）/ adult（成人）。
--
-- 三态而不是布尔：未判定与正常必须分开——未判定的一概隐藏会让用户以为条目丢了，
-- 而自动判定只在「未判定」时写入，保证人工设定不会被后续刷新覆盖。
ALTER TABLE library_item ADD COLUMN nsfw TEXT NOT NULL DEFAULT 'unknown';

CREATE INDEX IF NOT EXISTS idx_library_item_nsfw ON library_item (nsfw);
