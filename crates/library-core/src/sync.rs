use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};

use crate::db::Db;
use crate::error::{CoreError, Result};
use crate::model::{Conflict, NewConflict, SyncQueueEntry, SyncStatus};
use crate::time::now_iso8601;

const QUEUE_COLUMNS: &str =
    "id, item_id, op, payload_json, status, attempts, next_retry_at, created_at";

/// 同步推送队列仓储。
///
/// 本地变更（收藏状态/评分/进度）先入队（pending），由用户手动触发推送（ADR-007）。
/// 限速与网络调用由 bangumi-sync 负责，本仓储只管状态机流转。
pub trait SyncQueueRepository: Send + Sync {
    /// 入队一条待推送操作。同 item+op 不去重：连续两次改评分会产生两条，按顺序推送更直观。
    fn enqueue(
        &self,
        item_id: Option<&str>,
        op: &str,
        payload_json: &str,
    ) -> Result<SyncQueueEntry>;

    /// 取下一批 pending（按 created_at 升序，次重试到期优先）。
    fn list_pending(&self, limit: usize) -> Result<Vec<SyncQueueEntry>>;

    fn list_all(&self) -> Result<Vec<SyncQueueEntry>>;

    /// 标记为发送中（attempts += 1）。
    fn mark_sending(&self, id: &str) -> Result<SyncQueueEntry>;

    fn mark_done(&self, id: &str) -> Result<()>;

    fn mark_failed(&self, id: &str, next_retry_at: Option<&str>) -> Result<SyncQueueEntry>;

    /// 删除单条队列记录（用户手动丢弃待推送项）。
    fn delete(&self, id: &str) -> Result<bool>;

    /// 清理所有 done 记录，返回删除条数。
    fn clear_done(&self) -> Result<usize>;

    fn count_by_status(&self, status: SyncStatus) -> Result<i64>;
}

pub struct SqliteSyncQueueRepository {
    db: Arc<Db>,
}

impl SqliteSyncQueueRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

fn map_queue(row: &Row<'_>) -> rusqlite::Result<SyncQueueEntry> {
    let status_raw: String = row.get(4)?;
    let status = SyncStatus::parse(&status_raw).map_err(|err| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(err))
    })?;
    Ok(SyncQueueEntry {
        id: row.get(0)?,
        item_id: row.get(1)?,
        op: row.get(2)?,
        payload_json: row.get(3)?,
        status,
        attempts: row.get(5)?,
        next_retry_at: row.get(6)?,
        created_at: row.get(7)?,
    })
}

impl SyncQueueRepository for SqliteSyncQueueRepository {
    fn enqueue(
        &self,
        item_id: Option<&str>,
        op: &str,
        payload_json: &str,
    ) -> Result<SyncQueueEntry> {
        let now = now_iso8601();
        let id = uuid::Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO sync_queue (id, item_id, op, payload_json, status, attempts, \
                 next_retry_at, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'pending', 0, NULL, ?5)",
                params![id, item_id, op, payload_json, now],
            )?;
            fetch_queue(conn, &id)
        })
    }

    fn list_pending(&self, limit: usize) -> Result<Vec<SyncQueueEntry>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, item_id, op, payload_json, status, attempts, next_retry_at, \
                 created_at FROM sync_queue
                 WHERE status = 'pending'
                   AND (next_retry_at IS NULL OR next_retry_at <= ?1)
                 ORDER BY next_retry_at IS NULL DESC, created_at ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now_iso8601(), limit as i64], map_queue)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    fn list_all(&self) -> Result<Vec<SyncQueueEntry>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, item_id, op, payload_json, status, attempts, next_retry_at, \
                 created_at FROM sync_queue ORDER BY created_at ASC",
            )?;
            let rows = stmt.query_map([], map_queue)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    fn mark_sending(&self, id: &str) -> Result<SyncQueueEntry> {
        self.db.with_conn(|conn| {
            let affected = conn.execute(
                "UPDATE sync_queue SET status = 'sending', attempts = attempts + 1
                 WHERE id = ?1 AND status IN ('pending', 'failed')",
                [id],
            )?;
            if affected == 0 {
                return Err(CoreError::ItemNotFound(id.to_owned()));
            }
            fetch_queue(conn, id)
        })
    }

    fn mark_done(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE sync_queue SET status = 'done', next_retry_at = NULL WHERE id = ?1",
                [id],
            )?;
            Ok(())
        })
    }

    fn mark_failed(&self, id: &str, next_retry_at: Option<&str>) -> Result<SyncQueueEntry> {
        self.db.with_conn(|conn| {
            conn.execute(
                "UPDATE sync_queue SET status = 'failed', next_retry_at = ?2 WHERE id = ?1",
                params![id, next_retry_at],
            )?;
            fetch_queue(conn, id)
        })
    }

    fn delete(&self, id: &str) -> Result<bool> {
        self.db.with_conn(|conn| {
            let affected = conn.execute("DELETE FROM sync_queue WHERE id = ?1", [id])?;
            Ok(affected > 0)
        })
    }

    fn clear_done(&self) -> Result<usize> {
        self.db
            .with_conn(|conn| Ok(conn.execute("DELETE FROM sync_queue WHERE status = 'done'", [])?))
    }

    fn count_by_status(&self, status: SyncStatus) -> Result<i64> {
        self.db.with_conn(|conn| {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM sync_queue WHERE status = ?1",
                [status.as_str()],
                |row| row.get(0),
            )?)
        })
    }
}

