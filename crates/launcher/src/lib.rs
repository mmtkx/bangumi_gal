//! 游戏启动、绑定文件与进程树监控。
//!
//! 依赖规则：只依赖 library-core（事件与仓储 trait），不依赖任何其他功能 crate。
//! 可执行文件绑定的事实来源是条目文件夹内的 .gamelauncher.json（ADR-003），
//! 数据库 game_binding_index 只是查询索引。

mod config;
mod error;
mod process;

pub use config::{normalize_exe_path, GameLaunchConfig, BINDING_FILE_NAME};
pub use error::{LauncherError, Result};
pub use process::{process_table, reachable};

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use library_core::events::EventPublisher;
use library_core::model::{GameBindingIndex, ItemKind, LibraryItem};
use library_core::{AppEvent, GameBindingRepository};
use serde::Serialize;

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// 正在运行的游戏，向前端展示的最小信息。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunningGame {
    pub item_id: String,
    pub title: String,
    pub pid: u32,
    pub exe_path: String,
    pub started_at: String,
}

struct RunningState {
    game: RunningGame,
    /// 已观测到的进程树节点，用于跟踪「启动器退出但游戏仍在」的情况。
    tracked: Vec<u32>,
}

/// 启动器。运行状态只存在内存：重启应用后按当前进程表重新判定，不持久化。
pub struct Launcher {
    bus: Arc<dyn EventPublisher>,
    bindings: Arc<dyn GameBindingRepository>,
    running: Mutex<HashMap<String, RunningState>>,
    poll_interval: Duration,
}

