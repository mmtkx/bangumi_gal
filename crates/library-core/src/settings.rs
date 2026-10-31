use std::collections::BTreeMap;

use crate::error::Result;

/// 设置项的键名，集中定义避免各处散落字符串。
pub mod keys {
    pub const ANIME_ROOT: &str = "anime_root";
    pub const GAME_ROOT: &str = "game_root";
    pub const METADATA_CACHE_DIR: &str = "metadata_cache_dir";
    /// 外部播放器可执行文件路径（plan.md 6.4 的「用户配置的外部播放器」）。
    pub const PLAYER_PATH: &str = "player_path";

    /// 播放器启动参数模板，占位符 `{file}` 与 `{seconds}`。
    pub const PLAYER_ARGS: &str = "player_args";

    /// Bangumi OAuth App ID（`client_id`）。
    pub const BANGUMI_CLIENT_ID: &str = "bangumi_client_id";
    /// Bangumi OAuth App Secret（`client_secret`），仅存本机数据库。
    pub const BANGUMI_CLIENT_SECRET: &str = "bangumi_client_secret";
    /// OAuth 回调地址，必须与 Bangumi 应用后台填写值完全一致。
    pub const BANGUMI_REDIRECT_URI: &str = "bangumi_redirect_uri";
    /// 出站代理（如 `http://127.0.0.1:7890`）；空表示自动探测系统代理。
    pub const BANGUMI_PROXY: &str = "bangumi_proxy";

    /// 插件根目录；空表示使用默认位置（应用数据目录下的 plugins）。
    pub const PLUGINS_DIR: &str = "plugins_dir";
}

/// 键值设置存储的抽象。library-core 只负责落库，不做业务校验。
pub trait SettingsStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>>;
    fn set(&self, key: &str, value: &str) -> Result<()>;
    fn all(&self) -> Result<BTreeMap<String, String>>;
    fn remove(&self, key: &str) -> Result<()>;
}

/// `app_settings` 表的实现。
pub struct SqliteSettingsStore {
    db: std::sync::Arc<crate::db::Db>,
}

impl SqliteSettingsStore {
    pub fn new(db: std::sync::Arc<crate::db::Db>) -> Self {
        Self { db }
    }
}

impl SettingsStore for SqliteSettingsStore {
    fn get(&self, key: &str) -> Result<Option<String>> {
        self.db.with_conn(|conn| {
            use rusqlite::OptionalExtension;
            Ok(conn
                .query_row(
                    "SELECT value FROM app_settings WHERE key = ?1",
                    [key],
                    |row| row.get::<_, String>(0),
                )
                .optional()?)
        })
    }

    fn set(&self, key: &str, value: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![key, value],
            )?;
            Ok(())
        })
    }

    fn all(&self) -> Result<BTreeMap<String, String>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare("SELECT key, value FROM app_settings")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut map = BTreeMap::new();
            for row in rows {
                let (key, value) = row?;
                map.insert(key, value);
            }
            Ok(map)
        })
    }

    fn remove(&self, key: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM app_settings WHERE key = ?1", [key])?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::sync::Arc;

    fn store() -> SqliteSettingsStore {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteSettingsStore::new(Arc::new(db))
    }

    #[test]
    fn set_get_overwrite_and_remove() {
        let store = store();
        assert!(store.get(keys::ANIME_ROOT).expect("get").is_none());
        store.set(keys::ANIME_ROOT, "D:/anime").expect("set");
        assert_eq!(
            store.get(keys::ANIME_ROOT).expect("get"),
            Some("D:/anime".to_owned())
        );
        store.set(keys::ANIME_ROOT, "E:/anime").expect("overwrite");
        assert_eq!(
            store.get(keys::ANIME_ROOT).expect("get"),
            Some("E:/anime".to_owned())
        );
        store.set(keys::GAME_ROOT, "E:/game").expect("set");
        assert_eq!(store.all().expect("all").len(), 2);
        store.remove(keys::ANIME_ROOT).expect("remove");
        assert!(store.get(keys::ANIME_ROOT).expect("get").is_none());
    }
}