fn fetch_queue(conn: &rusqlite::Connection, id: &str) -> Result<SyncQueueEntry> {
    conn.query_row(
        &format!("SELECT {QUEUE_COLUMNS} FROM sync_queue WHERE id = ?1"),
        [id],
        map_queue,
    )
    .optional()?
    .ok_or_else(|| CoreError::ItemNotFound(id.to_owned()))
}

/// 冲突仓储。
pub trait ConflictRepository: Send + Sync {
    fn insert(&self, conflict: &NewConflict) -> Result<Conflict>;

    fn list_unresolved(&self) -> Result<Vec<Conflict>>;

    fn resolve(&self, id: &str) -> Result<()>;

    /// 同条目同字段的新冲突覆盖旧的未解决记录，避免列表堆叠重复冲突。
    fn upsert_for_item(&self, conflict: &NewConflict) -> Result<Conflict>;

    fn clear_for_item(&self, item_id: &str) -> Result<usize>;

    fn list_resolved(&self) -> Result<Vec<Conflict>>;
}

pub struct SqliteConflictRepository {
    db: Arc<Db>,
}

impl SqliteConflictRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

const CONFLICT_COLUMNS: &str = "id, item_id, field, local_value, cloud_value, local_ts, \
     cloud_ts, resolved";

fn map_conflict(row: &Row<'_>) -> rusqlite::Result<Conflict> {
    let resolved: i64 = row.get(7)?;
    Ok(Conflict {
        id: row.get(0)?,
        item_id: row.get(1)?,
        field: row.get(2)?,
        local_value: row.get(3)?,
        cloud_value: row.get(4)?,
        local_ts: row.get(5)?,
        cloud_ts: row.get(6)?,
        resolved: resolved != 0,
    })
}

