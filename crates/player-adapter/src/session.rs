use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use library_core::events::EventPublisher;
use library_core::model::ProgressSource;
use library_core::{AppEvent, ProgressRepository};
use sysinfo::{ProcessesToUpdate, System};

use crate::backend::{ActiveSession, PlayerBackend, PlayingSession};
use crate::error::{PlayerError, Result};

/// 进度写库与进程检查周期。1 秒足够让页面上的进度基本跟手；
/// 写入只发生在位置真正变化时，暂停或静止不会产生重复写库。
const DEFAULT_TICK: Duration = Duration::from_secs(1);

struct TrackedSession {
    item_id: String,
    episode: u32,
    file_name: String,
    uri: String,
    started: Instant,
    started_at: String,
    /// 本次会话的续播起点。写入进度时是「起点 + 已播时长」。
    start_seconds: u64,
    /// 起点是否真的生效；结束事件里也要如实反映。
    resume_applied: bool,
    /// 上一轮读到的播放器真实位置，进程消失后用它收尾（估算值在拖动过进度条后不可信）。
    last_reported: Option<u64>,
    /// 上一次写进数据库的进度值，未变化时跳过写库与事件。
    last_written: Option<u64>,
}

/// 决定这一轮写入的进度值（会话仍在运行）。
///
/// - 播放器能上报位置（PotPlayer）：以它为准，拖动进度条、跳转都能反映；
/// - 上报 0 但此前有位置：多为播放结束 / 用户停止，保留最后一次已知位置；
/// - 启动后 3 秒内上报 0：播放器可能还没执行 `/seek`，保留续播起点；
/// - 上报为 None（其他外部播放器）：退回「起点 + 本次时长」估算。
fn resolve_position(
    start_seconds: u64,
    elapsed_seconds: u64,
    reported: Option<u64>,
    last_reported: Option<u64>,
) -> u64 {
    match reported {
        Some(0) if last_reported.unwrap_or(0) > 0 => last_reported.unwrap_or(0),
        Some(position) if position < start_seconds && elapsed_seconds < 3 => start_seconds,
        Some(position) => position,
        None => start_seconds.saturating_add(elapsed_seconds),
    }
}

/// 进度是否有变化：没变就不重复写库、不重复发事件。
fn progress_changed(last_written: Option<u64>, position: u64) -> bool {
    last_written != Some(position)
}

/// 会话结束时的最终进度。
///
/// 播放器进程已退出时读不到位置，此时不能直接用以时长估算的值：
/// 用户拖动过进度条的话估算值会差很多。优先用上一轮读到的真实位置，
/// 只有当估算值落在「上一次真实位置之后 10 秒内」才认为播放是连续推进的。
fn finalize_position(
    start_seconds: u64,
    elapsed_seconds: u64,
    reported: Option<u64>,
    last_reported: Option<u64>,
) -> u64 {
    if reported.is_some() {
        return resolve_position(start_seconds, elapsed_seconds, reported, last_reported);
    }
    let estimate = start_seconds.saturating_add(elapsed_seconds);
    match last_reported {
        Some(last) if estimate < last || estimate > last.saturating_add(10) => last,
        Some(_) => estimate,
        None => estimate,
    }
}

/// 轮询时对会话做的只读快照，避免持锁访问进程表。
struct SessionSnapshot {
    pid: u32,
    item_id: String,
    episode: u32,
    file_name: String,
    uri: String,
    elapsed_seconds: u64,
    started_at: String,
    start_seconds: u64,
    /// 起点是否真的生效；结束事件里也要如实反映。
    resume_applied: bool,
    last_reported: Option<u64>,
    last_written: Option<u64>,
}

