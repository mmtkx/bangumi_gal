use std::sync::Arc;

use rusqlite::{params, OptionalExtension, Row};

use crate::db::Db;
use crate::error::Result;
use crate::model::GameBindingIndex;

const COLUMNS: &str = "item_id, exe_path, last_launched_at, last_pid";

/// 游戏绑定索引仓储。
///
/// 事实来源是条目文件夹内的 .gamelauncher.json（ADR-003），本表只是查询索引，
/// 换机或手工编辑文件后可由 launcher 重新回写。
pub trait GameBindingRepository: Send + Sync {
    fn upsert(&self, binding: &GameBindingIndex) -> Result<()>;
    fn get(&self, item_id: &str) -> Result<Option<GameBindingIndex>>;
    fn clear(&self, item_id: &str) -> Result<()>;
}

pub struct SqliteGameBindingRepository {
    db: Arc<Db>,
}

impl SqliteGameBindingRepository {
    pub fn new(db: Arc<Db>) -> Self {
        Self { db }
    }
}

fn map_row(row: &Row<'_>) -> rusqlite::Result<GameBindingIndex> {
    Ok(GameBindingIndex {
        item_id: row.get(0)?,
        exe_path: row.get(1)?,
        last_launched_at: row.get(2)?,
        last_pid: row.get(3)?,
    })
}

impl GameBindingRepository for SqliteGameBindingRepository {
    fn upsert(&self, binding: &GameBindingIndex) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO game_binding_index (item_id, exe_path, last_launched_at, last_pid)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(item_id) DO UPDATE SET
                   exe_path = excluded.exe_path,
                   last_launched_at = excluded.last_launched_at,
                   last_pid = excluded.last_pid",
                params![
                    binding.item_id,
                    binding.exe_path,
                    binding.last_launched_at,
                    binding.last_pid
                ],
            )?;
            Ok(())
        })
    }

    fn get(&self, item_id: &str) -> Result<Option<GameBindingIndex>> {
        self.db.with_conn(|conn| {
            Ok(conn
                .query_row(
                    &format!("SELECT {COLUMNS} FROM game_binding_index WHERE item_id = ?1"),
                    [item_id],
                    map_row,
                )
                .optional()?)
        })
    }

    fn clear(&self, item_id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            conn.execute(
                "DELETE FROM game_binding_index WHERE item_id = ?1",
                [item_id],
            )?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ItemKind, NewItem, UpsertOutcome};
    use crate::repo::{LibraryRepository, SqliteRepository};

    fn setup() -> (SqliteGameBindingRepository, String) {
        let db = Arc::new(Db::open_in_memory().expect("db"));
        db.migrate().expect("migrate");
        let repo = SqliteRepository::new(Arc::clone(&db));
        let id = match repo
            .upsert_scanned(&NewItem {
                kind: ItemKind::Game,
                title: "Game".into(),
                folder_path: "D:/games/game".into(),
                file_filter: None,
            })
            .expect("item")
        {
            UpsertOutcome::Created(item) => item.id,
            other => panic!("期望 Created，实际 {other:?}"),
        };
        (SqliteGameBindingRepository::new(db), id)
    }

    #[test]
    fn upsert_and_overwrite() {
        let (bindings, id) = setup();
        bindings
            .upsert(&GameBindingIndex {
                item_id: id.clone(),
                exe_path: "Game.exe".into(),
                last_launched_at: None,
                last_pid: None,
            })
            .expect("insert");
        let loaded = bindings.get(&id).expect("get").expect("exists");
        assert_eq!(loaded.exe_path, "Game.exe");

        bindings
            .upsert(&GameBindingIndex {
                item_id: id.clone(),
                exe_path: "bin/Game.exe".into(),
                last_launched_at: Some("2026-10-29T10:00:00Z".into()),
                last_pid: Some(1234),
            })
            .expect("update");
        let loaded = bindings.get(&id).expect("get").expect("exists");
        assert_eq!(loaded.exe_path, "bin/Game.exe");
        assert_eq!(loaded.last_pid, Some(1234));
    }

    #[test]
    fn clear_removes_row() {
        let (bindings, id) = setup();
        bindings
            .upsert(&GameBindingIndex {
                item_id: id.clone(),
                exe_path: "Game.exe".into(),
                last_launched_at: None,
                last_pid: None,
            })
            .expect("insert");
        bindings.clear(&id).expect("clear");
        assert!(bindings.get(&id).expect("get").is_none());
    }
}