impl ConflictRepository for SqliteConflictRepository {
    fn insert(&self, conflict: &NewConflict) -> Result<Conflict> {
        let id = uuid::Uuid::new_v4().to_string();
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO conflict (id, item_id, field, local_value, cloud_value, local_ts, \
                 cloud_ts, resolved)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                params![
                    id,
                    conflict.item_id,
                    conflict.field,
                    conflict.local_value,
                    conflict.cloud_value,
                    conflict.local_ts,
                    conflict.cloud_ts,
                ],
            )?;
            fetch_conflict(conn, &id)
        })
    }

    fn upsert_for_item(&self, conflict: &NewConflict) -> Result<Conflict> {
        // 同 item+field 的未解决冲突先删后插，保留最近一次的两侧值。
        self.db.with_conn(|conn| {
            if let Some(item_id) = &conflict.item_id {
                conn.execute(
                    "DELETE FROM conflict WHERE item_id = ?1 AND field = ?2 AND resolved = 0",
                    params![item_id, conflict.field],
                )?;
            }
            let id = uuid::Uuid::new_v4().to_string();
            conn.execute(
                "INSERT INTO conflict (id, item_id, field, local_value, cloud_value, local_ts, \
                 cloud_ts, resolved)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)",
                params![
                    id,
                    conflict.item_id,
                    conflict.field,
                    conflict.local_value,
                    conflict.cloud_value,
                    conflict.local_ts,
                    conflict.cloud_ts,
                ],
            )?;
            fetch_conflict(conn, &id)
        })
    }

    fn list_unresolved(&self) -> Result<Vec<Conflict>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, item_id, field, local_value, cloud_value, local_ts, cloud_ts, resolved \
                 FROM conflict WHERE resolved = 0 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map([], map_conflict)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }

    fn resolve(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let affected = conn.execute("UPDATE conflict SET resolved = 1 WHERE id = ?1", [id])?;
            if affected == 0 {
                return Err(CoreError::ItemNotFound(id.to_owned()));
            }
            Ok(())
        })
    }

    fn clear_for_item(&self, item_id: &str) -> Result<usize> {
        self.db.with_conn(|conn| {
            Ok(conn.execute("DELETE FROM conflict WHERE item_id = ?1", [item_id])?)
        })
    }

    fn list_resolved(&self) -> Result<Vec<Conflict>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, item_id, field, local_value, cloud_value, local_ts, cloud_ts, resolved \
                 FROM conflict WHERE resolved = 1 ORDER BY id ASC",
            )?;
            let rows = stmt.query_map([], map_conflict)?;
            let mut items = Vec::new();
            for row in rows {
                items.push(row?);
            }
            Ok(items)
        })
    }
}