/// 播放门面：启动播放器 → 会话计时 → 周期写入 progress → 结束时收尾并广播事件。
///
/// 依赖规则：只依赖 library-core 的 ProgressRepository / EventPublisher trait。
pub struct Player {
    /// 后端可替换：用户在设置里换了播放器后无需重建 Player（会话状态不能丢）。
    backend: Mutex<Arc<dyn PlayerBackend>>,
    progress: Arc<dyn ProgressRepository>,
    bus: Arc<dyn EventPublisher>,
    sessions: Mutex<HashMap<u32, TrackedSession>>,
    tick: Duration,
}

impl Player {
    pub fn new(
        backend: Arc<dyn PlayerBackend>,
        progress: Arc<dyn ProgressRepository>,
        bus: Arc<dyn EventPublisher>,
    ) -> Arc<Self> {
        Self::new_with_tick(backend, progress, bus, DEFAULT_TICK)
    }

    /// 指定轮询周期的构造入口，测试用短周期即可，不必等到 5 秒。
    pub fn new_with_tick(
        backend: Arc<dyn PlayerBackend>,
        progress: Arc<dyn ProgressRepository>,
        bus: Arc<dyn EventPublisher>,
        tick: Duration,
    ) -> Arc<Self> {
        Arc::new(Self {
            backend: Mutex::new(backend),
            progress,
            bus,
            sessions: Mutex::new(HashMap::new()),
            tick,
        })
    }

    /// 替换播放后端（用户在设置里改了播放器路径或参数时调用）。
    pub fn set_backend(&self, backend: Arc<dyn PlayerBackend>) -> Result<()> {
        let mut guard = self.backend.lock().map_err(|_| PlayerError::LockPoisoned)?;
        *guard = backend;
        Ok(())
    }

    fn current_backend(&self) -> Result<Arc<dyn PlayerBackend>> {
        let guard = self.backend.lock().map_err(|_| PlayerError::LockPoisoned)?;
        Ok(Arc::clone(&guard))
    }

