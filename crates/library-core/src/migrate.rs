use rusqlite::Connection;

use crate::error::Result;

/// 一条入库的迁移脚本。版本号必须严格递增且不可回改。
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// 迁移脚本随仓库版本管理，编译期嵌入二进制，运行期不依赖外部文件。
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "0001_init",
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "0002_collection",
        sql: include_str!("../migrations/0002_collection.sql"),
    },
    Migration {
        version: 3,
        name: "0003_loose_files",
        sql: include_str!("../migrations/0003_loose_files.sql"),
    },
];

/// 读取当前 schema 版本（`PRAGMA user_version`）。
pub fn current_version(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

/// 幂等应用所有未执行迁移，返回最终版本。
///
/// 每条迁移在独立事务中执行，失败整体回滚，不会留下半套表。
pub fn apply(conn: &mut Connection) -> Result<i64> {
    let mut version = current_version(conn)?;
    // 先收集待执行项，避免在循环内改动 version 时与迭代器借用冲突。
    let pending: Vec<&Migration> = MIGRATIONS
        .iter()
        .filter(|migration| migration.version > version)
        .collect();

    for migration in pending {
        let tx = conn.transaction()?;
        tx.execute_batch(migration.sql)?;
        tx.pragma_update(None, "user_version", migration.version)?;
        tx.commit()?;
        tracing::info!(
            version = migration.version,
            name = migration.name,
            "已应用数据库迁移"
        );
        version = migration.version;
    }
    Ok(version)
}

#[cfg(test)]
mod tests {
    use crate::db::Db;

    #[test]
    fn migration_is_repeatable_and_keeps_data() {
        let db = Db::open_in_memory().expect("db");
        assert_eq!(db.migrate().expect("first"), 3);
        assert_eq!(db.migrate().expect("second"), 3);

        db.with_conn(|conn| {
            conn.execute(
                "INSERT INTO app_settings (key, value) VALUES ('k', 'v')",
                [],
            )?;
            Ok(())
        })
        .expect("insert");

        assert_eq!(db.migrate().expect("third"), 3);
        let value: String = db
            .with_conn(|conn| {
                Ok(conn.query_row("SELECT value FROM app_settings", [], |r| r.get(0))?)
            })
            .expect("select");
        assert_eq!(value, "v");
    }

    #[test]
    fn all_plan_tables_exist() {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        let expected = [
            "library_item",
            "progress",
            "game_binding_index",
            "sync_queue",
            "conflict",
            "metadata_index",
            "plugin_registry",
            "app_settings",
        ];
        for table in expected {
            let count: i64 = db
                .with_conn(|conn| {
                    Ok(conn.query_row(
                        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                        [table],
                        |row| row.get(0),
                    )?)
                })
                .expect("count");
            assert_eq!(count, 1, "缺少表 {table}");
        }
    }

    #[test]
    fn loose_file_scope_allows_multiple_items_per_folder() {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");

        let insert = |conn: &rusqlite::Connection, id: &str, filter: &str| {
            conn.execute(
                "INSERT INTO library_item (id, kind, title, folder_path, file_filter, created_at, updated_at)
                 VALUES (?1, 'anime', ?1, 'D:/root', ?2, 't', 't')",
                rusqlite::params![id, filter],
            )
        };

        // 同一目录下按 file_filter 拆成多个条目是允许的（根目录松散文件）。
        db.with_conn(|conn| {
            insert(conn, "a", "kaguya")?;
            insert(conn, "b", "other")?;
            Ok(0i64)
        })
        .expect("同目录不同分组应允许");

        // 但同一目录 + 同一分组只能有一个条目。
        let duplicated = db
            .with_conn(|conn| {
                insert(conn, "c", "kaguya")?;
                Ok(0i64)
            })
            .is_err();
        assert!(duplicated, "同目录同分组应被唯一索引拒绝");
    }

    #[test]
    fn kind_check_constraint_rejects_unknown_kind() {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        let result = db.with_conn(|conn| {
            Ok(conn.execute(
                "INSERT INTO library_item (id, kind, title, folder_path, created_at, updated_at)
                 VALUES ('1', 'movie', 't', 'p', 'now', 'now')",
                [],
            )?)
        });
        assert!(result.is_err());
    }

    #[test]
    fn collection_columns_exist_from_0002() {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        let count: i64 = db
            .with_conn(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM pragma_table_info('library_item') \
                     WHERE name IN ('collection_status', 'rating', 'collection_updated_at')",
                    [],
                    |row| row.get(0),
                )?)
            })
            .expect("count");
        assert_eq!(count, 3, "0002 迁移应新增 3 列");
    }
}