fn fetch_conflict(conn: &rusqlite::Connection, id: &str) -> Result<Conflict> {
    conn.query_row(
        &format!("SELECT {CONFLICT_COLUMNS} FROM conflict WHERE id = ?1"),
        [id],
        map_conflict,
    )
    .optional()?
    .ok_or_else(|| CoreError::ItemNotFound(id.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, NewItem, UpsertOutcome};
    use crate::repo::{LibraryRepository, SqliteRepository};

    fn setup() -> (
        SqliteRepository,
        SqliteSyncQueueRepository,
        SqliteConflictRepository,
        String,
    ) {
        let db = Arc::new(Db::open_in_memory().expect("db"));
        db.migrate().expect("migrate");
        let repo = SqliteRepository::new(Arc::clone(&db));
        let id = match repo
            .upsert_scanned(&NewItem {
                kind: ItemKind::Anime,
                title: "Show".into(),
                folder_path: "D:/anime/show".into(),
                file_filter: None,
            })
            .expect("item")
        {
            UpsertOutcome::Created(item) => item.id,
            other => panic!("期望 Created，实际 {other:?}"),
        };
        (
            repo,
            SqliteSyncQueueRepository::new(Arc::clone(&db)),
            SqliteConflictRepository::new(db),
            id,
        )
    }

    #[test]
    fn enqueue_and_list_pending() {
        let (_repo, queue, _conflict, id) = setup();
        let entry = queue
            .enqueue(Some(&id), "collection", r#"{"status":3}"#)
            .expect("enqueue");
        assert_eq!(entry.status, SyncStatus::Pending);
        assert_eq!(entry.attempts, 0);

        let pending = queue.list_pending(10).expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].op, "collection");
    }

    #[test]
    fn list_pending_respects_next_retry() {
        let (_repo, queue, _conflict, id) = setup();
        queue.enqueue(Some(&id), "collection", "{}").expect("e1");
        // 把这条置 failed 且 next_retry_at 设到未来，list_pending 应跳过它。
        let failed = queue
            .mark_failed(
                &queue.list_all().expect("all")[0].id,
                Some("9999-01-01T00:00:00Z"),
            )
            .expect("failed");
        assert_eq!(failed.status, SyncStatus::Failed);
        assert!(queue.list_pending(10).expect("pending").is_empty());
    }

    #[test]
    fn mark_sending_increments_attempts() {
        let (_repo, queue, _conflict, id) = setup();
        let entry = queue.enqueue(Some(&id), "progress", "{}").expect("enqueue");
        let sending = queue.mark_sending(&entry.id).expect("sending");
        assert_eq!(sending.status, SyncStatus::Sending);
        assert_eq!(sending.attempts, 1);
    }

    #[test]
    fn mark_sending_rejects_done_entry() {
        let (_repo, queue, _conflict, id) = setup();
        let entry = queue.enqueue(Some(&id), "progress", "{}").expect("enqueue");
        queue.mark_done(&entry.id).expect("done");
        assert!(queue.mark_sending(&entry.id).is_err());
    }

    #[test]
    fn clear_done_only_removes_done() {
        let (_repo, queue, _conflict, id) = setup();
        let e1 = queue.enqueue(Some(&id), "progress", "{}").expect("e1");
        let _e2 = queue.enqueue(Some(&id), "progress", "{}").expect("e2");
        queue.mark_done(&e1.id).expect("done e1");
        assert_eq!(queue.clear_done().expect("clear"), 1);
        assert_eq!(queue.list_all().expect("all").len(), 1);
    }

    #[test]
    fn count_by_status() {
        let (_repo, queue, _conflict, id) = setup();
        let e1 = queue.enqueue(Some(&id), "progress", "{}").expect("e1");
        let _e2 = queue.enqueue(Some(&id), "progress", "{}").expect("e2");
        queue.mark_done(&e1.id).expect("done");
        assert_eq!(queue.count_by_status(SyncStatus::Done).expect("done"), 1);
        assert_eq!(
            queue.count_by_status(SyncStatus::Pending).expect("pending"),
            1
        );
    }

    #[test]
    fn conflict_insert_and_resolve() {
        let (_repo, _queue, conflict, id) = setup();
        let row = conflict
            .insert(&NewConflict {
                item_id: Some(id.clone()),
                field: "collection_status".into(),
                local_value: Some("3".into()),
                cloud_value: Some("2".into()),
                local_ts: Some("2026-10-29T10:00:00Z".into()),
                cloud_ts: Some("2026-10-29T09:00:00Z".into()),
            })
            .expect("insert");
        assert!(!row.resolved);
        assert_eq!(conflict.list_unresolved().expect("list").len(), 1);

        conflict.resolve(&row.id).expect("resolve");
        assert!(conflict.list_unresolved().expect("list").is_empty());
        assert_eq!(conflict.list_resolved().expect("resolved").len(), 1);
    }

    #[test]
    fn upsert_replaces_existing_unresolved() {
        let (_repo, _queue, conflict, id) = setup();
        conflict
            .insert(&NewConflict {
                item_id: Some(id.clone()),
                field: "rating".into(),
                local_value: Some("8".into()),
                cloud_value: Some("6".into()),
                local_ts: None,
                cloud_ts: None,
            })
            .expect("first");
        let replaced = conflict
            .upsert_for_item(&NewConflict {
                item_id: Some(id.clone()),
                field: "rating".into(),
                local_value: Some("9".into()),
                cloud_value: Some("6".into()),
                local_ts: None,
                cloud_ts: None,
            })
            .expect("upsert");
        assert_eq!(replaced.local_value.as_deref(), Some("9"));
        // 旧的未解决被删，只剩新的一条。
        assert_eq!(conflict.list_unresolved().expect("list").len(), 1);
    }

    #[test]
    fn clear_for_item_removes_all_states() {
        let (_repo, _queue, conflict, id) = setup();
        conflict
            .insert(&NewConflict {
                item_id: Some(id.clone()),
                field: "rating".into(),
                local_value: None,
                cloud_value: None,
                local_ts: None,
                cloud_ts: None,
            })
            .expect("insert");
        assert_eq!(conflict.clear_for_item(&id).expect("clear"), 1);
        assert_eq!(conflict.list_unresolved().expect("list").len(), 0);
    }
}
