use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};

use crate::db::Db;
use crate::error::Result;
use crate::model::MetadataIndex;
use crate::time::now_iso8601;

const COLUMNS: &str = "subject_id, cached_path, fetched_at";

/// 元数据缓存索引仓储（plan.md 5.1 `metadata_index`）。
///
/// 缓存文件本体由 bangumi-sync 写入可配置目录（ADR-009），本表只记录路径与抓取时间，
/// 便于判断是否需要刷新与在设置页展示缓存占用。
pub trait MetadataIndexRepository: Send + Sync {
    fn upsert(&self, subject_id: i64, cached_path: &str) -> Result<MetadataIndex>;
    fn get(&self, subject_id: i64) -> Result<Option<MetadataIndex>>;
    fn list(&self) -> Result<Vec<MetadataIndex>>;
    fn remove(&self, subject_id: i64) -> Result<()>;
    fn count(&self) -> Result<i64>;
}

pub struct SqliteMetadataIndexRepository {
    db: Arc<Db>,
}

impl SqliteMetadataIndexRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

fn map_row(row: &Row<'_>) -> rusqlite::Result<MetadataIndex> {
    Ok(MetadataIndex {
        subject_id: row.get(0)?,
        cached_path: row.get(1)?,
        fetched_at: row.get(2)?,
    })
}

impl MetadataIndexRepository for SqliteMetadataIndexRepository {
    fn upsert(&self, subject_id: i64, cached_path: &str) -> Result<MetadataIndex> {
        let now = now_iso8601();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO metadata_index (subject_id, cached_path, fetched_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(subject_id) DO UPDATE SET
                   cached_path = excluded.cached_path,
                   fetched_at = excluded.fetched_at",
                params![subject_id, cached_path, now],
            )?;
            conn.query_row(
                &format!("SELECT {COLUMNS} FROM metadata_index WHERE subject_id = ?1"),
                [subject_id],
                map_row,
            )
            .optional()?
            .ok_or_else(|| crate::CoreError::ItemNotFound(subject_id.to_string()))
        })
    }

    fn get(&self, subject_id: i64) -> Result<Option<MetadataIndex>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM metadata_index WHERE subject_id = ?1"),
                    [subject_id],
                    map_row,
                )
                .optional()?)
        })
    }

    fn list(&self) -> Result<Vec<MetadataIndex>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM metadata_index ORDER BY fetched_at DESC"
            ))?;
            let rows = stmt.query_map([], map_row)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    fn remove(&self, subject_id: i64) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM metadata_index WHERE subject_id = ?1",
                [subject_id],
            )?;
            Ok(())
        })
    }

    fn count(&self) -> Result<i64> {
        self.db.with_conn(|conn| {
            Ok(conn.query_row("SELECT COUNT(*) FROM metadata_index", [], |row| row.get(0))?)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteMetadataIndexRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteMetadataIndexRepository::new(Arc::new(db))
    }

    #[test]
    fn upsert_and_get() {
        let repo = repo();
        let row = repo
            .upsert(400602, "D:/cache/400602/subject.json")
            .expect("upsert");
        assert_eq!(row.subject_id, 400602);
        assert_eq!(
            repo.get(400602).expect("get").expect("exists").cached_path,
            "D:/cache/400602/subject.json"
        );
    }

    #[test]
    fn upsert_overwrites_path_and_refreshes_time() {
        let repo = repo();
        repo.upsert(1, "a").expect("first");
        let second = repo.upsert(1, "b").expect("second");
        assert_eq!(second.cached_path, "b");
        assert_eq!(repo.count().expect("count"), 1);
    }

    #[test]
    fn list_and_remove() {
        let repo = repo();
        repo.upsert(1, "a").expect("1");
        repo.upsert(2, "b").expect("2");
        assert_eq!(repo.list().expect("list").len(), 2);
        repo.remove(1).expect("remove");
        assert_eq!(repo.count().expect("count"), 1);
    }
}
