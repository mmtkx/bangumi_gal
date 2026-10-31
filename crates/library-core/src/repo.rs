use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};

use crate::db::Db;
use crate::error::{CoreError, Result};
use crate::model::{ItemKind, LibraryItem, NewItem, UpsertOutcome};
use crate::time::now_iso8601;

const ITEM_COLUMNS: &str =
    "id, kind, title, bangumi_subject_id, folder_path, created_at, updated_at, \
     collection_status, rating, collection_updated_at, file_filter";

/// 条目仓储接口。
///
/// 扫描器只依赖本 trait（定义在 library-core），因此 scanner 之间、scanner 与 future 模块之间
/// 不需要互相依赖（plan.md 3.2）。
pub trait LibraryRepository: Send + Sync {
    /// 按 `folder_path` 幂等写入。已存在时只更新标题与类型，绝不覆盖 `bangumi_subject_id`。
    fn upsert_scanned(&self, item: &NewItem) -> Result<UpsertOutcome>;

    fn list_items(&self, kind: Option<ItemKind>) -> Result<Vec<LibraryItem>>;

    fn get_item(&self, id: &str) -> Result<Option<LibraryItem>>;

    fn find_by_folder(&self, folder_path: &str) -> Result<Option<LibraryItem>>;

    /// 手动绑定/解绑 Bangumi 条目 ID（本阶段只落库，不发起网络请求）。
    fn bind_bangumi_subject(&self, id: &str, subject_id: Option<i64>) -> Result<LibraryItem>;

    /// 更新收藏状态 / 评分，刷新 collection_updated_at 供冲突比较（0002 迁移）。
    fn set_collection(
        &self,
        id: &str,
        collection_status: Option<i64>,
        rating: Option<i64>,
    ) -> Result<LibraryItem>;

    /// 删除条目记录（仅数据库，不动磁盘文件）。关联 progress / game_binding_index 由外键级联清理。
    fn delete_item(&self, id: &str) -> Result<()>;

    /// 清空条目记录（可按类型）。只删数据库，磁盘文件一概不碰。
    ///
    /// 返回删除的行数。关联的进度、绑定、同步队列、冲突由外键级联清理。
    fn delete_all_items(&self, kind: Option<ItemKind>) -> Result<usize>;

    fn count_items(&self, kind: Option<ItemKind>) -> Result<i64>;
}

/// `library_item` 的 SQLite 实现。
pub struct SqliteRepository {
    db: Arc<Db>,
}

impl SqliteRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }

    fn fetch(conn: &rusqlite::Connection, id: &str) -> Result<LibraryItem> {
        conn.query_row(
            &format!("SELECT {ITEM_COLUMNS} FROM library_item WHERE id = ?1"),
            [id],
            map_item,
        )
        .optional()?
        .ok_or_else(|| CoreError::ItemNotFound(id.to_owned()))
    }
}

fn map_item(row: &Row<'_>) -> rusqlite::Result<LibraryItem> {
    let kind_raw: String = row.get(1)?;
    let kind = ItemKind::parse(&kind_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(err))
    })?;
    Ok(LibraryItem {
        id: row.get(0)?,
        kind,
        title: row.get(2)?,
        bangumi_subject_id: row.get(3)?,
        folder_path: row.get(4)?,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
        collection_status: row.get(7)?,
        rating: row.get(8)?,
        collection_updated_at: row.get(9)?,
        file_filter: row.get(10)?,
    })
}