impl Launcher {
    pub fn new(bus: Arc<dyn EventPublisher>, bindings: Arc<dyn GameBindingRepository>) -> Self {
        Self {
            bus,
            bindings,
            running: Mutex::new(HashMap::new()),
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    /// 仅供测试或调试缩短轮询间隔。
    pub fn with_poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// 启动游戏。未绑定可执行文件时返回 `binding_missing`，由前端引导选择 exe。
    pub fn start(&self, item: &LibraryItem) -> Result<RunningGame> {
        if item.kind != ItemKind::Game {
            return Err(LauncherError::NotAGame(item.id.clone()));
        }
        if let Some(pid) = self.running_pid(&item.id)? {
            return Err(LauncherError::AlreadyRunning(pid));
        }

        let folder = PathBuf::from(&item.folder_path);
        let mut config = GameLaunchConfig::read(&folder)?.ok_or(LauncherError::BindingMissing)?;
        let exe = config.validate(&folder)?;
        let working_dir = config.resolved_working_dir(&folder);

        let pid = process::spawn(&exe, &config.args, &working_dir)?;
        let started_at = library_core::time::now_iso8601();

        config.last_launched_at = Some(started_at.clone());
        config.last_pid = Some(pid);
        // 进程已经起来了，写回失败不应让调用方误以为启动失败；下次启动会重写。
        if let Err(err) = config.write(&folder) {
            tracing::warn!(error = %err, path = %folder.display(), "回写 .gamelauncher.json 失败");
        }
        if let Err(err) = self.bindings.upsert(&GameBindingIndex {
            item_id: item.id.clone(),
            exe_path: config.exe_path.clone(),
            last_launched_at: Some(started_at.clone()),
            last_pid: Some(i64::from(pid)),
        }) {
            tracing::warn!(error = %err, item = %item.id, "写入 game_binding_index 失败");
        }

        let game = RunningGame {
            item_id: item.id.clone(),
            title: item.title.clone(),
            pid,
            exe_path: exe.to_string_lossy().to_string(),
            started_at,
        };

        {
            let mut guard = self
                .running
                .lock()
                .map_err(|_| LauncherError::LockPoisoned)?;
            guard.insert(
                item.id.clone(),
                RunningState {
                    game: game.clone(),
                    tracked: vec![pid],
                },
            );
        }

        self.bus.publish(AppEvent::LauncherGameStarted {
            item_id: item.id.clone(),
            exe_path: game.exe_path.clone(),
            pid,
        });
        tracing::info!(item = %item.id, pid, exe = %game.exe_path, "游戏已启动");
        Ok(game)
    }

    /// 绑定（或修改）可执行文件。修改后立即生效：每次启动都重新读取绑定文件。
    pub fn set_binding(&self, item: &LibraryItem, exe_path: &str) -> Result<GameLaunchConfig> {
        if item.kind != ItemKind::Game {
            return Err(LauncherError::NotAGame(item.id.clone()));
        }
        let folder = PathBuf::from(&item.folder_path);
        let picked = Path::new(exe_path);
        if !picked.is_file() {
            return Err(LauncherError::ExeNotFound(picked.to_path_buf()));
        }

        let stored = normalize_exe_path(&folder, picked);
        let config = GameLaunchConfig::new(stored.clone());
        config.validate(&folder)?;
        config.write(&folder)?;
        self.bindings.upsert(&GameBindingIndex {
            item_id: item.id.clone(),
            exe_path: stored,
            last_launched_at: None,
            last_pid: None,
        })?;

        tracing::info!(item = %item.id, exe = %config.exe_path, "可执行文件绑定已更新");
        Ok(config)
    }

    pub fn binding(&self, item: &LibraryItem) -> Result<Option<GameLaunchConfig>> {
        GameLaunchConfig::read(&PathBuf::from(&item.folder_path))
    }

    pub fn running(&self) -> Result<Vec<RunningGame>> {
        let guard = self
            .running
            .lock()
            .map_err(|_| LauncherError::LockPoisoned)?;
        let mut games: Vec<RunningGame> = guard.values().map(|state| state.game.clone()).collect();
        games.sort_by(|left, right| left.item_id.cmp(&right.item_id));
        Ok(games)
    }

    pub fn running_pid(&self, item_id: &str) -> Result<Option<u32>> {
        let guard = self
            .running
            .lock()
            .map_err(|_| LauncherError::LockPoisoned)?;
        Ok(guard.get(item_id).map(|state| state.game.pid))
    }

    /// 轮询一次进程树，返回本次判定为退出的游戏。
    pub fn check_once(&self) -> Result<Vec<RunningGame>> {
        let Some(table) = process::process_table() else {
            tracing::warn!("无法获取进程表，跳过本轮游戏状态检查");
            return Ok(Vec::new());
        };
        let mut exited = Vec::new();

        {
            let mut guard = self
                .running
                .lock()
                .map_err(|_| LauncherError::LockPoisoned)?;
            let mut finished = Vec::new();
            for (item_id, state) in guard.iter_mut() {
                let alive = process::reachable(&state.tracked, &table);
                if alive.is_empty() {
                    finished.push(item_id.clone());
                } else {
                    state.tracked = alive;
                }
            }
            for item_id in finished {
                if let Some(state) = guard.remove(&item_id) {
                    exited.push(state.game);
                }
            }
        }

        for game in &exited {
            self.bus.publish(AppEvent::LauncherGameExited {
                item_id: game.item_id.clone(),
                pid: game.pid,
            });
            tracing::info!(item = %game.item_id, pid = game.pid, "游戏已退出");
        }
        Ok(exited)
    }

    /// 常驻轮询循环。
    ///
    /// 本 crate 不依赖 Tauri，因此不自己 spawn：调用方负责在可用的运行时上启动
    /// （app-services 用 `tauri::async_runtime::spawn`）。
    pub async fn run_monitor(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(self.poll_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(err) = self.check_once() {
                tracing::warn!(error = %err, "游戏进程轮询失败");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_core::binding::SqliteGameBindingRepository;
    use library_core::db::Db;
    use library_core::events::NoopPublisher;
    use library_core::model::{NewItem, UpsertOutcome};
    use library_core::repo::{LibraryRepository, SqliteRepository};
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lagm-launch-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn setup(folder: &Path) -> (Launcher, LibraryItem) {
        let db = Arc::new(Db::open_in_memory().expect("db"));
        db.migrate().expect("migrate");
        let repo = SqliteRepository::new(Arc::clone(&db));
        let item = match repo
            .upsert_scanned(&NewItem {
                kind: ItemKind::Game,
                title: "Test Game".into(),
                folder_path: folder.to_string_lossy().to_string(),
                file_filter: None,
            })
            .expect("item")
        {
            UpsertOutcome::Created(item) => item,
            other => panic!("期望 Created，实际 {other:?}"),
        };
        let bindings = Arc::new(SqliteGameBindingRepository::new(db));
        let launcher = Launcher::new(Arc::new(NoopPublisher), bindings);
        (launcher, item)
    }

    #[test]
    fn start_without_binding_reports_missing() {
        let dir = temp_dir("missing");
        let (launcher, item) = setup(&dir);
        let err = launcher.start(&item).expect_err("应当要求先绑定");
        assert_eq!(err.code(), "binding_missing");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn start_rejects_non_game_item() {
        let dir = temp_dir("kind");
        let (launcher, mut item) = setup(&dir);
        item.kind = ItemKind::Anime;
        assert_eq!(
            launcher.start(&item).expect_err("动画不能启动").code(),
            "not_a_game"
        );
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn set_binding_writes_file_and_index() {
        let dir = temp_dir("bind");
        let exe = dir.join("game.exe");
        fs::write(&exe, b"not a real exe").expect("exe");
        let (launcher, item) = setup(&dir);

        let config = launcher
            .set_binding(&item, &exe.to_string_lossy())
            .expect("bind");
        assert_eq!(config.exe_path, "game.exe");
        assert!(GameLaunchConfig::binding_path(&dir).is_file());

        let loaded = launcher.binding(&item).expect("read").expect("exists");
        assert_eq!(loaded.exe_path, "game.exe");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn set_binding_rejects_missing_file() {
        let dir = temp_dir("bind-missing");
        let (launcher, item) = setup(&dir);
        let err = launcher
            .set_binding(&item, &dir.join("nope.exe").to_string_lossy())
            .expect_err("应当拒绝");
        assert_eq!(err.code(), "exe_not_found");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn running_is_empty_before_start() {
        let dir = temp_dir("idle");
        let (launcher, item) = setup(&dir);
        assert!(launcher.running().expect("running").is_empty());
        assert!(launcher.running_pid(&item.id).expect("pid").is_none());
        fs::remove_dir_all(dir).ok();
    }

    /// 真实拉起一个短命进程，验证「退出后自动变为未运行」这条链路。
    #[cfg(windows)]
    #[test]
    fn detects_real_process_exit() {
        let dir = temp_dir("exit");
        let (launcher, item) = setup(&dir);
        let mut config = GameLaunchConfig::new("C:\\Windows\\System32\\cmd.exe");
        config.args = vec!["/C".into(), "exit".into()];
        config.write(&dir).expect("write config");

        let game = launcher.start(&item).expect("start");
        assert!(game.pid > 0);
        assert_eq!(launcher.running().expect("running").len(), 1);

        let mut exited = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            exited = launcher.check_once().expect("tick");
            if !exited.is_empty() {
                break;
            }
        }
        assert_eq!(exited.len(), 1, "进程退出后应被判定为未运行");
        assert!(launcher.running().expect("running").is_empty());
        fs::remove_dir_all(dir).ok();
    }
}
