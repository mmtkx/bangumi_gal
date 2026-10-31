use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};

use crate::db::Db;
use crate::error::{CoreError, Result};
use crate::model::{Progress, ProgressSource};
use crate::time::now_iso8601;

const COLUMNS: &str = "item_id, episode, watched_seconds, source, updated_at";

/// 观看进度仓储。
///
/// 外部播放器只能估算位置（plan.md 6.4），因此进度由本地会话周期写入，
/// `source` 固定为 local；M3 接入云端后同一集会有 local 与 cloud 两行，便于冲突计算。
pub trait ProgressRepository: Send + Sync {
    fn upsert(
        &self,
        item_id: &str,
        episode: u32,
        watched_seconds: u64,
        source: ProgressSource,
    ) -> Result<Progress>;

    fn get(&self, item_id: &str, episode: u32, source: ProgressSource) -> Result<Option<Progress>>;

    fn list_by_item(&self, item_id: &str) -> Result<Vec<Progress>>;
}

pub struct SqliteProgressRepository {
    db: Arc<Db>,
}

impl SqliteProgressRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

fn map_row(row: &Row<'_>) -> rusqlite::Result<Progress> {
    Ok(Progress {
        item_id: row.get(0)?,
        episode: row.get(1)?,
        watched_seconds: row.get(2)?,
        source: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

impl ProgressRepository for SqliteProgressRepository {
    fn upsert(
        &self,
        item_id: &str,
        episode: u32,
        watched_seconds: u64,
        source: ProgressSource,
    ) -> Result<Progress> {
        let now = now_iso8601();
        self.db.with_conn(|conn| {
            let affected = conn.execute(
                "INSERT INTO progress (item_id, episode, watched_seconds, source, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(item_id, episode, source)
                 DO UPDATE SET watched_seconds = excluded.watched_seconds, updated_at = excluded.updated_at",
                params![
                    item_id,
                    episode as i64,
                    watched_seconds as i64,
                    source.as_str(),
                    now
                ],
            )?;
            if affected == 0 {
                return Err(CoreError::ItemNotFound(item_id.to_owned()));
            }
            conn.query_row(
                &format!(
                    "SELECT {COLUMNS} FROM progress WHERE item_id = ?1 AND episode = ?2 AND source = ?3"
                ),
                params![item_id, episode as i64, source.as_str()],
                map_row,
            )
            .optional()?
            .ok_or_else(|| CoreError::ItemNotFound(item_id.to_owned()))
        })
    }

    fn get(&self, item_id: &str, episode: u32, source: ProgressSource) -> Result<Option<Progress>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!(
                        "SELECT {COLUMNS} FROM progress WHERE item_id = ?1 AND episode = ?2 AND source = ?3"
                    ),
                    params![item_id, episode as i64, source.as_str()],
                    map_row,
                )
                .optional()?)
        })
    }

    fn list_by_item(&self, item_id: &str) -> Result<Vec<Progress>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM progress WHERE item_id = ?1 ORDER BY episode ASC, source ASC"
            ))?;
            let rows = stmt.query_map([item_id], map_row)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, NewItem};
    use crate::repo::{LibraryRepository, SqliteRepository};

    fn setup() -> (SqliteRepository, SqliteProgressRepository, String) {
        let db = Arc::new(Db::open_in_memory().expect("db"));
        db.migrate().expect("migrate");
        let repo = SqliteRepository::new(Arc::clone(&db));
        let item = repo
            .upsert_scanned(&NewItem {
                kind: ItemKind::Anime,
                title: "Show".into(),
                folder_path: "D:/anime/show".into(),
                file_filter: None,
            })
            .expect("item");
        let id = match item {
            crate::model::UpsertOutcome::Created(item) => item.id,
            other => panic!("期望 Created，实际 {other:?}"),
        };
        (repo, SqliteProgressRepository::new(db), id)
    }

    #[test]
    fn upsert_overwrites_same_episode() {
        let (_repo, progress, id) = setup();
        let first = progress
            .upsert(&id, 1, 30, ProgressSource::Local)
            .expect("insert");
        assert_eq!(first.watched_seconds, 30);

        let second = progress
            .upsert(&id, 1, 90, ProgressSource::Local)
            .expect("update");
        assert_eq!(second.watched_seconds, 90);
        assert_eq!(progress.list_by_item(&id).expect("list").len(), 1);
    }

    #[test]
    fn local_and_cloud_rows_coexist() {
        let (_repo, progress, id) = setup();
        progress
            .upsert(&id, 1, 10, ProgressSource::Local)
            .expect("l");
        progress
            .upsert(&id, 1, 20, ProgressSource::Cloud)
            .expect("c");
        assert_eq!(progress.list_by_item(&id).expect("list").len(), 2);
        assert_eq!(
            progress
                .get(&id, 1, ProgressSource::Cloud)
                .expect("get")
                .expect("exists")
                .watched_seconds,
            20
        );
    }

    #[test]
    fn unknown_item_violates_foreign_key() {
        let (_repo, progress, _id) = setup();
        assert!(progress
            .upsert("missing", 1, 5, ProgressSource::Local)
            .is_err());
    }
}