impl LibraryRepository for SqliteRepository {
    fn upsert_scanned(&self, item: &NewItem) -> Result<UpsertOutcome> {
        let now = now_iso8601();
        self.db.with_conn(|conn| {
            // 身份是 (folder_path, file_filter)：根目录松散文件会被拆成多个条目，
            // 只按 folder_path 查会把它们互相覆盖。
            let existing: Option<(String, String, String)> = conn
                .query_row(
                    "SELECT id, title, kind FROM library_item
                     WHERE folder_path = ?1 AND IFNULL(file_filter, '') = ?2",
                    rusqlite::params![item.folder_path, item.file_filter.as_deref().unwrap_or("")],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;

            match existing {
                Some((id, title, kind)) => {
                    if title == item.title && kind == item.kind.as_str() {
                        return Ok(UpsertOutcome::Unchanged(Self::fetch(conn, &id)?));
                    }
                    conn.execute(
                        "UPDATE library_item SET title = ?2, kind = ?3, updated_at = ?4 WHERE id = ?1",
                        params![id, item.title, item.kind.as_str(), now],
                    )?;
                    Ok(UpsertOutcome::Updated(Self::fetch(conn, &id)?))
                }
                None => {
                    let id = uuid::Uuid::new_v4().to_string();
                    conn.execute(
                        "INSERT INTO library_item
                             (id, kind, title, bangumi_subject_id, folder_path, file_filter, created_at, updated_at)
                         VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7)",
                        params![
                            id,
                            item.kind.as_str(),
                            item.title,
                            item.folder_path,
                            item.file_filter,
                            now,
                            now
                        ],
                    )?;
                    Ok(UpsertOutcome::Created(Self::fetch(conn, &id)?))
                }
            }
        })
    }

    fn list_items(&self, kind: Option<ItemKind>) -> Result<Vec<LibraryItem>> {
        self.db.with_conn(|conn| {
            let mut items = Vec::new();
            match kind {
                Some(kind) => {
                    let mut stmt = conn.prepare(&format!(
                        "SELECT {ITEM_COLUMNS} FROM library_item WHERE kind = ?1
                         ORDER BY title COLLATE NOCASE ASC"
                    ))?;
                    let rows = stmt.query_map([kind.as_str()], map_item)?;
                    for row in rows {
                        items.push(row?);
                    }
                }
                None => {
                    let mut stmt = conn.prepare(&format!(
                        "SELECT {ITEM_COLUMNS} FROM library_item
                         ORDER BY kind ASC, title COLLATE NOCASE ASC"
                    ))?;
                    let rows = stmt.query_map([], map_item)?;
                    for row in rows {
                        items.push(row?);
                    }
                }
            }
            Ok(items)
        })
    }

