use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};

use crate::db::Db;
use crate::error::{CoreError, Result};
use crate::model::PluginRegistryEntry;
use crate::time::now_iso8601;

const COLUMNS: &str = "id, version, kind, manifest_path, enabled, installed_at";

/// 插件注册表（plan.md 5.1 `plugin_registry`）。
///
/// 只记录「已安装」的插件与启停状态；插件文件本体在磁盘上，由 plugin-host 解析。
pub trait PluginRegistryRepository: Send + Sync {
    /// 安装或更新；已存在时保留 `enabled` 与 `installed_at`。
    fn upsert(
        &self,
        id: &str,
        version: &str,
        kind: &str,
        manifest_path: &str,
    ) -> Result<PluginRegistryEntry>;
    fn get(&self, id: &str) -> Result<Option<PluginRegistryEntry>>;
    fn list(&self) -> Result<Vec<PluginRegistryEntry>>;
    fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginRegistryEntry>;
    fn remove(&self, id: &str) -> Result<()>;
    fn count(&self) -> Result<i64>;
}

pub struct SqlitePluginRegistryRepository {
    db: Arc<Db>,
}

impl SqlitePluginRegistryRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

fn map_row(row: &Row<'_>) -> rusqlite::Result<PluginRegistryEntry> {
    Ok(PluginRegistryEntry {
        id: row.get(0)?,
        version: row.get(1)?,
        kind: row.get(2)?,
        manifest_path: row.get(3)?,
        enabled: row.get::<_, i64>(4)? != 0,
        installed_at: row.get(5)?,
    })
}

impl PluginRegistryRepository for SqlitePluginRegistryRepository {
    fn upsert(
        &self,
        id: &str,
        version: &str,
        kind: &str,
        manifest_path: &str,
    ) -> Result<PluginRegistryEntry> {
        let now = now_iso8601();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO plugin_registry (id, version, kind, manifest_path, enabled, installed_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                   version = excluded.version,
                   kind = excluded.kind,
                   manifest_path = excluded.manifest_path",
                params![id, version, kind, manifest_path, now],
            )?;
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM plugin_registry WHERE id = ?1"),
                [id],
                map_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::ItemNotFound(id.to_owned()))
        })
    }

    fn get(&self, id: &str) -> Result<Option<PluginRegistryEntry>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM plugin_registry WHERE id = ?1"),
                    [id],
                    map_row,
                )
                .optional()?)
        })
    }

    fn list(&self) -> Result<Vec<PluginRegistryEntry>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM plugin_registry ORDER BY id ASC"
            ))?;
            let rows = stmt.query_map([], map_row)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginRegistryEntry> {
        let changed = self.db.with_conn(|conn| {
            Ok(conn.execute(
                "UPDATE plugin_registry SET enabled = ?2 WHERE id = ?1",
                params![id, i64::from(enabled)],
            )?)
        })?;
        if changed == 0 {
            return Err(CoreError::ItemNotFound(id.to_owned()));
        }
        self.get(id)?
            .ok_or_else(|| CoreError::ItemNotFound(id.to_owned()))
    }

    fn remove(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM plugin_registry WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    fn count(&self) -> Result<i64> {
        self.db.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM plugin_registry", [], |row| row.get(0))?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqlitePluginRegistryRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqlitePluginRegistryRepository::new(Arc::new(db))
    }

    #[test]
    fn upsert_and_get() {
        let repo = repo();
        let entry = repo
            .upsert("com.example.a", "0.1.0", "ui", "D:/plugins/a/manifest.json")
            .expect("upsert");
        assert_eq!(entry.id, "com.example.a");
        assert!(!entry.enabled);
        assert_eq!(
            repo.get("com.example.a")
                .expect("get")
                .expect("exists")
                .version,
            "0.1.0"
        );
    }

    #[test]
    fn upsert_preserves_enabled_and_installed_at() {
        let repo = repo();
        repo.upsert("a", "0.1.0", "ui", "D:/a/manifest.json")
            .expect("1");
        let enabled = repo.set_enabled("a", true).expect("enable");
        let again = repo
            .upsert("a", "0.2.0", "ui", "D:/a/manifest.json")
            .expect("2");
        assert!(again.enabled);
        assert_eq!(again.version, "0.2.0");
        assert_eq!(again.installed_at, enabled.installed_at);
    }

    #[test]
    fn set_enabled_rejects_unknown() {
        let repo = repo();
        assert_eq!(
            repo.set_enabled("nope", true).expect_err("unknown").code(),
            "item_not_found"
        );
    }

    #[test]
    fn list_and_remove() {
        let repo = repo();
        repo.upsert("a", "1", "ui", "D:/a").expect("a");
        repo.upsert("b", "1", "capability", "D:/b").expect("b");
        assert_eq!(repo.list().expect("list").len(), 2);
        repo.remove("a").expect("remove");
        assert_eq!(repo.count().expect("count"), 1);
    }
}