    pub fn backend_name(&self) -> Result<&'static str> {
        Ok(self.current_backend()?.name())
    }

    /// 播放指定文件。
    ///
    /// - `start_seconds` 为断点续播位置，由调用方从 progress 表读出后传入；
    /// - 进度写入「起点 + 本次会话时长」，因此手动校正过的值不会被清零覆盖。
    pub fn play(
        &self,
        item_id: &str,
        episode: u32,
        file_name: &str,
        uri: &Path,
        start_seconds: u64,
    ) -> Result<PlayingSession> {
        let backend = self.current_backend()?;
        let pid = backend.play(uri, start_seconds)?;
        // 传了起点不代表播放器真的跳过去了（模板没 {seconds}、或系统默认播放器不支持）。
        let resume_applied = start_seconds > 0 && backend.resume_applied();
        let started_at = library_core::time::now_iso8601();

        {
            let mut guard = self
                .sessions
                .lock()
                .map_err(|_| PlayerError::LockPoisoned)?;
            guard.insert(
                pid,
                TrackedSession {
                    item_id: item_id.to_owned(),
                    episode,
                    file_name: file_name.to_owned(),
                    uri: uri.to_string_lossy().to_string(),
                    started: Instant::now(),
                    started_at: started_at.clone(),
                    start_seconds,
                    resume_applied,
                    last_reported: None,
                    // play() 已经把起点写进库，这里同步记录，避免下一轮重复写。
                    last_written: Some(start_seconds),
                },
            );
        }

        self.bus.publish(AppEvent::PlayerSessionStarted {
            item_id: item_id.to_owned(),
            episode,
            file_name: file_name.to_owned(),
            pid,
        });
        // 写回起点：新集创建 0 秒记录，已有进度的集保持原值（不会被清零）。
        self.write_progress(item_id, episode, start_seconds);
        tracing::info!(
            item = item_id,
            episode,
            pid,
            file = file_name,
            start_seconds,
            resume_applied,
            "播放会话开始"
        );

        Ok(PlayingSession {
            item_id: item_id.to_owned(),
            episode,
            file_name: file_name.to_owned(),
            uri: uri.to_string_lossy().to_string(),
            pid,
            started_at,
            start_seconds,
            resume_applied,
        })
    }

    /// 手动校正进度时调用：把运行中会话的起点重置为校正值。
    ///
    /// 否则下一轮轮询会用「旧起点 + 时长」把用户刚改的值覆盖掉。
    pub fn rebase(&self, item_id: &str, episode: u32, seconds: u64) -> Result<()> {
        let mut guard = self
            .sessions
            .lock()
            .map_err(|_| PlayerError::LockPoisoned)?;
        for session in guard.values_mut() {
            if session.item_id == item_id && session.episode == episode {
                session.start_seconds = seconds;
                session.started = Instant::now();
            }
        }
        Ok(())
    }

    pub fn active(&self) -> Result<Vec<ActiveSession>> {
        let backend = self.current_backend()?;
        let snapshot: Vec<SessionSnapshot> = {
            let guard = self
                .sessions
                .lock()
                .map_err(|_| PlayerError::LockPoisoned)?;
            guard
                .iter()
                .map(|(pid, session)| SessionSnapshot {
                    pid: *pid,
                    item_id: session.item_id.clone(),
                    episode: session.episode,
                    file_name: session.file_name.clone(),
                    uri: session.uri.clone(),
                    elapsed_seconds: session.started.elapsed().as_secs(),
                    started_at: session.started_at.clone(),
                    start_seconds: session.start_seconds,
                    resume_applied: session.resume_applied,
                    last_reported: session.last_reported,
                    last_written: session.last_written,
                })
                .collect()
        };

        let mut sessions: Vec<ActiveSession> = snapshot
            .into_iter()
            .map(|session| {
                let reported = backend.get_position(session.pid).ok().flatten();
                ActiveSession {
                    item_id: session.item_id,
                    episode: session.episode,
                    file_name: session.file_name,
                    pid: session.pid,
                    started_at: session.started_at,
                    position_seconds: resolve_position(
                        session.start_seconds,
                        session.elapsed_seconds,
                        reported,
                        session.last_reported,
                    ),
                    start_seconds: session.start_seconds,
                }
            })
            .collect();
        sessions.sort_by(|left, right| left.started_at.cmp(&right.started_at));
        Ok(sessions)
    }

    /// 轮询一次：存活会话推进进度，已退出的会话写最终进度并广播结束事件。
    pub fn check_once(&self) -> Result<Vec<PlayingSession>> {
        let backend = self.current_backend()?;
        let Some(table) = process_table() else {
            tracing::warn!("无法获取进程表，跳过本轮播放会话检查");
            return Ok(Vec::new());
        };

        let snapshot: Vec<SessionSnapshot> = {
            let guard = self
                .sessions
                .lock()
                .map_err(|_| PlayerError::LockPoisoned)?;
            guard
                .iter()
                .map(|(pid, session)| SessionSnapshot {
                    pid: *pid,
                    item_id: session.item_id.clone(),
                    episode: session.episode,
                    file_name: session.file_name.clone(),
                    uri: session.uri.clone(),
                    elapsed_seconds: session.started.elapsed().as_secs(),
                    started_at: session.started_at.clone(),
                    start_seconds: session.start_seconds,
                    resume_applied: session.resume_applied,
                    last_reported: session.last_reported,
                    last_written: session.last_written,
                })
                .collect()
        };

        let mut finished = Vec::new();
        for session in snapshot {
            let SessionSnapshot {
                pid,
                item_id,
                episode,
                file_name,
                uri,
                elapsed_seconds,
                started_at,
                start_seconds,
                resume_applied,
                last_reported,
                last_written,
            } = session;
            let alive = table.contains_key(&pid);
            let reported = backend.get_position(pid).ok().flatten();
            let position = if alive {
                resolve_position(start_seconds, elapsed_seconds, reported, last_reported)
            } else {
                finalize_position(start_seconds, elapsed_seconds, reported, last_reported)
            };

            // 位置没变就不写库、不发事件：暂停或静止时不会每秒刷一次库。
            let changed = progress_changed(last_written, position);
            if changed {
                self.write_progress(&item_id, episode, position);
            }

            if alive {
                if reported.is_some() || changed {
                    let mut guard = self
                        .sessions
                        .lock()
                        .map_err(|_| PlayerError::LockPoisoned)?;
                    if let Some(tracked) = guard.get_mut(&pid) {
                        if let Some(reported) = reported {
                            tracked.last_reported = Some(reported);
                        }
                        if changed {
                            tracked.last_written = Some(position);
                        }
                    }
                }
                if changed {
                    self.bus.publish(AppEvent::PlayerSessionProgress {
                        item_id: item_id.clone(),
                        episode,
                        watched_seconds: position,
                    });
                }
                continue;
            }

            {
                let mut guard = self
                    .sessions
                    .lock()
                    .map_err(|_| PlayerError::LockPoisoned)?;
                guard.remove(&pid);
            }
            self.bus.publish(AppEvent::PlayerSessionFinished {
                item_id: item_id.clone(),
                episode,
                watched_seconds: position,
            });
            tracing::info!(
                item = %item_id,
                episode,
                pid,
                position,
                "播放会话结束"
            );
            finished.push(PlayingSession {
                item_id,
                episode,
                file_name,
                uri,
                pid,
                started_at,
                start_seconds,
                resume_applied,
            });
        }
        Ok(finished)
    }

    /// 常驻轮询循环。由调用方在可用运行时上启动（app-services 用 tauri::async_runtime）。
    pub async fn run_monitor(self: Arc<Self>) {
        let mut ticker = tokio::time::interval(self.tick);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            if let Err(err) = self.check_once() {
                tracing::warn!(error = %err, "播放会话轮询失败");
            }
        }
    }

    fn write_progress(&self, item_id: &str, episode: u32, seconds: u64) {
        if let Err(err) = self
            .progress
            .upsert(item_id, episode, seconds, ProgressSource::Local)
        {
            tracing::warn!(error = %err, item = %item_id, episode, "写入播放进度失败");
        }
    }
}

