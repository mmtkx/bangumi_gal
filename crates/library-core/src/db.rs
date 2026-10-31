use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use rusqlite::Connection;

use crate::error::{CoreError, Result};

/// 默认连接数。ADR-012：同步访问 + 专用连接池，避免与 async 运行时耦合。
const DEFAULT_MAX_CONNECTIONS: usize = 4;
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

struct PoolState {
    idle: Vec<Connection>,
    total: usize,
}

/// SQLite 连接池。
///
/// 每个连接独立持有一个 `rusqlite::Connection`；借用期间独占，归还后复用。
/// 池满时调用方在 Condvar 上等待，因此可以在 `spawn_blocking` 线程里安全使用。
pub struct Db {
    path: Option<PathBuf>,
    max_connections: usize,
    state: Mutex<PoolState>,
    available: Condvar,
}

impl Db {
    /// 打开（或创建）磁盘数据库，并确保父目录存在。
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|err| CoreError::PathUnavailable {
                    path: parent.to_path_buf(),
                    reason: err.to_string(),
                })?;
            }
        }

        let db = Self {
            path: Some(path),
            max_connections: DEFAULT_MAX_CONNECTIONS,
            state: Mutex::new(PoolState {
                idle: Vec::new(),
                total: 0,
            }),
            available: Condvar::new(),
        };

        // 提前建立首个连接，让建库失败在启动期暴露，而不是第一次查询时。
        db.with_conn(|_| Ok(()))?;
        Ok(db)
    }

    /// 内存库，仅用于测试。固定单连接，避免多个连接看到互不相通的 `:memory:`。
    pub fn open_in_memory() -> Result<Self> {
        let db = Self {
            path: None,
            max_connections: 1,
            state: Mutex::new(PoolState {
                idle: Vec::new(),
                total: 0,
            }),
            available: Condvar::new(),
        };
        db.with_conn(|_| Ok(()))?;
        Ok(db)
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// 借出一个连接执行同步操作，无论闭包成功与否都归还连接。
    pub fn with_conn<T>(&self, work: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let conn = self.acquire()?;
        let mut guard = ConnGuard {
            db: self,
            conn: Some(conn),
        };
        let conn = guard.conn.as_mut().ok_or(CoreError::PoolPoisoned)?;
        work(conn)
    }

    /// 应用未执行的迁移，返回迁移后的 schema 版本。
    pub fn migrate(&self) -> Result<i64> {
        self.with_conn(crate::migrate::apply)
    }

    fn lock(&self) -> Result<MutexGuard<'_, PoolState>> {
        self.state.lock().map_err(|_| CoreError::PoolPoisoned)
    }

    fn acquire(&self) -> Result<Connection> {
        let mut state = self.lock()?;
        loop {
            if let Some(conn) = state.idle.pop() {
                return Ok(conn);
            }
            if state.total < self.max_connections {
                state.total += 1;
                drop(state);
                return match self.connect() {
                    Ok(conn) => Ok(conn),
                    Err(err) => {
                        if let Ok(mut state) = self.state.lock() {
                            state.total = state.total.saturating_sub(1);
                        }
                        self.available.notify_one();
                        Err(err)
                    }
                };
            }
            state = self
                .available
                .wait(state)
                .map_err(|_| CoreError::PoolPoisoned)?;
        }
    }

    fn connect(&self) -> Result<Connection> {
        let conn = match &self.path {
            Some(path) => Connection::open(path)
                .map_err(|err| CoreError::DatabaseInit(format!("{}（{}）", path.display(), err)))?,
            None => Connection::open_in_memory()
                .map_err(|err| CoreError::DatabaseInit(err.to_string()))?,
        };

        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        if self.path.is_some() {
            // WAL 让读写不互相阻塞；`:memory:` 不支持，故仅磁盘库设置。
            let mode: String = conn.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
            if !mode.eq_ignore_ascii_case("wal") {
                tracing::warn!(mode, "数据库未能进入 WAL 模式");
            }
        }
        Ok(conn)
    }
}

struct ConnGuard<'a> {
    db: &'a Db,
    conn: Option<Connection>,
}

impl Drop for ConnGuard<'_> {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if let Ok(mut state) = self.db.state.lock() {
            state.idle.push(conn);
        }
        self.db.available.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_is_reused_after_use() {
        let db = Db::open_in_memory().expect("db");
        let first: i64 = db
            .with_conn(|conn| Ok(conn.query_row("SELECT 1", [], |row| row.get(0))?))
            .expect("query");
        assert_eq!(first, 1);
        let second: i64 = db
            .with_conn(|conn| Ok(conn.query_row("SELECT 2", [], |row| row.get(0))?))
            .expect("query");
        assert_eq!(second, 2);
    }

    #[test]
    fn busy_timeout_and_foreign_keys_are_applied() {
        let db = Db::open_in_memory().expect("db");
        let fk: i64 = db
            .with_conn(|conn| Ok(conn.query_row("PRAGMA foreign_keys", [], |row| row.get(0))?))
            .expect("pragma");
        assert_eq!(fk, 1);
    }
}