    fn get_item(&self, id: &str) -> Result<Option<LibraryItem>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ITEM_COLUMNS} FROM library_item WHERE id = ?1"),
                    [id],
                    map_item,
                )
                .optional()?)
        })
    }

    fn find_by_folder(&self, folder_path: &str) -> Result<Option<LibraryItem>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {ITEM_COLUMNS} FROM library_item WHERE folder_path = ?1"),
                    [folder_path],
                    map_item,
                )
                .optional()?)
        })
    }

    fn bind_bangumi_subject(&self, id: &str, subject_id: Option<i64>) -> Result<LibraryItem> {
        let now = now_iso8601();
        self.db.with_conn(|conn| {
            let affected = conn.execute(
                "UPDATE library_item SET bangumi_subject_id = ?2, updated_at = ?3 WHERE id = ?1",
                params![id, subject_id, now],
            )?;
            if affected == 0 {
                return Err(CoreError::ItemNotFound(id.to_owned()));
            }
            Self::fetch(conn, id)
        })
    }

    fn delete_item(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute("DELETE FROM library_item WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    fn delete_all_items(&self, kind: Option<ItemKind>) -> Result<usize> {
        self.db.with_conn(|conn| {
            let removed = match kind {
                Some(kind) => {
                    conn.execute("DELETE FROM library_item WHERE kind = ?1", [kind.as_str()])?
                }
                None => conn.execute("DELETE FROM library_item", [])?,
            };
            Ok(removed)
        })
    }

    fn set_collection(
        &self,
        id: &str,
        collection_status: Option<i64>,
        rating: Option<i64>,
    ) -> Result<LibraryItem> {
        let now = now_iso8601();
        self.db.with_conn(|conn| {
            let affected = conn.execute(
                "UPDATE library_item
                 SET collection_status = ?2, rating = ?3, collection_updated_at = ?4, updated_at = ?5
                 WHERE id = ?1",
                params![id, collection_status, rating, now, now],
            )?;
            if affected == 0 {
                return Err(CoreError::ItemNotFound(id.to_owned()));
            }
            Self::fetch(conn, id)
        })
    }

    fn count_items(&self, kind: Option<ItemKind>) -> Result<i64> {
        self.db.with_conn(|conn| match kind {
            Some(kind) => Ok(conn.query_row(
                "SELECT COUNT(*) FROM library_item WHERE kind = ?1",
                [kind.as_str()],
                |row| row.get(0),
            )?),
            None => Ok(conn.query_row("SELECT COUNT(*) FROM library_item", [], |row| row.get(0))?),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> SqliteRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteRepository::new(Arc::new(db))
    }

    fn item(title: &str, folder: &str) -> NewItem {
        NewItem {
            kind: ItemKind::Anime,
            title: title.to_owned(),
            folder_path: folder.to_owned(),
            file_filter: None,
        }
    }

    #[test]
    fn upsert_is_idempotent_and_detects_changes() {
        let repo = repo();
        let created = repo.upsert_scanned(&item("A", "D:/a")).expect("create");
        assert!(matches!(created, UpsertOutcome::Created(_)));

        let again = repo.upsert_scanned(&item("A", "D:/a")).expect("again");
        assert!(matches!(again, UpsertOutcome::Unchanged(_)));

        let renamed = repo.upsert_scanned(&item("A2", "D:/a")).expect("rename");
        match renamed {
            UpsertOutcome::Updated(updated) => assert_eq!(updated.title, "A2"),
            other => panic!("期望 Updated，实际 {other:?}"),
        }
        assert_eq!(repo.count_items(None).expect("count"), 1);
    }

    #[test]
    fn rescan_keeps_manual_binding() {
        let repo = repo();
        let created = repo.upsert_scanned(&item("A", "D:/a")).expect("create");
        let id = match created {
            UpsertOutcome::Created(item) => item.id,
            other => panic!("期望 Created，实际 {other:?}"),
        };
        repo.bind_bangumi_subject(&id, Some(1234)).expect("bind");
        repo.upsert_scanned(&item("A", "D:/a")).expect("rescan");

        let stored = repo.get_item(&id).expect("get").expect("exists");
        assert_eq!(stored.bangumi_subject_id, Some(1234));
    }

    #[test]
    fn bind_unknown_item_fails() {
        let repo = repo();
        assert!(matches!(
            repo.bind_bangumi_subject("missing", Some(1))
                .expect_err("应当失败"),
            CoreError::ItemNotFound(_)
        ));
    }

    #[test]
    fn list_filters_by_kind() {
        let repo = repo();
        repo.upsert_scanned(&item("Anime", "D:/a")).expect("a");
        repo.upsert_scanned(&NewItem {
            kind: ItemKind::Game,
            title: "Game".into(),
            folder_path: "D:/g".into(),
            file_filter: None,
        })
        .expect("g");

        assert_eq!(repo.list_items(None).expect("all").len(), 2);
        assert_eq!(
            repo.list_items(Some(ItemKind::Anime)).expect("anime").len(),
            1
        );
        assert_eq!(repo.count_items(Some(ItemKind::Game)).expect("game"), 1);
    }

    #[test]
    fn set_collection_persists_and_timestamps() {
        let repo = repo();
        let created = repo.upsert_scanned(&item("A", "D:/a")).expect("create");
        let (id, fresh) = match created {
            UpsertOutcome::Created(item) => (item.id.clone(), item),
            other => panic!("期望 Created，实际 {other:?}"),
        };
        assert!(fresh.collection_status.is_none());
        assert!(fresh.rating.is_none());
        assert!(fresh.collection_updated_at.is_none());

        let updated = repo.set_collection(&id, Some(3), Some(8)).expect("set");
        assert_eq!(updated.collection_status, Some(3));
        assert_eq!(updated.rating, Some(8));
        assert!(updated.collection_updated_at.is_some());

        // 重读确认落库。
        let stored = repo.get_item(&id).expect("get").expect("exists");
        assert_eq!(stored.collection_status, Some(3));
        assert_eq!(stored.rating, Some(8));
        assert!(stored.collection_updated_at.is_some());
    }
}