/// 进程表快照。功能 crate 之间禁止互相依赖，故与 launcher 各保留一份实现。
fn process_table() -> Option<HashMap<u32, Option<u32>>> {
    let mut system = System::new().ok()?;
    system.refresh_processes(ProcessesToUpdate::All, true);
    Some(
        system
            .processes()
            .iter()
            .map(|(pid, process)| (pid.as_u32(), process.parent().map(|parent| parent.as_u32())))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::PlayerState;
    use library_core::db::Db;
    use library_core::events::NoopPublisher;
    use library_core::model::{ItemKind, NewItem, ProgressSource, UpsertOutcome};
    use library_core::repo::{LibraryRepository, SqliteRepository};
    use std::process::{Command, Stdio};
    use std::sync::Arc as StdArc;
    use tokio::sync::broadcast;

    /// 测试用后端：拉起一个进程模拟播放器，避免依赖机器上真实安装的播放器。
    struct FakeBackend {
        states: broadcast::Sender<PlayerState>,
        last_start_seconds: Mutex<Option<u64>>,
        /// 大于 0 时用 ping 拉起一个存活数秒的进程，用来测试会话仍在进行中的分支。
        alive_secs: u32,
        /// 模拟播放器上报的位置；None 表示播放器不支持回传（走时长估算）。
        reported_position: Option<u64>,
    }

    impl FakeBackend {
        fn fast() -> Arc<Self> {
            Self::new(0, None)
        }

        fn slow(alive_secs: u32) -> Arc<Self> {
            Self::new(alive_secs, None)
        }

        fn reporting(position: u64) -> Arc<Self> {
            Self::new(5, Some(position))
        }

        fn new(alive_secs: u32, reported_position: Option<u64>) -> Arc<Self> {
            let (states, _) = broadcast::channel(8);
            Arc::new(Self {
                states,
                last_start_seconds: Mutex::new(None),
                alive_secs,
                reported_position,
            })
        }
    }

    impl PlayerBackend for FakeBackend {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn play(&self, uri: &Path, start_seconds: u64) -> Result<u32> {
            if !uri.is_file() {
                return Err(PlayerError::MediaNotFound(uri.to_path_buf()));
            }
            if let Ok(mut guard) = self.last_start_seconds.lock() {
                *guard = Some(start_seconds);
            }

            let mut command = Command::new("C:\\Windows\\System32\\cmd.exe");
            if self.alive_secs == 0 {
                command.args(["/C", "exit"]);
            } else {
                command
                    .args([
                        "/C",
                        "ping",
                        "-n",
                        &(self.alive_secs + 1).to_string(),
                        "127.0.0.1",
                    ])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null());
            }
            let child = command.spawn().map_err(|err| PlayerError::Spawn {
                path: uri.to_path_buf(),
                reason: err.to_string(),
            })?;
            let _ = self.states.send(PlayerState::Playing);
            Ok(child.id())
        }

        fn pause(&self) -> Result<()> {
            Ok(())
        }

        fn seek(&self, _seconds: u64) -> Result<()> {
            Ok(())
        }

        fn get_position(&self, _pid: u32) -> Result<Option<u64>> {
            Ok(self.reported_position)
        }

        fn on_state_change(&self) -> broadcast::Receiver<PlayerState> {
            self.states.subscribe()
        }
    }

    fn setup() -> (
        Arc<Player>,
        Arc<library_core::progress::SqliteProgressRepository>,
        String,
        Arc<FakeBackend>,
    ) {
        setup_with(FakeBackend::fast())
    }

    fn setup_with(
        backend: Arc<FakeBackend>,
    ) -> (
        Arc<Player>,
        Arc<library_core::progress::SqliteProgressRepository>,
        String,
        Arc<FakeBackend>,
    ) {
        let db = StdArc::new(Db::open_in_memory().expect("db"));
        db.migrate().expect("migrate");
        let repo = SqliteRepository::new(StdArc::clone(&db));
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
        let progress = StdArc::new(library_core::progress::SqliteProgressRepository::new(db));
        let player = Player::new(backend.clone(), progress.clone(), Arc::new(NoopPublisher));
        (player, progress, id, backend)
    }

    fn temp_media(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("lagm-{}-{}", name, std::process::id()));
        std::fs::write(&path, b"x").expect("media");
        path
    }

    #[test]
    fn play_rejects_missing_media() {
        let (player, _progress, id, _backend) = setup();
        let err = player
            .play(&id, 1, "01.mkv", Path::new("D:/nope.mkv"), 0)
            .expect_err("应当拒绝");
        assert_eq!(err.code(), "media_not_found");
    }

    #[cfg(windows)]
    #[test]
    fn play_passes_resume_position_to_backend() {
        let (player, _progress, id, backend) = setup();
        let media = temp_media("resume.mkv");
        player.play(&id, 1, "01.mkv", &media, 600).expect("play");
        assert_eq!(*backend.last_start_seconds.lock().expect("lock"), Some(600));
        std::fs::remove_file(media).ok();
    }

    #[cfg(windows)]
    #[test]
    fn resume_position_is_not_cleared_by_new_session() {
        let (player, progress, id, _backend) = setup();
        let media = temp_media("keep.mkv");
        progress
            .upsert(&id, 1, 600, ProgressSource::Local)
            .expect("seed");

        player.play(&id, 1, "01.mkv", &media, 600).expect("play");
        let stored = progress
            .list_by_item(&id)
            .expect("list")
            .into_iter()
            .find(|row| row.episode == 1)
            .expect("row");
        assert!(stored.watched_seconds >= 600, "不应被清零：{stored:?}");
        std::fs::remove_file(media).ok();
    }

    #[test]
    fn progress_changed_only_when_value_moves() {
        assert!(progress_changed(None, 10));
        assert!(progress_changed(Some(10), 11));
        assert!(!progress_changed(Some(11), 11));
    }

    #[test]
    fn resolve_position_prefers_reported_value() {
        // 播放器上报的位置优先：拖动进度条后即使往前跳也能记录。
        assert_eq!(resolve_position(600, 10, Some(1200), None), 1200);
        // 拖回开头也算数（已过启动宽限期）。
        assert_eq!(resolve_position(600, 10, Some(5), None), 5);
        // 没有回传通道时退回估算。
        assert_eq!(resolve_position(600, 30, None, None), 630);
        // 启动 3 秒内上报 0：保留续播起点，避免刚续播就被写成 0。
        assert_eq!(resolve_position(600, 1, Some(0), None), 600);
        // 从 0 开始时正常采纳上报值。
        assert_eq!(resolve_position(0, 1, Some(3), None), 3);
    }

    #[test]
    fn finalize_position_prefers_last_reported_when_process_is_gone() {
        // 进程已退出、读不到位置：拖动过进度条时估算值不可信，用最后一次真实位置。
        assert_eq!(finalize_position(0, 30, None, Some(1200)), 1200);
        // 正常推进：估算值与最后一次真实位置相差在 10 秒内，采用估算值补齐尾巴。
        assert_eq!(finalize_position(600, 32, None, Some(630)), 632);
        // 从未读到过位置：只能退回估算。
        assert_eq!(finalize_position(600, 30, None, None), 630);
        // 进程还活着时直接以它为准。
        assert_eq!(finalize_position(0, 30, Some(900), Some(800)), 900);
    }

    #[cfg(windows)]
    #[test]
    fn reported_position_overrides_estimate() {
        let (player, progress, id, _backend) = setup_with(FakeBackend::reporting(1200));
        let media = temp_media("reported.mkv");
        player.play(&id, 1, "01.mkv", &media, 0).expect("play");
        player.check_once().expect("tick");

        let stored = progress
            .list_by_item(&id)
            .expect("list")
            .into_iter()
            .find(|row| row.episode == 1)
            .expect("row");
        assert_eq!(stored.watched_seconds, 1200, "应写入播放器上报的真实位置");
        std::fs::remove_file(media).ok();
    }

    #[cfg(windows)]
    #[test]
    fn rebase_keeps_manual_correction_while_playing() {
        let (player, progress, id, _backend) = setup_with(FakeBackend::slow(5));
        let media = temp_media("rebase.mkv");
        player.play(&id, 2, "02.mkv", &media, 0).expect("play");

        // 会话仍在运行时手动校正到 900 秒，后续轮询不能把它写回 0。
        player.rebase(&id, 2, 900).expect("rebase");
        player.check_once().expect("tick");

        let stored = progress
            .list_by_item(&id)
            .expect("list")
            .into_iter()
            .find(|row| row.episode == 2)
            .expect("row");
        assert!(
            stored.watched_seconds >= 900,
            "播放中的校正不应被覆盖：{stored:?}"
        );
        std::fs::remove_file(media).ok();
    }

    #[cfg(windows)]
    #[test]
    fn session_finishes_and_writes_position_after_start() {
        let (player, progress, id, _backend) = setup();
        let media = temp_media("finish.mkv");
        player.play(&id, 1, "01.mkv", &media, 600).expect("play");

        let mut finished = Vec::new();
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            finished = player.check_once().expect("tick");
            if !finished.is_empty() {
                break;
            }
        }
        assert_eq!(finished.len(), 1);
        assert!(player.active().expect("active").is_empty());

        let stored = progress
            .list_by_item(&id)
            .expect("list")
            .into_iter()
            .find(|row| row.episode == 1)
            .expect("progress row");
        assert_eq!(stored.source, "local");
        assert!(
            stored.watched_seconds >= 600,
            "最终进度应包含续播起点：{stored:?}"
        );
        std::fs::remove_file(media).ok();
    }
}
