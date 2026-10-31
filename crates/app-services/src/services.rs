use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use bangumi_sync::{
    BangumiClient, BangumiHttpClient, CollectionPayload, InMemoryTokenStore, KeyringTokenStore,
    LocalCollection, LocalProgress, MetadataCache, OAuthConfig, ProgressPayload,
    SyncConfigProvider, SyncEngine, SyncResult as SyncOpResult, TokenStore,
};
use launcher::{Launcher, RunningGame};
use library_core::binding::SqliteGameBindingRepository;
use library_core::db::Db;
use library_core::metadata::{MetadataIndexRepository, SqliteMetadataIndexRepository};
use library_core::model::{ItemKind, LibraryItem, MediaProbe, ProgressSource};
use library_core::plugin::{PluginRegistryRepository, SqlitePluginRegistryRepository};
use library_core::progress::{ProgressRepository, SqliteProgressRepository};
use library_core::repo::{LibraryRepository, SqliteRepository};
use library_core::settings::SqliteSettingsStore;
use library_core::sync::{
    ConflictRepository, SqliteConflictRepository, SqliteSyncQueueRepository, SyncQueueRepository,
};
use library_core::{AppEvent, CoreError, EventBus, GameBindingRepository};
use player_adapter::{
    ActiveSession, ExternalPlayerAdapter, Player, PlayerBackend, PlayerError, PlayingSession,
    SystemDefaultPlayer,
};
use plugin_host::{
    load_manifest, CapabilityRuntime, HostBridge, Permission, PluginHost, PluginKind,
    ValidatedPlugin, MANIFEST_FILE,
};
use scanner_anime::AnimeScanner;
use scanner_game::GameScanner;

use crate::delete::{DeleteMode, FolderRemover, SystemRemover};
use crate::dto::{
    AccountDto, AutoMatchReportDto, BangumiStatusDto, BatchDeleteDto, BatchRefreshDto,
    CacheClearDto, CacheStatsDto, CollectionDto, ConflictDto, DeleteItemDto, GameBindingDto,
    GameStatusDto, ImportReportDto, ImportedItemDto, ItemDetailDto, ItemSummaryDto,
    MediaSummaryDto, MetadataCacheDto, PluginDto, PluginEntryDto, PluginInvocationDto,
    PluginOverviewDto, PreviewRelatedDto, PreviewTagDto, ProgressDto, PullReportDto, PushReportDto,
    ResolveReportDto, ScanSummaryDto, SettingsDto, SubjectPreviewDto, SubjectSearchHitDto,
    SyncQueueDto, SyncStatsDto,
};
use crate::error::{ApiResult, AppError};
use crate::settings::{
    default_metadata_dir, AppSettings, BangumiConfigInput, SettingsInput, SettingsService,
    DEFAULT_BANGUMI_REDIRECT_URI,
};

/// 组合根：按 plan.md 3.1 装配各模块，并作为唯一对外入口。
///
/// 依赖方向恒为 app-services → 功能 crate → library-core；
/// 功能 crate 之间没有任何直接依赖，只通过 library-core 的事件与 trait 协作。
/// 播放后端选择：清空播放器路径时用系统默认关联程序。
#[derive(Debug, Clone, PartialEq, Eq)]
enum PlayerBackendChoice {
    System,
    /// 用户指定的播放器：可执行文件 + 参数模板。
    External(PathBuf, String),
}

pub struct Services {
    db: Arc<Db>,
    bus: Arc<EventBus>,
    repo: Arc<SqliteRepository>,
    progress: Arc<SqliteProgressRepository>,
    sync_queue: Arc<SqliteSyncQueueRepository>,
    conflicts: Arc<SqliteConflictRepository>,
    metadata_index: Arc<SqliteMetadataIndexRepository>,
    client: Arc<dyn BangumiClient>,
    sync_engine: Arc<SyncEngine>,
    token_store: Arc<dyn TokenStore>,
    config_provider: Arc<dyn SyncConfigProvider>,
    settings: SettingsService,
    plugins: Arc<SqlitePluginRegistryRepository>,
    /// 默认插件目录（设置未自定义时使用）。
    default_plugins_dir: PathBuf,
    /// 能力插件的 WASM 沙箱运行时（引擎复用，模块每次调用按需编译）。
    plugin_runtime: CapabilityRuntime,
    anime: AnimeScanner,
    game: GameScanner,
    launcher: Arc<Launcher>,
    player: Arc<Player>,
    /// 当前注入给 player 的播放器路径，用于判断是否需要替换后端。
    player_backend_path: Mutex<Option<PlayerBackendChoice>>,
    /// 目录删除后端；生产用系统回收站，测试注入不写回收站的实现。
    remover: Arc<dyn FolderRemover>,
}

/// 用本地设置实现 OAuth 配置来源，供 bangumi-sync 在刷新 token 时读取。
struct SettingsSyncConfig {
    settings: SettingsService,
}

/// 把 Services 的只读能力与事件总线暴露给 WASM 沙箱。
///
/// 权限判定在 plugin-host 的导入实现里先做一次；这里复用 `plugin_emit`，
/// 由服务层再校验一次权限与事件名，避免插件绕过清单约束。
struct ServicesBridge {
    services: Arc<Services>,
    id: String,
}

impl HostBridge for ServicesBridge {
    fn read_items(&self) -> Result<String, String> {
        let items = self
            .services
            .list_items(None)
            .map_err(|err| err.to_string())?;
        serde_json::to_string(&items).map_err(|err| err.to_string())
    }

    fn emit(&self, name: &str, payload: &str) -> Result<(), String> {
        let value: serde_json::Value =
            serde_json::from_str(payload).map_err(|err| err.to_string())?;
        self.services
            .plugin_emit(&self.id, name, value)
            .map_err(|err| err.to_string())
    }
}

impl SyncConfigProvider for SettingsSyncConfig {
    fn oauth_config(&self) -> SyncOpResult<OAuthConfig> {
        let config = self
            .settings
            .bangumi_config()
            .map_err(|err| bangumi_sync::SyncError::Internal(err.to_string()))?
            .ok_or(bangumi_sync::SyncError::NotAuthorized)?;
        Ok(OAuthConfig {
            client_id: config.client_id,
            client_secret: config.client_secret,
            redirect_uri: config.redirect_uri,
        })
    }

    fn proxy(&self) -> Option<String> {
        // 显式配置优先；否则自动探测环境变量 / Windows 系统代理。
        let explicit = self
            .settings
            .bangumi_config()
            .ok()
            .flatten()
            .and_then(|config| config.proxy);
        explicit.or_else(bangumi_sync::proxy::resolve_proxy)
    }
}

impl Services {
    /// 生产构造：token 存 Windows 凭据管理器，API 走真实网络客户端。
    pub fn new(db_path: &Path) -> ApiResult<Self> {
        Self::build(db_path, Arc::new(KeyringTokenStore::lagm()), None)
    }

    /// 测试用：注入自定义客户端与内存 token 存储。
    pub fn with_client(db_path: &Path, client: Arc<dyn BangumiClient>) -> ApiResult<Self> {
        Self::build(db_path, Arc::new(InMemoryTokenStore::new()), Some(client))
    }

    fn build(
        db_path: &Path,
        token_store: Arc<dyn TokenStore>,
        injected: Option<Arc<dyn BangumiClient>>,
    ) -> ApiResult<Self> {
        Self::build_with_remover(db_path, token_store, injected, Arc::new(SystemRemover))
    }

    /// 测试专用：替换目录删除后端，避免 `cargo test` 往系统回收站写临时目录。
    #[doc(hidden)]
    pub fn build_with_remover(
        db_path: &Path,
        token_store: Arc<dyn TokenStore>,
        injected: Option<Arc<dyn BangumiClient>>,
        remover: Arc<dyn FolderRemover>,
    ) -> ApiResult<Self> {
        let db = Arc::new(Db::open(db_path)?);
        db.migrate()?;

        let bus = Arc::new(EventBus::new());
        let repo = Arc::new(SqliteRepository::new(Arc::clone(&db)));
        let progress = Arc::new(SqliteProgressRepository::new(Arc::clone(&db)));
        let sync_queue = Arc::new(SqliteSyncQueueRepository::new(Arc::clone(&db)));
        let conflicts = Arc::new(SqliteConflictRepository::new(Arc::clone(&db)));
        let metadata_index = Arc::new(SqliteMetadataIndexRepository::new(Arc::clone(&db)));
        let bindings: Arc<dyn GameBindingRepository> =
            Arc::new(SqliteGameBindingRepository::new(Arc::clone(&db)));
        let store = Arc::new(SqliteSettingsStore::new(Arc::clone(&db)));
        let settings = SettingsService::new(store);
        // 内置应用构建下清掉旧版本残留的用户凭据（不会有任何界面入口再改它）。
        match settings.purge_saved_bangumi_credentials() {
            Ok(true) => tracing::info!("已清理旧版本保存的 Bangumi 应用凭据（当前使用内置应用）"),
            Ok(false) => {}
            Err(err) => tracing::warn!(error = %err, "清理旧 Bangumi 凭据失败"),
        }
        let config_provider: Arc<dyn SyncConfigProvider> = Arc::new(SettingsSyncConfig {
            settings: settings.clone(),
        });
        let client: Arc<dyn BangumiClient> = match injected {
            Some(client) => client,
            None => Arc::new(BangumiHttpClient::new(
                Arc::clone(&token_store),
                Arc::clone(&config_provider),
            )?),
        };

        let sync_engine = Arc::new(SyncEngine::new(
            Arc::clone(&client),
            Arc::clone(&sync_queue) as Arc<dyn SyncQueueRepository>,
            Arc::clone(&conflicts) as Arc<dyn ConflictRepository>,
            Arc::clone(&repo) as Arc<dyn LibraryRepository>,
            Arc::clone(&progress) as Arc<dyn ProgressRepository>,
        ));

        let launcher = Arc::new(Launcher::new(bus.clone(), bindings));
        // 未配置播放器时注入指向空路径的后端；真正播放前会用设置里的路径替换它。
        // 未配置播放器时走系统默认关联程序，开箱即可播放。
        let placeholder: Arc<dyn PlayerBackend> = Arc::new(SystemDefaultPlayer::new());
        let player = Player::new(placeholder, progress.clone(), bus.clone());

        let plugins = Arc::new(SqlitePluginRegistryRepository::new(Arc::clone(&db)));
        let default_plugins_dir = db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("plugins");
        if let Err(err) = std::fs::create_dir_all(&default_plugins_dir) {
            tracing::warn!(error = %err, root = %default_plugins_dir.display(), "创建插件目录失败");
        }

        let services = Self {
            db,
            bus,
            repo,
            progress,
            sync_queue,
            conflicts,
            metadata_index,
            client,
            sync_engine,
            token_store,
            config_provider,
            settings,
            plugins,
            default_plugins_dir,
            plugin_runtime: CapabilityRuntime::new()?,
            anime: AnimeScanner::new()?,
            game: GameScanner::new()?,
            launcher,
            player,
            player_backend_path: Mutex::new(None),
            remover,
        };
        // 把插件根目录里已存在且清单合法的插件登记进注册表（默认禁用）。
        services.sync_discovered_plugins();
        Ok(services)
    }

    /// 启动游戏状态轮询与播放会话轮询。两者都是常驻任务，随应用退出结束。
    ///
    /// 这里用 Tauri 的全局运行时：在 setup 阶段（主线程）没有 tokio reactor，
    /// 直接 tokio::spawn 会 panic。
    pub fn spawn_background_tasks(self: &Arc<Self>) {
        tauri::async_runtime::spawn(Arc::clone(&self.launcher).run_monitor());
        tauri::async_runtime::spawn(Arc::clone(&self.player).run_monitor());
        // plan.md 6.6：启动后自动拉取一次云端数据（未授权时静默跳过）。
        tauri::async_runtime::spawn(Self::run_startup_pull(Arc::clone(self)));
        // 播放会话结束后把「看过这一集」写入推送队列。
        tauri::async_runtime::spawn(Self::run_progress_sync(Arc::clone(self)));
    }

    /// 会话结束时的入队：时长为 0 表示没看成，不推送。
    fn enqueue_session_progress(
        &self,
        item_id: &str,
        episode: u32,
        watched_seconds: u64,
    ) -> ApiResult<()> {
        if watched_seconds == 0 {
            return Ok(());
        }
        self.enqueue_progress(item_id, episode, watched_seconds)
    }

    /// 监听播放会话结束，把进度写进推送队列。
    ///
    /// 播放器 crate 不依赖同步队列（plan.md 3.2），所以这里订阅主干道事件再入队：
    /// 事件里已经带了集号与本次观看时长，不需要再读库。
    async fn run_progress_sync(self: Arc<Self>) {
        let mut receiver = self.bus.subscribe();
        loop {
            match receiver.recv().await {
                Ok(event) => {
                    let AppEvent::PlayerSessionFinished {
                        item_id,
                        episode,
                        watched_seconds,
                    } = event.as_ref()
                    else {
                        continue;
                    };
                    if let Err(err) =
                        self.enqueue_session_progress(item_id, *episode, *watched_seconds)
                    {
                        tracing::warn!(error = %err, item = %item_id, "播放结束入队失败");
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    tracing::warn!(skipped, "进度同步订阅落后，已跳过部分事件");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    /// 启动拉取：等界面就绪几秒后执行一次；未授权或无绑定条目时仅记录日志。
    async fn run_startup_pull(self: Arc<Self>) {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if !self.client.is_authorized() {
            tracing::info!("未授权 Bangumi，跳过启动拉取");
            return;
        }
        // 先无条件续期：Bangumi 的 access/refresh token 都是 7 天且一起轮换，
        // 只在临近过期才刷新会让 refresh token 一路老化，用户隔一周再开就要重新授权。
        match self.client.force_refresh().await {
            Ok(()) => tracing::info!("Bangumi token 已续期"),
            Err(bangumi_sync::SyncError::NotAuthorized) => {
                tracing::warn!("Bangumi 授权已失效，需要用户重新授权");
                return;
            }
            // 网络/代理问题不该打断启动，沿用现有 token 继续尝试拉取。
            Err(err) => tracing::warn!(error = %err, "启动续期失败，沿用现有 token"),
        }
        match self.pull_all().await {
            Ok(report) => tracing::info!(
                scanned = report.scanned_items,
                conflicts = report.conflicts,
                imported_progress = report.imported_progress,
                "启动拉取完成"
            ),
            Err(err) => tracing::warn!(error = %err, "启动拉取失败"),
        }
    }

    pub fn bus(&self) -> &EventBus {
        &self.bus
    }

    pub fn db_path(&self) -> Option<PathBuf> {
        self.db.path().map(Path::to_path_buf)
    }

    pub fn get_settings(&self) -> ApiResult<SettingsDto> {
        Ok(self.settings_to_dto(self.settings.load()?))
    }

    pub fn save_settings(&self, input: &SettingsInput) -> ApiResult<SettingsDto> {
        let saved = self.settings.save(input)?;
        // 播放器路径或参数变化立即生效：下次播放用新后端，无需重启应用。
        // 清空路径表示回到「系统默认播放器」。
        let choice = match &saved.player_path {
            Some(path) => {
                let template = saved
                    .player_args
                    .clone()
                    .unwrap_or_else(|| crate::settings::DEFAULT_PLAYER_ARGS.to_owned());
                PlayerBackendChoice::External(path.clone(), template)
            }
            None => PlayerBackendChoice::System,
        };
        self.ensure_player_backend(choice)?;
        tracing::info!(
            anime = ?saved.anime_root,
            game = ?saved.game_root,
            cache = %saved.metadata_cache_dir.display(),
            player = ?saved.player_path,
            args = ?saved.player_args,
            "设置已更新"
        );
        // 插件目录可能变化：重新扫描并登记（best-effort）。
        self.sync_discovered_plugins();
        Ok(self.settings_to_dto(saved))
    }

    /// 后台执行「扫描 → 自动匹配」，不阻塞设置保存的返回。
    ///
    /// 由命令层在「资源根目录发生变化」后调用：用户加完根目录就想直接看到条目。
    pub(crate) fn spawn_auto_import(self: &Arc<Self>) {
        let services = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let scan_owner = Arc::clone(&services);
            let scanned = tauri::async_runtime::spawn_blocking(move || scan_owner.scan()).await;
            match scanned {
                Ok(Ok(summary)) => tracing::info!(
                    created = summary.total_created,
                    updated = summary.total_updated,
                    "自动导入：扫描完成"
                ),
                Ok(Err(err)) => {
                    tracing::warn!(error = %err, "自动导入：扫描失败");
                    return;
                }
                Err(err) => {
                    tracing::warn!(error = %err, "自动导入：扫描任务异常");
                    return;
                }
            }
            if !services.client.is_authorized() {
                tracing::info!("自动导入：未授权，跳过自动匹配");
                return;
            }
            match services.auto_match_and_pull().await {
                Ok(report) => tracing::info!(
                    bound = report.bound,
                    unmatched = report.unmatched.len(),
                    "自动导入：匹配完成"
                ),
                Err(err) => tracing::warn!(error = %err, "自动导入：匹配失败"),
            }
        });
    }

    /// 扫描两个根目录。已配置的根目录才会被扫描，未配置的给出提示而不是报错。
    pub fn scan(&self) -> ApiResult<ScanSummaryDto> {
        let settings = self.settings.load()?;
        let mut messages = Vec::new();
        let mut anime_report = None;
        let mut game_report = None;

        match &settings.anime_root {
            Some(root) => {
                let root_text = root.to_string_lossy().to_string();
                self.bus.publish(AppEvent::ScannerJobStarted {
                    kind: ItemKind::Anime,
                    root: root_text.clone(),
                });
                match self.anime.scan(root, self.repo.as_ref(), self.bus.as_ref()) {
                    Ok(report) => {
                        self.bus.publish(AppEvent::ScannerJobFinished {
                            kind: ItemKind::Anime,
                            root: root_text,
                            created: report.created,
                            updated: report.updated,
                            unchanged: report.unchanged,
                        });
                        anime_report = Some(report);
                    }
                    Err(err) => {
                        let message = format!("动画扫描失败：{err}");
                        tracing::warn!(error = %err, "动画扫描失败");
                        self.bus.publish(AppEvent::ScannerJobFailed {
                            kind: ItemKind::Anime,
                            root: root_text,
                            message: message.clone(),
                        });
                        messages.push(message);
                    }
                }
            }
            None => messages.push("尚未配置动画根目录，已跳过动画扫描".to_owned()),
        }

        match &settings.game_root {
            Some(root) => {
                let root_text = root.to_string_lossy().to_string();
                self.bus.publish(AppEvent::ScannerJobStarted {
                    kind: ItemKind::Game,
                    root: root_text.clone(),
                });
                match self.game.scan(root, self.repo.as_ref(), self.bus.as_ref()) {
                    Ok(report) => {
                        self.bus.publish(AppEvent::ScannerJobFinished {
                            kind: ItemKind::Game,
                            root: root_text,
                            created: report.created,
                            updated: report.updated,
                            unchanged: report.unchanged,
                        });
                        game_report = Some(report);
                    }
                    Err(err) => {
                        let message = format!("游戏扫描失败：{err}");
                        tracing::warn!(error = %err, "游戏扫描失败");
                        self.bus.publish(AppEvent::ScannerJobFailed {
                            kind: ItemKind::Game,
                            root: root_text,
                            message: message.clone(),
                        });
                        messages.push(message);
                    }
                }
            }
            None => messages.push("尚未配置游戏根目录，已跳过游戏扫描".to_owned()),
        }

        let total_created = anime_report.as_ref().map_or(0, |r| r.created)
            + game_report.as_ref().map_or(0, |r| r.created);
        let total_updated = anime_report.as_ref().map_or(0, |r| r.updated)
            + game_report.as_ref().map_or(0, |r| r.updated);
        let total_unchanged = anime_report.as_ref().map_or(0, |r| r.unchanged)
            + game_report.as_ref().map_or(0, |r| r.unchanged);

        Ok(ScanSummaryDto {
            anime: anime_report,
            game: game_report,
            total_created,
            total_updated,
            total_unchanged,
            messages,
        })
    }

    pub fn list_items(&self, kind: Option<ItemKind>) -> ApiResult<Vec<ItemSummaryDto>> {
        let items = self.repo.list_items(kind)?;
        Ok(items.iter().map(|item| self.to_summary(item)).collect())
    }

    pub fn get_item(&self, id: &str) -> ApiResult<ItemDetailDto> {
        let item = self.require_item(id)?;
        let probe = self.probe(&item);
        Ok(ItemDetailDto {
            item: summary_from(&item, &probe),
            files: probe.files,
        })
    }

    /// 清空条目记录（可按类型）：只删数据库，磁盘上的文件一概不碰。
    ///
    /// 进度、绑定、同步队列、冲突由外键级联清理；元数据缓存目录保留，
    /// 因为它是按 subject_id 存的，与条目记录无关。
    pub fn clear_items(&self, kind: Option<ItemKind>) -> ApiResult<usize> {
        let removed = self.repo.delete_all_items(kind)?;
        tracing::info!(
            removed,
            kind = ?kind,
            "已清空条目记录（未触碰磁盘文件）"
        );
        Ok(removed)
    }

    /// 手动绑定 Bangumi 条目 ID。本阶段只落库并发占位事件，不调用任何 API（plan.md 6.2）。
    pub fn bind_bangumi_subject(
        &self,
        id: &str,
        subject_id: Option<i64>,
    ) -> ApiResult<ItemSummaryDto> {
        if let Some(value) = subject_id {
            if value <= 0 {
                return Err(AppError::Validation(
                    "Bangumi 条目 ID 必须是正整数".to_owned(),
                ));
            }
        }
        let item = self.repo.bind_bangumi_subject(id, subject_id)?;
        self.bus.publish(AppEvent::LibraryItemBound {
            id: item.id.clone(),
            bangumi_subject_id: item.bangumi_subject_id,
        });
        tracing::info!(
            item = %item.id,
            subject = ?item.bangumi_subject_id,
            "手动绑定 Bangumi 条目（占位，未调用 API）"
        );
        Ok(self.to_summary(&item))
    }

    /// 游戏条目的绑定信息与运行状态。
    pub fn game_status(&self, id: &str) -> ApiResult<GameStatusDto> {
        let item = self.require_item(id)?;
        let running_pid = self.launcher.running_pid(&item.id)?;
        let binding = self
            .launcher
            .binding(&item)?
            .map(|config| binding_dto(&item, &config));
        Ok(GameStatusDto {
            running_pid,
            binding,
        })
    }

    /// 绑定或更换可执行文件，返回最新的绑定信息。
    pub fn set_game_binding(&self, id: &str, exe_path: &str) -> ApiResult<GameStatusDto> {
        let item = self.require_item(id)?;
        if item.kind != ItemKind::Game {
            return Err(AppError::Validation(
                "只有游戏条目可以绑定可执行文件".to_owned(),
            ));
        }
        self.launcher.set_binding(&item, exe_path)?;
        self.game_status(id)
    }

    pub fn start_game(&self, id: &str) -> ApiResult<RunningGame> {
        let item = self.require_item(id)?;
        Ok(self.launcher.start(&item)?)
    }

    pub fn running_games(&self) -> ApiResult<Vec<RunningGame>> {
        Ok(self.launcher.running()?)
    }

    pub fn active_sessions(&self) -> ApiResult<Vec<ActiveSession>> {
        Ok(self.player.active()?)
    }

    /// 播放条目内指定文件。集号取扫描解析结果，解析不到则用文件序号（从 1 起）。
    /// 播放「指定集之后」的第一集；已是最后一集时返回 None。
    ///
    /// 连播由前端全局订阅会话结束事件触发，因此这里只负责「下一集是谁」，
    /// 不依赖详情页是否还开着（离开页面也能继续）。
    pub fn play_next_episode(
        &self,
        item_id: &str,
        finished_episode: u32,
    ) -> ApiResult<Option<PlayingSession>> {
        let item = self.require_item(item_id)?;
        if item.kind != ItemKind::Anime {
            return Ok(None);
        }
        let folder = PathBuf::from(&item.folder_path);
        let probe = self.anime.probe(&folder, item.file_filter.as_deref())?;
        let mut ordered: Vec<(u32, String)> = probe
            .files
            .iter()
            .filter_map(|file| file.episode.map(|episode| (episode, file.name.clone())))
            .collect();
        ordered.sort_by_key(|(episode, _)| *episode);
        let Some((_, file_name)) = ordered
            .into_iter()
            .find(|(episode, _)| *episode > finished_episode)
        else {
            return Ok(None);
        };
        self.play_episode(item_id, &file_name).map(Some)
    }

    /// 批量返回**已缓存**的封面（data URL），不发网络请求。
    ///
    /// 列表页切到封面视图时用：有缓存立刻显示，缺的由 `spawn_cover_prefetch` 后台补。
    pub fn item_covers_cached(&self, ids: &[String]) -> ApiResult<HashMap<String, String>> {
        let settings = self.settings.load()?;
        let cache = MetadataCache::new(settings.metadata_cache_dir);
        let mut covers = HashMap::new();
        for id in ids {
            let Ok(item) = self.require_item(id) else {
                continue;
            };
            let Some(subject_id) = item.bangumi_subject_id else {
                continue;
            };
            let path = cache.cover_file(subject_id);
            match std::fs::read(&path) {
                Ok(bytes) if !bytes.is_empty() => {
                    covers.insert(id.clone(), crate::cover::data_url(&bytes));
                }
                _ => {}
            }
        }
        Ok(covers)
    }

    /// 后台补齐缺失的封面，每完成一条发 `metadata:cover:ready`。
    ///
    /// 限并发 2：封面走 CDN，不值得占满带宽；失败只记日志，不打扰界面。
    pub fn spawn_cover_prefetch(self: &Arc<Self>, ids: Vec<String>) {
        let services = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let semaphore = Arc::new(tokio::sync::Semaphore::new(2));
            let mut tasks = tokio::task::JoinSet::new();
            for id in ids {
                let Ok(permit) = Arc::clone(&semaphore).acquire_owned().await else {
                    break;
                };
                let services = Arc::clone(&services);
                tasks.spawn(async move {
                    let _permit = permit;
                    // 已有缓存就不必再动网络。
                    if let Ok(item) = services.require_item(&id) {
                        if let Some(subject_id) = item.bangumi_subject_id {
                            let settings = match services.settings.load() {
                                Ok(settings) => settings,
                                Err(_) => return,
                            };
                            let cache = MetadataCache::new(settings.metadata_cache_dir);
                            if cache.cover_file(subject_id).is_file() {
                                return;
                            }
                            match cache
                                .ensure_cover(services.client.as_ref(), subject_id)
                                .await
                            {
                                Ok(Some(_)) => {
                                    services
                                        .bus
                                        .publish(AppEvent::MetadataCoverReady { item_id: id });
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    tracing::debug!(error = %err, item = %id, "封面预热失败");
                                }
                            }
                        }
                    }
                });
            }
            while tasks.join_next().await.is_some() {}
        });
    }

    /// 读取（必要时下载并缓存）条目封面，返回 data URL。
    ///
    /// 未绑定 Bangumi、没有封面或下载失败都返回 None——封面是锦上添花，
    /// 不该让详情页报错或转圈。
    pub async fn item_cover(&self, item_id: &str) -> ApiResult<Option<String>> {
        let item = self.require_item(item_id)?;
        let Some(subject_id) = item.bangumi_subject_id else {
            return Ok(None);
        };
        let settings = self.settings.load()?;
        let cache = MetadataCache::new(settings.metadata_cache_dir);
        match cache.ensure_cover(self.client.as_ref(), subject_id).await {
            Ok(Some(path)) => {
                let bytes = std::fs::read(&path)
                    .map_err(|err| AppError::Internal(format!("读取封面失败：{err}")))?;
                Ok(Some(crate::cover::data_url(&bytes)))
            }
            Ok(None) => Ok(None),
            Err(err) => {
                tracing::warn!(error = %err, subject = subject_id, "封面缓存失败");
                Ok(None)
            }
        }
    }

    pub fn play_episode(&self, item_id: &str, file_name: &str) -> ApiResult<PlayingSession> {
        let item = self.require_item(item_id)?;
        if item.kind != ItemKind::Anime {
            return Err(AppError::Validation("只有动画条目可以播放".to_owned()));
        }

        let folder = PathBuf::from(&item.folder_path);
        let probe = self.anime.probe(&folder, item.file_filter.as_deref())?;
        let index = probe
            .files
            .iter()
            .position(|file| file.name == file_name)
            .ok_or_else(|| AppError::Core(CoreError::ItemNotFound(file_name.to_owned())))?;
        let file = &probe.files[index];
        let episode = file.episode.unwrap_or((index as u32) + 1);
        let uri = folder.join(&file.name);
        if !uri.is_file() {
            return Err(AppError::Player(PlayerError::MediaNotFound(uri)));
        }

        // 先确认文件真实存在，再解析播放器：文件找不到时不该先报播放器的问题。
        // 未指定播放器时交给系统默认关联程序（按扩展名查注册表）。
        let settings = self.settings.load()?;
        let choice = match settings.player_path.clone() {
            Some(path) => {
                let template = settings
                    .player_args
                    .clone()
                    .unwrap_or_else(|| crate::settings::DEFAULT_PLAYER_ARGS.to_owned());
                PlayerBackendChoice::External(path, template)
            }
            None => PlayerBackendChoice::System,
        };

        // 断点续播：取本集已有进度作为起点，进度写入时按「起点 + 本次时长」累加。
        let start_seconds = self
            .progress
            .get(&item.id, episode, ProgressSource::Local)?
            .map(|row| u64::try_from(row.watched_seconds).unwrap_or_default())
            .unwrap_or(0);

        self.ensure_player_backend(choice)?;
        Ok(self
            .player
            .play(&item.id, episode, &file.name, &uri, start_seconds)?)
    }

    pub fn list_progress(&self, item_id: &str) -> ApiResult<Vec<ProgressDto>> {
        let rows = self.progress.list_by_item(item_id)?;
        Ok(rows
            .into_iter()
            .map(|row| ProgressDto {
                episode: u32::try_from(row.episode).unwrap_or_default(),
                watched_seconds: u64::try_from(row.watched_seconds).unwrap_or_default(),
                source: row.source,
                updated_at: row.updated_at,
            })
            .collect())
    }

    /// 手动校正进度（plan.md 6.4 的「UI 提供手动校正入口」）。
    pub fn set_progress(
        &self,
        item_id: &str,
        episode: u32,
        watched_seconds: u64,
    ) -> ApiResult<ProgressDto> {
        if episode == 0 {
            return Err(AppError::Validation("集号必须大于 0".to_owned()));
        }
        self.require_item(item_id)?;
        let row = self
            .progress
            .upsert(item_id, episode, watched_seconds, ProgressSource::Local)?;
        // 播放中的会话要把起点一起校正，否则下一轮轮询会覆盖掉这次手动修改。
        self.player.rebase(item_id, episode, watched_seconds)?;
        // 手动校正是最明确的一次「我看到第几集」，直接进推送队列；
        // Bangumi 只有「看过 / 未看」，watched_seconds > 0 折算成看过。
        self.enqueue_progress(item_id, episode, watched_seconds)?;
        Ok(ProgressDto {
            episode,
            watched_seconds,
            source: row.source,
            updated_at: row.updated_at,
        })
    }

    /// 手动推进一次轮询。正常由后台任务驱动，测试与排障时可主动调用。
    pub fn tick(&self) -> ApiResult<()> {
        self.launcher.check_once()?;
        self.player.check_once()?;
        Ok(())
    }

    // ---- M3 同步（离线骨架） ----

    /// 设置收藏状态 / 评分：落库并（已绑定时）入同步队列。
    pub fn set_collection(
        &self,
        id: &str,
        collection_status: Option<i64>,
        rating: Option<i64>,
    ) -> ApiResult<CollectionDto> {
        if let Some(status) = collection_status {
            if !(1..=5).contains(&status) {
                return Err(AppError::Validation(
                    "收藏状态必须是 1-5（想看/看过/在看/搁置/抛弃）".to_owned(),
                ));
            }
        }
        if let Some(value) = rating {
            if !(0..=10).contains(&value) {
                return Err(AppError::Validation("评分必须在 0-10 之间".to_owned()));
            }
        }
        let item = self.repo.set_collection(id, collection_status, rating)?;
        if let Some(subject_id) = item.bangumi_subject_id {
            let payload = CollectionPayload {
                item_id: item.id.clone(),
                subject_id,
                collection_status: item.collection_status,
                rating: item.rating,
            };
            let entry = self.sync_queue.enqueue(
                Some(&item.id),
                library_core::model::sync_op::COLLECTION,
                &payload.to_json()?,
            )?;
            self.bus.publish(AppEvent::SyncQueueEnqueued {
                id: entry.id,
                item_id: entry.item_id,
                op: entry.op,
            });
        }
        Ok(CollectionDto {
            item_id: item.id,
            collection_status: item.collection_status,
            rating: item.rating,
            collection_updated_at: item.collection_updated_at,
        })
    }

    /// 若条目已绑定 Bangumi，则把单集进度写入推送队列。未绑定直接跳过。
    fn enqueue_progress(&self, item_id: &str, episode: u32, watched_seconds: u64) -> ApiResult<()> {
        let Some(item) = self.repo.get_item(item_id)? else {
            return Ok(());
        };
        let Some(subject_id) = item.bangumi_subject_id else {
            return Ok(());
        };
        let payload = ProgressPayload {
            item_id: item.id.clone(),
            subject_id,
            episode,
            watched_seconds,
        };
        let entry = self.sync_queue.enqueue(
            Some(&item.id),
            library_core::model::sync_op::PROGRESS,
            &payload.to_json()?,
        )?;
        self.bus.publish(AppEvent::SyncQueueEnqueued {
            id: entry.id,
            item_id: entry.item_id,
            op: entry.op,
        });
        Ok(())
    }

    pub fn list_sync_queue(&self) -> ApiResult<Vec<SyncQueueDto>> {
        let entries = self.sync_queue.list_all()?;
        Ok(entries
            .into_iter()
            .map(|entry| SyncQueueDto {
                item_title: entry
                    .item_id
                    .as_deref()
                    .and_then(|id| self.repo.get_item(id).ok().flatten())
                    .map(|item| item.title),
                id: entry.id,
                item_id: entry.item_id,
                op: entry.op,
                status: entry.status.as_str().to_owned(),
                attempts: entry.attempts,
                next_retry_at: entry.next_retry_at,
                created_at: entry.created_at,
            })
            .collect())
    }

    pub fn sync_stats(&self) -> ApiResult<SyncStatsDto> {
        let stats = self.sync_engine.stats()?;
        Ok(SyncStatsDto {
            authorized: self.client.is_authorized(),
            pending: stats.pending,
            failed: stats.failed,
            done: stats.done,
            unresolved_conflicts: stats.unresolved_conflicts,
        })
    }

    /// 手动触发一次推送（按限速逐条发送）。
    pub async fn push_pending(&self, limit: usize) -> ApiResult<PushReportDto> {
        let report = self.sync_engine.push_pending(limit.max(1)).await?;
        Ok(PushReportDto {
            attempted: report.attempted,
            succeeded: report.succeeded,
            failed: report.failed,
        })
    }

    /// 拉取所有已绑定条目的云端数据并与本地比较（plan.md 6.6 启动拉取）。
    pub async fn pull_all(&self) -> ApiResult<PullReportDto> {
        if !self.client.is_authorized() {
            return Err(AppError::Sync(bangumi_sync::SyncError::NotAuthorized));
        }
        let started = std::time::Instant::now();
        let items = self.repo.list_items(None)?;
        let mut report = PullReportDto {
            scanned_items: 0,
            conflicts: 0,
            imported_collection: 0,
            imported_progress: 0,
            messages: Vec::new(),
        };

        let mut consecutive_network_failures = 0usize;
        for item in items {
            let Some(subject_id) = item.bangumi_subject_id else {
                continue;
            };
            let local = LocalCollection {
                item_id: item.id.clone(),
                subject_id,
                collection_status: item.collection_status,
                rating: item.rating,
                collection_updated_at: item.collection_updated_at.clone(),
            };
            let local_progress = self.local_progress(&item, subject_id)?;
            match self.sync_engine.pull_item(&local, &local_progress).await {
                Ok(pull) => {
                    report.scanned_items += 1;
                    report.conflicts += pull.conflicts.len();
                    if pull.imported_collection {
                        report.imported_collection += 1;
                    }
                    report.imported_progress += pull.imported_progress;
                    for conflict in &pull.conflicts {
                        self.bus.publish(AppEvent::SyncConflictDetected {
                            item_id: item.id.clone(),
                            field: conflict.field.clone(),
                        });
                    }
                }
                Err(err) => {
                    let text = err.to_string();
                    let network = text.contains("网络请求失败") || text.contains("超时");
                    report.messages.push(format!(
                        "{} 拉取失败：{}",
                        item.title,
                        short_reason(&text, 80)
                    ));
                    if network {
                        consecutive_network_failures += 1;
                        // 网络整体不可用时，后面每个条目都要等一次超时，没必要。
                        if consecutive_network_failures >= 2 {
                            report.messages.push(
                                "网络不可用，已跳过剩余条目的云端拉取；请检查代理或稍后重试"
                                    .to_owned(),
                            );
                            break;
                        }
                    } else {
                        consecutive_network_failures = 0;
                    }
                }
            }
        }
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            items = report.scanned_items,
            "拉取云端完成"
        );
        Ok(report)
    }

    fn local_progress(&self, item: &LibraryItem, subject_id: i64) -> ApiResult<Vec<LocalProgress>> {
        let rows = self.progress.list_by_item(&item.id)?;
        Ok(rows
            .into_iter()
            .filter(|row| row.source == ProgressSource::Local.as_str())
            .map(|row| LocalProgress {
                item_id: item.id.clone(),
                subject_id,
                episode: u32::try_from(row.episode).unwrap_or_default(),
                watched_seconds: u64::try_from(row.watched_seconds).unwrap_or_default(),
                updated_at: Some(row.updated_at),
            })
            .collect())
    }

    pub fn list_conflicts(&self) -> ApiResult<Vec<ConflictDto>> {
        let rows = self.conflicts.list_unresolved()?;
        Ok(rows
            .into_iter()
            .map(|row| ConflictDto {
                item_title: row
                    .item_id
                    .as_deref()
                    .and_then(|id| self.repo.get_item(id).ok().flatten())
                    .map(|item| item.title),
                id: row.id,
                item_id: row.item_id,
                field: row.field,
                local_value: row.local_value,
                cloud_value: row.cloud_value,
                local_ts: row.local_ts,
                cloud_ts: row.cloud_ts,
            })
            .collect())
    }

    /// 批量裁决冲突：`choice` 为 `local` 采用本地，`cloud` 采用云端。
    pub fn resolve_conflicts(&self, ids: &[String], choice: &str) -> ApiResult<ResolveReportDto> {
        let choice = choice.to_ascii_lowercase();
        if choice != "local" && choice != "cloud" {
            return Err(AppError::Validation(
                "裁决方式只能是 local 或 cloud".to_owned(),
            ));
        }
        let unresolved = self.conflicts.list_unresolved()?;
        let mut resolved = 0;
        for conflict in unresolved
            .into_iter()
            .filter(|row| ids.is_empty() || ids.contains(&row.id))
        {
            if let Some(item_id) = conflict.item_id.clone() {
                if let Err(err) = self.apply_conflict(&item_id, &conflict, &choice) {
                    tracing::warn!(conflict = %conflict.id, error = %err, "应用冲突失败，跳过");
                    continue;
                }
            }
            self.conflicts.resolve(&conflict.id)?;
            resolved += 1;
        }
        Ok(ResolveReportDto { resolved })
    }

    fn apply_conflict(
        &self,
        item_id: &str,
        conflict: &library_core::model::Conflict,
        choice: &str,
    ) -> ApiResult<()> {
        let from_cloud = choice == "cloud";
        if conflict.field == library_core::model::conflict_field::COLLECTION_STATUS
            || conflict.field == library_core::model::conflict_field::RATING
        {
            let item = self.require_item(item_id)?;
            let mut status = item.collection_status;
            let mut rating = item.rating;
            if conflict.field == library_core::model::conflict_field::COLLECTION_STATUS
                && from_cloud
            {
                status = parse_i64(conflict.cloud_value.as_deref()).or(status);
            }
            if conflict.field == library_core::model::conflict_field::RATING && from_cloud {
                rating = parse_i64(conflict.cloud_value.as_deref()).or(rating);
            }
            self.repo.set_collection(item_id, status, rating)?;
            if !from_cloud {
                // 采用本地：把当前本地值推送云端。
                if let Some(subject_id) = item.bangumi_subject_id {
                    let payload = CollectionPayload {
                        item_id: item_id.to_owned(),
                        subject_id,
                        collection_status: status,
                        rating,
                    };
                    self.sync_queue.enqueue(
                        Some(item_id),
                        library_core::model::sync_op::COLLECTION,
                        &payload.to_json()?,
                    )?;
                }
            }
            return Ok(());
        }

        if let Some(episode) = conflict
            .field
            .strip_prefix("progress:")
            .and_then(|raw| raw.parse::<u32>().ok())
        {
            // 本地秒数与云端布尔不可比，冲突值统一是「看过 / 未看」；
            // 采用云端时保留本地秒数（本地没记录则写 1 秒占位，表示「看过了」）。
            let local_seconds = self
                .list_progress(item_id)?
                .into_iter()
                .find(|row| row.episode == episode)
                .map(|row| row.watched_seconds)
                .unwrap_or(0);
            if from_cloud {
                let watched = conflict.cloud_value.as_deref() == Some("看过");
                let seconds = if watched { local_seconds.max(1) } else { 0 };
                self.progress
                    .upsert(item_id, episode, seconds, ProgressSource::Local)?;
            } else {
                self.enqueue_progress(item_id, episode, local_seconds)?;
            }
            return Ok(());
        }

        Err(AppError::Internal(format!(
            "未知冲突字段：{}",
            conflict.field
        )))
    }

    /// 抓取并缓存条目元数据到可配置目录（ADR-009）。
    pub async fn cache_metadata(&self, item_id: &str) -> ApiResult<MetadataCacheDto> {
        let item = self.require_item(item_id)?;
        let subject_id = item.bangumi_subject_id.ok_or_else(|| {
            AppError::Validation("条目尚未绑定 Bangumi ID，无法缓存元数据".to_owned())
        })?;
        let settings = self.settings.load()?;
        let cache = MetadataCache::new(settings.metadata_cache_dir);
        let path = cache
            .refresh(
                self.client.as_ref(),
                self.metadata_index.as_ref(),
                subject_id,
            )
            .await?;
        Ok(MetadataCacheDto {
            subject_id,
            cached_path: path.to_string_lossy().to_string(),
        })
    }

    // ---- 元数据缓存与日志目录（M5 迭代） ----

    /// 统计元数据缓存目录的条目数、文件数与占用字节。
    pub fn metadata_cache_stats(&self) -> ApiResult<CacheStatsDto> {
        let settings = self.settings.load()?;
        let dir = settings.metadata_cache_dir.clone();
        let mut stats = CacheStatsDto {
            dir: dir.to_string_lossy().to_string(),
            subject_dirs: 0,
            file_count: 0,
            total_bytes: 0,
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(stats);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            stats.subject_dirs += 1;
            let (files, bytes) = dir_usage(&path);
            stats.file_count += files;
            stats.total_bytes += bytes;
        }
        Ok(stats)
    }

    /// 清空元数据缓存目录下的条目子目录。
    ///
    /// 只删缓存：不动条目目录、不动数据库里的 metadata_index 记录以外的东西。
    pub fn clear_metadata_cache(&self) -> ApiResult<CacheClearDto> {
        let settings = self.settings.load()?;
        let dir = settings.metadata_cache_dir.clone();
        let mut report = CacheClearDto {
            removed_dirs: 0,
            freed_bytes: 0,
        };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Ok(report);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let (_, bytes) = dir_usage(&path);
            match std::fs::remove_dir_all(&path) {
                Ok(()) => {
                    report.removed_dirs += 1;
                    report.freed_bytes += bytes;
                }
                Err(err) => {
                    tracing::warn!(path = %path.display(), error = %err, "清理缓存失败");
                }
            }
        }
        tracing::info!(
            dirs = report.removed_dirs,
            bytes = report.freed_bytes,
            "已清理元数据缓存"
        );
        Ok(report)
    }

    /// 打开日志目录（不存在时先创建），便于用户取日志排查。
    pub fn open_log_dir(&self) -> ApiResult<()> {
        let dir = crate::logs::default_log_dir();
        std::fs::create_dir_all(&dir)
            .map_err(|err| AppError::Internal(format!("创建日志目录失败：{err}")))?;
        std::process::Command::new("explorer")
            .arg(&dir)
            .spawn()
            .map_err(|err| AppError::Internal(format!("打开日志目录失败：{err}")))?;
        Ok(())
    }

    // ---- 拖入导入（M5 迭代） ----

    /// 把拖进窗口的目录 / zip 识别成动画或游戏，剪切到对应根目录后重新扫描。
    ///
    /// 单项失败不影响其它项；导入成功后立刻 scan，条目会随扫描事件出现在列表里。
    pub fn import_drop(&self, paths: &[String]) -> ApiResult<ImportReportDto> {
        let settings = self.settings.load()?;
        let staging = std::env::temp_dir().join("lagm-import");
        std::fs::create_dir_all(&staging)
            .map_err(|err| AppError::Internal(format!("创建暂存目录失败：{err}")))?;

        let mut report = ImportReportDto::default();
        for raw in paths {
            let source = PathBuf::from(raw);
            let plan = match crate::import::plan(&source, &staging) {
                Ok(plan) => plan,
                Err(err) => {
                    report.failed.push(format!("{}：{err}", source.display()));
                    continue;
                }
            };

            let root = match plan.kind {
                ItemKind::Anime => settings.anime_root.clone(),
                ItemKind::Game => settings.game_root.clone(),
            };
            let Some(root) = root else {
                report.skipped.push(format!(
                    "{}：未配置{}根目录，已跳过",
                    plan.name,
                    if plan.kind == ItemKind::Anime {
                        "动画"
                    } else {
                        "游戏"
                    }
                ));
                if let Some(dir) = &plan.cleanup {
                    std::fs::remove_dir_all(dir).ok();
                }
                continue;
            };

            let result = crate::import::move_into(&plan.content, &root, &plan.name);
            if let Some(dir) = &plan.cleanup {
                // zip 的暂存目录在搬运后已空，直接清掉。
                std::fs::remove_dir_all(dir).ok();
            }
            match result {
                Ok(target) => {
                    tracing::info!(
                        source = %source.display(),
                        target = %target.display(),
                        kind = ?plan.kind,
                        "已导入并搬入根目录"
                    );
                    report.imported.push(ImportedItemDto {
                        source: source.to_string_lossy().to_string(),
                        target: target.to_string_lossy().to_string(),
                        name: plan.name,
                        kind: plan.kind,
                    });
                }
                Err(err) => report.failed.push(format!("{}：{err}", plan.name)),
            }
        }

        if !report.imported.is_empty() {
            // 扫描失败不影响「已经搬过去」这个事实，只记录日志。
            if let Err(err) = self.scan() {
                tracing::warn!(error = %err, "导入后扫描失败");
                report.failed.push(format!("导入完成，但扫描失败：{err}"));
            }
        }
        Ok(report)
    }

    // ---- 批量操作（M5 迭代） ----

    /// 批量刷新元数据：只处理已绑定 Bangumi 的条目，逐条缓存元数据并拉取收藏。
    pub async fn refresh_metadata_batch(
        self: &Arc<Self>,
        ids: &[String],
    ) -> ApiResult<BatchRefreshDto> {
        let total = ids.len();
        let mut report = BatchRefreshDto::default();
        if total == 0 {
            return Ok(report);
        }

        let semaphore = Arc::new(tokio::sync::Semaphore::new(3));
        let mut tasks = tokio::task::JoinSet::new();
        for id in ids {
            let permit = Arc::clone(&semaphore)
                .acquire_owned()
                .await
                .map_err(|err| AppError::Internal(format!("批量刷新调度失败：{err}")))?;
            let services = Arc::clone(self);
            let id = id.clone();
            tasks.spawn(async move {
                let _permit = permit;
                let outcome = services.refresh_single(&id).await;
                (id, outcome)
            });
        }

        let mut done = 0usize;
        while let Some(joined) = tasks.join_next().await {
            done += 1;
            let (id, outcome) = match joined {
                Ok(pair) => pair,
                Err(err) => (
                    String::new(),
                    RefreshOutcome::Failed(format!("刷新任务异常：{err}")),
                ),
            };
            let ok = matches!(outcome, RefreshOutcome::Refreshed);
            match outcome {
                RefreshOutcome::Refreshed => report.refreshed += 1,
                RefreshOutcome::Skipped => report.skipped += 1,
                RefreshOutcome::Failed(message) => {
                    report.failed += 1;
                    report.messages.push(message);
                }
            }
            self.bus.publish(AppEvent::MetadataBatchProgress {
                done,
                total,
                item_id: id,
                ok,
            });
        }

        tracing::info!(
            total,
            refreshed = report.refreshed,
            skipped = report.skipped,
            failed = report.failed,
            "批量刷新完成"
        );
        Ok(report)
    }

    /// 单条刷新，供批量并发调用。
    async fn refresh_single(&self, id: &str) -> RefreshOutcome {
        let item = match self.require_item(id) {
            Ok(item) => item,
            Err(_) => return RefreshOutcome::Failed(format!("{id}：条目不存在")),
        };
        if item.bangumi_subject_id.is_none() {
            return RefreshOutcome::Skipped;
        }
        match self.refresh_item(id).await {
            Ok(()) => RefreshOutcome::Refreshed,
            Err(err) => RefreshOutcome::Failed(format!(
                "{}：{}",
                item.title,
                short_reason(&err.to_string(), 60)
            )),
        }
    }

    /// 批量移入回收站。永久删除不提供批量入口，必须逐条输入标题确认。
    pub fn trash_items(&self, ids: &[String]) -> ApiResult<BatchDeleteDto> {
        let mut report = BatchDeleteDto::default();
        for id in ids {
            let title = self
                .repo
                .get_item(id)?
                .map(|item| item.title)
                .unwrap_or_else(|| id.clone());
            match self.delete_item(id, DeleteMode::Trash, None) {
                Ok(_) => report.deleted += 1,
                Err(err) => {
                    report.failed += 1;
                    report.messages.push(format!("{title}：{err}"));
                }
            }
        }
        Ok(report)
    }

    // ---- 资源定位（M5 迭代） ----

    /// 在资源管理器里打开条目目录，或选中该条目下的某个文件。
    ///
    /// 只做「定位」，不启动文件（启动是 launcher / player 的职责）。
    pub fn open_in_explorer(&self, item_id: &str, file_name: Option<&str>) -> ApiResult<()> {
        let item = self.require_item(item_id)?;
        let folder = PathBuf::from(&item.folder_path);
        if !folder.is_dir() {
            return Err(AppError::Validation(format!(
                "条目目录不存在：{}",
                folder.display()
            )));
        }

        match file_name {
            None => {
                std::process::Command::new("explorer")
                    .arg(&folder)
                    .spawn()
                    .map_err(|err| AppError::Internal(format!("打开资源管理器失败：{err}")))?;
            }
            Some(name) => {
                let target = folder.join(name);
                if !target.is_file() {
                    return Err(AppError::Validation(format!(
                        "文件不存在：{}",
                        target.display()
                    )));
                }
                // 文件名来自前端，先 canonicalize 再确认没跑出条目目录。
                let folder_abs = folder
                    .canonicalize()
                    .map_err(|err| AppError::Internal(format!("条目目录不可访问：{err}")))?;
                let target_abs = target
                    .canonicalize()
                    .map_err(|err| AppError::Internal(format!("文件不可访问：{err}")))?;
                if !target_abs.starts_with(&folder_abs) {
                    return Err(AppError::Validation("文件不在条目目录内".to_owned()));
                }
                std::process::Command::new("explorer")
                    .arg(format!("/select,{}", target_abs.display()))
                    .spawn()
                    .map_err(|err| AppError::Internal(format!("打开资源管理器失败：{err}")))?;
            }
        }
        Ok(())
    }

    // ---- 删除资源（M5，plan.md 6.5 / ADR-005） ----

    /// 删除条目对应的磁盘目录与本地记录。
    ///
    /// 默认走系统回收站；永久删除必须由调用方传入与条目标题一致的确认词。
    /// 运行中的资源一律拒绝，避免文件被占用导致删到一半。
    pub fn delete_item(
        &self,
        id: &str,
        mode: DeleteMode,
        confirm: Option<&str>,
    ) -> ApiResult<DeleteItemDto> {
        let item = self.require_item(id)?;

        if item.kind == ItemKind::Game {
            if self.launcher.running_pid(&item.id)?.is_some() {
                return Err(AppError::Validation(format!(
                    "游戏正在运行，请先退出再删除：{}",
                    item.title
                )));
            }
        } else if self
            .player
            .active()?
            .iter()
            .any(|session| session.item_id == item.id)
        {
            return Err(AppError::Validation(
                "该条目正在播放，请先关闭播放器再删除".to_owned(),
            ));
        }

        let settings = self.settings.load()?;
        let root = match item.kind {
            ItemKind::Anime => settings.anime_root.clone(),
            ItemKind::Game => settings.game_root.clone(),
        }
        .ok_or_else(|| AppError::Validation("尚未配置对应根目录，无法校验删除位置".to_owned()))?;

        let folder = PathBuf::from(&item.folder_path);
        // 目录已不存在说明文件被手工移走：只清记录，不报错。
        if folder.exists() {
            crate::delete::ensure_inside(&folder, &root).map_err(AppError::Validation)?;
            for other in self.repo.list_items(None)? {
                if other.id == item.id {
                    continue;
                }
                // 扫描规则变化时可能出现嵌套目录，删外层会连带删掉别的条目。
                if PathBuf::from(&other.folder_path).starts_with(&folder) {
                    return Err(AppError::Validation(format!(
                        "该目录下还包含其它条目「{}」，已拒绝删除",
                        other.title
                    )));
                }
            }
        }

        if mode == DeleteMode::Permanent && confirm.unwrap_or_default().trim() != item.title.trim()
        {
            return Err(AppError::Validation(
                "永久删除不可恢复：请输入完整条目标题作为确认词".to_owned(),
            ));
        }

        let files_removed = folder.exists();
        crate::delete::remove_folder(&folder, mode, self.remover.as_ref())
            .map_err(AppError::Internal)?;
        self.repo.delete_item(&item.id)?;
        self.bus.publish(AppEvent::LibraryItemDeleted {
            id: item.id.clone(),
            kind: item.kind,
            title: item.title.clone(),
        });
        tracing::info!(
            item = %item.id,
            mode = mode.as_str(),
            files_removed,
            "已删除资源"
        );

        Ok(DeleteItemDto {
            item_id: item.id,
            title: item.title,
            mode: mode.as_str().to_owned(),
            folder_path: item.folder_path,
            files_removed,
        })
    }

    // ---- 插件（M4 阶段 1） ----

    /// 当前生效的插件目录：设置里有自定义值则用它，否则用默认目录。
    fn plugin_host(&self) -> ApiResult<PluginHost> {
        let configured = self.settings.load()?.plugins_dir;
        let root = configured.unwrap_or_else(|| self.default_plugins_dir.clone());
        let host = PluginHost::new(root);
        if let Err(err) = host.ensure_root() {
            tracing::warn!(error = %err, root = %host.root().display(), "创建插件目录失败");
        }
        Ok(host)
    }

    pub fn plugin_overview(&self) -> ApiResult<PluginOverviewDto> {
        let host = self.plugin_host()?;
        Ok(PluginOverviewDto {
            root: host.root().to_string_lossy().to_string(),
            plugins: self.plugin_list()?,
        })
    }

    fn plugin_list(&self) -> ApiResult<Vec<PluginDto>> {
        let mut items: Vec<PluginDto> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        for entry in self.plugins.list()? {
            seen.insert(entry.id.clone());
            let dir = Path::new(&entry.manifest_path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default();
            match load_manifest(&dir) {
                Ok(validated) => items.push(plugin_dto(&validated, entry.enabled, true)),
                Err(err) => items.push(PluginDto {
                    id: entry.id.clone(),
                    name: entry.id.clone(),
                    version: entry.version.clone(),
                    kind: entry.kind.clone(),
                    api_version: String::new(),
                    enabled: entry.enabled,
                    installed: true,
                    manifest_path: entry.manifest_path.clone(),
                    dir: dir.to_string_lossy().to_string(),
                    permissions: Vec::new(),
                    capabilities: Vec::new(),
                    mount: None,
                    error: Some(err.to_string()),
                }),
            }
        }

        for found in self.plugin_host()?.discover() {
            let Some(manifest) = found.manifest else {
                continue;
            };
            if seen.contains(&manifest.id) {
                continue;
            }
            items.push(PluginDto {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                kind: found
                    .kind
                    .map(|kind| kind.as_str().to_owned())
                    .unwrap_or_else(|| manifest.kind.clone()),
                api_version: manifest.api_version.clone(),
                enabled: false,
                installed: false,
                manifest_path: found.dir.join(MANIFEST_FILE).to_string_lossy().to_string(),
                dir: found.dir.to_string_lossy().to_string(),
                permissions: manifest.permissions.clone(),
                capabilities: manifest.capabilities.clone(),
                mount: manifest.mount.clone(),
                error: found.error,
            });
        }

        items.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(items)
    }

    /// 登记一个插件目录（不复制文件，manifest_path 指向原位置）。
    pub fn plugin_install(&self, plugin_dir: &str) -> ApiResult<PluginOverviewDto> {
        let validated = load_manifest(Path::new(plugin_dir))?;
        self.plugins.upsert(
            &validated.manifest.id,
            &validated.manifest.version,
            validated.kind.as_str(),
            &validated.manifest_path.to_string_lossy(),
        )?;
        tracing::info!(plugin = %validated.manifest.id, "已登记插件");
        self.plugin_overview()
    }

    pub fn plugin_set_enabled(&self, id: &str, enabled: bool) -> ApiResult<PluginOverviewDto> {
        self.plugins.set_enabled(id, enabled)?;
        tracing::info!(plugin = id, enabled, "插件启停变更");
        self.plugin_overview()
    }

    /// 从注册表移除；插件文件保留，便于重新安装。
    pub fn plugin_uninstall(&self, id: &str) -> ApiResult<PluginOverviewDto> {
        self.plugins.remove(id)?;
        tracing::info!(plugin = id, "已卸载插件（文件保留）");
        self.plugin_overview()
    }

    /// 把插件根目录里清单合法的插件登记进注册表（默认禁用）。
    fn sync_discovered_plugins(&self) {
        let Ok(host) = self.plugin_host() else {
            return;
        };
        for found in host.discover() {
            if found.error.is_some() {
                continue;
            }
            let Some(manifest) = found.manifest else {
                continue;
            };
            match self.plugins.get(&manifest.id) {
                Ok(Some(_)) => {}
                Ok(None) => {
                    let kind = found
                        .kind
                        .map(|value| value.as_str())
                        .unwrap_or(manifest.kind.as_str());
                    let path = found.dir.join(MANIFEST_FILE).to_string_lossy().to_string();
                    if let Err(err) =
                        self.plugins
                            .upsert(&manifest.id, &manifest.version, kind, &path)
                    {
                        tracing::warn!(plugin = %manifest.id, error = %err, "登记插件失败");
                    }
                }
                Err(err) => tracing::warn!(error = %err, "读取插件注册表失败"),
            }
        }
    }

    /// 取 UI 插件入口源码：要求已安装、已启用、kind=ui，入口不超过 512 KiB。
    pub fn plugin_ui_entry(&self, id: &str) -> ApiResult<PluginEntryDto> {
        let validated = self.require_enabled_plugin(id)?;
        if validated.kind != PluginKind::Ui {
            return Err(AppError::Validation(format!(
                "插件 {id} 不是 ui 类型，不能加载到界面"
            )));
        }
        const MAX_SOURCE_BYTES: usize = 512 * 1024;
        let source = std::fs::read_to_string(&validated.entry_path).map_err(|err| {
            AppError::Internal(format!(
                "读取插件入口失败（{}）：{err}",
                validated.entry_path.display()
            ))
        })?;
        if source.len() > MAX_SOURCE_BYTES {
            return Err(AppError::Validation(format!(
                "插件入口超过 {MAX_SOURCE_BYTES} 字节，已拒绝加载"
            )));
        }
        let mount = validated.manifest.mount.clone().unwrap_or_default();
        self.bus
            .publish(AppEvent::PluginLifecycleLoaded { id: id.to_owned() });
        Ok(PluginEntryDto {
            id: id.to_owned(),
            api_version: validated.manifest.api_version.clone(),
            mount,
            source,
        })
    }

    /// 宿主 API：读取条目列表（需 read:library）。
    pub fn plugin_read_items(&self, id: &str) -> ApiResult<Vec<ItemSummaryDto>> {
        let validated = self.require_enabled_plugin(id)?;
        if !validated.has(Permission::ReadLibrary) {
            return Err(AppError::Validation(format!(
                "插件 {id} 未声明 read:library 权限"
            )));
        }
        self.list_items(None)
    }

    /// 宿主 API：插件发事件（需 emit:events），统一走主干道。
    pub fn plugin_emit(&self, id: &str, name: &str, payload: serde_json::Value) -> ApiResult<()> {
        let validated = self.require_enabled_plugin(id)?;
        if !validated.has(Permission::EmitEvents) {
            return Err(AppError::Validation(format!(
                "插件 {id} 未声明 emit:events 权限"
            )));
        }
        let name = name.trim();
        if name.is_empty()
            || name.len() > 64
            || !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
        {
            return Err(AppError::Validation(
                "事件名只能包含字母、数字、点、下划线、短横线（≤64 字符）".to_owned(),
            ));
        }
        self.bus.publish(AppEvent::PluginEmitted {
            id: id.to_owned(),
            name: name.to_owned(),
            payload,
        });
        Ok(())
    }

    /// 前端卸载 UI 插件时上报，用于生命周期事件。
    pub fn plugin_ui_unloaded(&self, id: &str) -> ApiResult<()> {
        self.bus
            .publish(AppEvent::PluginLifecycleUnloaded { id: id.to_owned() });
        Ok(())
    }

    /// 执行一次能力插件（M4 阶段 2）。
    ///
    /// 隔离由 plugin-host 的沙箱保证：指令预算、内存上限、导入白名单；
    /// 这里只做「已安装 / 已启用 / 类型正确」的前置校验与宿主能力注入。
    pub fn plugin_invoke(
        self: &Arc<Self>,
        id: &str,
        input: serde_json::Value,
    ) -> ApiResult<PluginInvocationDto> {
        let validated = self.require_enabled_plugin(id)?;
        if validated.kind != PluginKind::Capability {
            return Err(AppError::Validation(format!(
                "插件 {id} 不是 capability 类型，不能执行"
            )));
        }
        let input_text = serde_json::to_string(&input)
            .map_err(|err| AppError::Validation(format!("输入无法序列化：{err}")))?;
        let bridge = Arc::new(ServicesBridge {
            services: Arc::clone(self),
            id: id.to_owned(),
        });
        let report = self
            .plugin_runtime
            .invoke(&validated, &input_text, bridge)?;
        let output: serde_json::Value = serde_json::from_str(&report.output)
            .map_err(|err| AppError::Internal(format!("插件输出无法解析：{err}")))?;
        tracing::info!(
            plugin = id,
            duration_ms = report.duration_ms,
            fuel = report.fuel_used,
            "能力插件执行完成"
        );
        Ok(PluginInvocationDto {
            id: id.to_owned(),
            output,
            logs: report.logs,
            duration_ms: report.duration_ms,
            fuel_used: report.fuel_used,
        })
    }

    fn require_enabled_plugin(&self, id: &str) -> ApiResult<ValidatedPlugin> {
        let entry = self
            .plugins
            .get(id)?
            .ok_or_else(|| AppError::Validation(format!("插件未安装：{id}")))?;
        if !entry.enabled {
            return Err(AppError::Validation(format!("插件未启用：{id}")));
        }
        let dir = Path::new(&entry.manifest_path)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Ok(load_manifest(&dir)?)
    }

    // ---- 自动匹配 ----

    /// 扫描后自动匹配未绑定条目：搜索 → 绑定 → 缓存元数据 → 拉取云端。
    ///
    /// 保守策略：只有 `best_match` 足够自信且不歧义才绑定，其余记入 `unmatched`。
    pub async fn auto_match_and_pull(&self) -> ApiResult<AutoMatchReportDto> {
        if !self.client.is_authorized() {
            return Err(AppError::Sync(bangumi_sync::SyncError::NotAuthorized));
        }
        let started = std::time::Instant::now();
        let items = self.repo.list_items(None)?;
        let unbound: Vec<&LibraryItem> = items
            .iter()
            .filter(|item| item.bangumi_subject_id.is_none())
            .collect();
        let mut report = AutoMatchReportDto {
            unbound_scanned: unbound.len(),
            ..Default::default()
        };

        for item in unbound {
            // Bangumi 类型：2 动画 / 4 游戏。
            let subject_type = match item.kind {
                ItemKind::Anime => 2,
                ItemKind::Game => 4,
            };
            // 候选标题：先条目名（文件夹名），再目录内文件名派生的系列名。
            // 「壳目录」场景里文件夹名是用户随手起的，真正能对上 Bangumi 的往往是文件名。
            let mut queries: Vec<String> = vec![item.title.clone()];
            for hint in self.series_title_hints(item) {
                if !queries
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&hint))
                {
                    queries.push(hint);
                }
            }

            let mut matched: Option<(String, bangumi_sync::MatchCandidate)> = None;
            let mut any_hits = false;
            let mut last_error: Option<String> = None;
            let mut network_down = false;

            for query in &queries {
                let (hits, search_error) = self.search_merged(query, subject_type).await;
                if let Some(err) = &search_error {
                    if err.contains("网络请求失败") && hits.is_empty() {
                        network_down = true;
                        break;
                    }
                    last_error = Some(err.clone());
                }
                if hits.is_empty() {
                    continue;
                }
                any_hits = true;
                // 罗马音标题靠内置对照表换成了日文名去搜索，打分也必须用同一个名字：
                // 否则搜到了正确的条目，也会因为「罗马音 vs 日文」相似度为 0 而被判不匹配。
                let scoring_title = bangumi_sync::native_title_for(query)
                    .map(str::to_owned)
                    .unwrap_or_else(|| query.clone());
                if let Some(candidate) = bangumi_sync::best_match(&scoring_title, &hits) {
                    matched = Some((query.clone(), candidate));
                    break;
                }
            }

            if network_down {
                report.messages.push(format!(
                    "网络不可用，已跳过剩余 {} 个条目的搜索；请检查代理或稍后重试",
                    report.unbound_scanned.saturating_sub(report.bound)
                ));
                break;
            }

            match matched {
                Some((query, candidate)) => {
                    match self
                        .repo
                        .bind_bangumi_subject(&item.id, Some(candidate.hit.id))
                    {
                        Ok(bound) => {
                            report.bound += 1;
                            tracing::info!(
                                title = %item.title,
                                matched_by = %query,
                                subject = candidate.hit.id,
                                score = candidate.score,
                                "自动匹配并绑定 Bangumi 条目"
                            );
                            self.bus.publish(AppEvent::LibraryItemBound {
                                id: bound.id.clone(),
                                bangumi_subject_id: bound.bangumi_subject_id,
                            });
                            // 只拉刚绑定的这一条：全部重拉一次会随条目数线性变慢。
                            match self.refresh_item(&item.id).await {
                                Ok(()) => {
                                    report.metadata_cached += 1;
                                    report.pulled += 1;
                                }
                                Err(err) => report.messages.push(format!(
                                    "{} 绑定后拉取失败：{}",
                                    item.title,
                                    short_reason(&err.to_string(), 60)
                                )),
                            }
                        }
                        Err(err) => report.unmatched.push(format!(
                            "{}（绑定失败：{}）",
                            item.title,
                            short_reason(&err.to_string(), 60)
                        )),
                    }
                }
                None => {
                    let reason = if any_hits {
                        "候选相似度不足或存在歧义".to_owned()
                    } else {
                        last_error.unwrap_or_else(|| "搜索无结果".to_owned())
                    };
                    report.unmatched.push(format!(
                        "{}（{}）",
                        item.title,
                        short_reason(&reason, 60)
                    ));
                }
            }
            // 轻微节流，避免搜索接口限流（600ms 会让条目多时体感明显变慢）。
            tokio::time::sleep(std::time::Duration::from_millis(180)).await;
        }

        // 已绑定但还没有缓存元数据的条目补一次，避免老数据一直空着。
        for item in &items {
            let Some(subject_id) = item.bangumi_subject_id else {
                continue;
            };
            if self.metadata_index.get(subject_id)?.is_some() {
                continue;
            }
            match self.cache_metadata(&item.id).await {
                Ok(_) => report.metadata_cached += 1,
                Err(err) => report
                    .messages
                    .push(format!("{} 元数据缓存失败：{err}", item.title)),
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // 已绑定条目的云端数据只在启动时整体拉一次（plan.md 6.6）；
        // 这里再全量拉一遍会让「扫描 + 自动匹配」随条目数线性变慢，冲突也会重复生成。
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis(),
            unbound = report.unbound_scanned,
            bound = report.bound,
            "自动匹配完成"
        );
        Ok(report)
    }

    /// 立即拉取单个条目的云端收藏与进度（详情页「拉取云端」用）。
    ///
    /// 未绑定时给出可读的校验错误；未授权由上层转成「请先授权」的提示。
    pub async fn pull_item_now(&self, item_id: &str) -> ApiResult<ItemSummaryDto> {
        self.refresh_item(item_id).await?;
        self.get_item(item_id).map(|detail| detail.item)
    }

    /// 用多个关键词变体搜索并合并结果；返回（去重后的命中, 最后一次错误）。
    async fn search_merged(
        &self,
        title: &str,
        subject_type: i64,
    ) -> (Vec<bangumi_sync::SubjectSearchHit>, Option<String>) {
        let mut merged: Vec<bangumi_sync::SubjectSearchHit> = Vec::new();
        let mut seen: HashSet<i64> = HashSet::new();
        let mut last_error = None;
        for keyword in bangumi_sync::search_keywords(title) {
            match self.client.search_subjects(keyword, subject_type).await {
                Ok(hits) => {
                    for hit in hits {
                        if seen.insert(hit.id) {
                            merged.push(hit);
                        }
                    }
                }
                Err(err) => last_error = Some(err.to_string()),
            }
            if !merged.is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        }
        (merged, last_error)
    }

    /// 手动搜索 Bangumi 条目（详情页「搜索」用）。`keyword` 为空时用条目标题。
    pub async fn search_bangumi(
        &self,
        item_id: &str,
        keyword: Option<String>,
    ) -> ApiResult<Vec<SubjectSearchHitDto>> {
        if !self.client.is_authorized() {
            return Err(AppError::Sync(bangumi_sync::SyncError::NotAuthorized));
        }
        let item = self.require_item(item_id)?;
        let subject_type = match item.kind {
            ItemKind::Anime => 2,
            ItemKind::Game => 4,
        };
        let query = keyword
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| item.title.clone());
        let (hits, search_error) = self.search_merged(&query, subject_type).await;
        if hits.is_empty() {
            if let Some(error) = search_error {
                return Err(AppError::Sync(bangumi_sync::SyncError::Api(error)));
            }
        }
        Ok(hits
            .into_iter()
            .map(|hit| SubjectSearchHitDto {
                id: hit.id,
                name: hit.name,
                name_cn: hit.name_cn,
                score: hit.score,
            })
            .collect())
    }

    /// 绑定后立即拉取该条目的云端收藏并缓存元数据。
    pub async fn refresh_item(&self, item_id: &str) -> ApiResult<()> {
        if !self.client.is_authorized() {
            return Err(AppError::Sync(bangumi_sync::SyncError::NotAuthorized));
        }
        let item = self.require_item(item_id)?;
        let Some(subject_id) = item.bangumi_subject_id else {
            return Err(AppError::Validation("条目尚未绑定 Bangumi ID".to_owned()));
        };
        let local = LocalCollection {
            item_id: item.id.clone(),
            subject_id,
            collection_status: item.collection_status,
            rating: item.rating,
            collection_updated_at: item.collection_updated_at.clone(),
        };
        let local_progress = self.local_progress(&item, subject_id)?;
        let report = self.sync_engine.pull_item(&local, &local_progress).await?;
        for conflict in &report.conflicts {
            self.bus.publish(AppEvent::SyncConflictDetected {
                item_id: item.id.clone(),
                field: conflict.field.clone(),
            });
        }
        if report.imported_collection {
            self.bus.publish(AppEvent::LibraryItemUpdated {
                id: item.id.clone(),
                kind: item.kind,
                title: item.title.clone(),
                folder_path: item.folder_path.clone(),
            });
        }
        self.cache_metadata(item_id).await?;
        Ok(())
    }

    // ---- Bangumi OAuth ----

    pub fn bangumi_status(&self) -> ApiResult<BangumiStatusDto> {
        let token = self.token_store.load()?;
        let using_builtin = self.using_builtin_app();
        let config = self.settings.bangumi_config()?;
        Ok(BangumiStatusDto {
            authorized: token.is_some(),
            expires_at: token.and_then(|value| value.expires_at),
            has_credentials: config.is_some(),
            // 内置应用不把凭据回传给前端，界面上也就无从展示或篡改。
            client_id: if using_builtin {
                None
            } else {
                config.as_ref().map(|value| value.client_id.clone())
            },
            redirect_uri: config
                .as_ref()
                .map(|value| value.redirect_uri.clone())
                .filter(|_| !using_builtin),
            using_builtin_app: using_builtin,
        })
    }

    /// 当前构建是否内置了 Bangumi 应用（内置后凭据不可修改）。
    fn using_builtin_app(&self) -> bool {
        self.settings.builtin_bangumi_embedded()
    }

    /// 当前登录账号的资料（昵称、头像、ID）；未授权时返回 authorized=false。
    pub async fn bangumi_account(&self) -> ApiResult<AccountDto> {
        let token = self.token_store.load()?;
        let Some(token) = token else {
            return Ok(AccountDto {
                authorized: false,
                id: None,
                username: None,
                nickname: None,
                avatar_url: None,
                expires_at: None,
            });
        };
        let profile = self.client.fetch_me().await.map_err(|err| {
            tracing::warn!(error = %err, "读取 Bangumi 账号资料失败");
            err
        })?;
        Ok(AccountDto {
            authorized: true,
            id: Some(profile.id),
            username: Some(profile.username),
            nickname: Some(profile.nickname),
            avatar_url: profile.avatar_url,
            expires_at: token.expires_at,
        })
    }

    /// 搜索结果的右侧预览：条目概览 + 标签 + 关联条目 + 封面。
    ///
    /// 关联条目和封面都是「有更好」的附加信息，单独失败不影响主体字段。
    pub async fn bangumi_subject_preview(&self, subject_id: i64) -> ApiResult<SubjectPreviewDto> {
        if !self.client.is_authorized() {
            return Err(AppError::Sync(bangumi_sync::SyncError::NotAuthorized));
        }
        let meta = self.client.fetch_subject_metadata(subject_id).await?;
        let text = |key: &str| {
            meta.get(key)
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };

        let related = match self.client.fetch_related_subjects(subject_id).await {
            Ok(rows) => rows
                .into_iter()
                .map(|row| PreviewRelatedDto {
                    id: row.id,
                    name: row.name,
                    name_cn: row.name_cn,
                    relation: row.relation,
                    subject_type: row.subject_type,
                })
                .collect(),
            Err(err) => {
                tracing::debug!(error = %err, subject = subject_id, "关联条目读取失败");
                Vec::new()
            }
        };

        let cover = match bangumi_sync::cover_url(&meta) {
            Some(url) => match self.client.fetch_bytes(url).await {
                Ok(bytes) if !bytes.is_empty() => Some(crate::cover::data_url(&bytes)),
                Ok(_) => None,
                Err(err) => {
                    tracing::debug!(error = %err, subject = subject_id, "预览封面下载失败");
                    None
                }
            },
            None => None,
        };

        let tags = meta
            .get("tags")
            .and_then(serde_json::Value::as_array)
            .map(|rows| {
                rows.iter()
                    .take(12)
                    .filter_map(|row| {
                        Some(PreviewTagDto {
                            name: row.get("name")?.as_str()?.to_owned(),
                            count: row.get("count").and_then(serde_json::Value::as_i64)?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let rating = meta.get("rating");
        Ok(SubjectPreviewDto {
            id: subject_id,
            name: text("name"),
            name_cn: text("name_cn"),
            summary: text("summary"),
            subject_type: meta
                .get("type")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
            date: meta
                .get("date")
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            eps: meta.get("eps").and_then(serde_json::Value::as_i64),
            score: rating
                .and_then(|row| row.get("score"))
                .and_then(serde_json::Value::as_f64),
            rank: rating
                .and_then(|row| row.get("rank"))
                .and_then(serde_json::Value::as_i64),
            rating_total: rating
                .and_then(|row| row.get("total"))
                .and_then(serde_json::Value::as_i64)
                .unwrap_or_default(),
            tags,
            related,
            cover,
            url: format!("https://bgm.tv/subject/{subject_id}"),
        })
    }

    /// 用系统默认浏览器打开 http(s) 链接。
    pub fn open_external(&self, url: &str) -> ApiResult<()> {
        let trimmed = url.trim();
        if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
            return Err(AppError::Validation("只允许打开 http(s) 链接".to_owned()));
        }
        std::process::Command::new("cmd")
            .args(["/C", "start", "", trimmed])
            .spawn()
            .map_err(|err| AppError::Internal(format!("打开浏览器失败：{err}")))?;
        Ok(())
    }

    /// 丢弃一条待推送记录（只删本地队列，不影响云端）。
    pub fn discard_sync_entry(&self, id: &str) -> ApiResult<bool> {
        let removed = self.sync_queue.delete(id)?;
        if !removed {
            return Err(AppError::Validation(
                "队列记录不存在（可能已被推送或清理）".to_owned(),
            ));
        }
        tracing::info!(entry = id, "已移除同步队列记录");
        Ok(true)
    }

    /// 保存 App ID / Secret / 回调地址（三者全空则清除）。
    pub fn save_bangumi_config(&self, input: &BangumiConfigInput) -> ApiResult<BangumiStatusDto> {
        self.settings.save_bangumi(
            &input.client_id,
            &input.client_secret,
            &input.redirect_uri,
            &input.proxy,
        )?;
        self.bangumi_status()
    }

    pub fn bangumi_logout(&self) -> ApiResult<BangumiStatusDto> {
        self.token_store.clear()?;
        tracing::info!("已清除 Bangumi 授权");
        self.bus
            .publish(AppEvent::BangumiAuthChanged { authorized: false });
        self.bangumi_status()
    }

    /// 走完整 OAuth：打开浏览器 → 本地回调 → 换 token → 存入凭据管理器。
    pub async fn bangumi_authorize(&self) -> ApiResult<BangumiStatusDto> {
        let config = self.config_provider.oauth_config()?;
        let state = uuid::Uuid::new_v4().to_string();
        let url = bangumi_sync::build_authorize_url(&config, &state);
        tracing::info!(redirect = %config.redirect_uri, "开始 Bangumi 授权");
        bangumi_sync::open_browser(&url)?;

        let redirect = config.redirect_uri.clone();
        let expected = state.clone();
        // 等回调是阻塞操作，放到阻塞线程；超时 180 秒。
        let code = tauri::async_runtime::spawn_blocking(move || {
            bangumi_sync::wait_for_callback(
                &redirect,
                &expected,
                std::time::Duration::from_secs(180),
            )
        })
        .await
        .map_err(|err| AppError::Join(err.to_string()))??;

        let token = bangumi_sync::http_client::exchange_code_with_client(
            &config,
            &code,
            self.config_provider.proxy().as_deref(),
        )
        .await?;
        self.token_store.save(token)?;
        tracing::info!("Bangumi 授权成功");
        self.bus
            .publish(AppEvent::BangumiAuthChanged { authorized: true });
        self.bangumi_status()
    }

    /// 兜底：浏览器已跳转但本地监听没收到时，粘贴回调地址（或授权码）完成授权。
    pub async fn bangumi_complete_manual(&self, callback: &str) -> ApiResult<BangumiStatusDto> {
        let config = self.config_provider.oauth_config()?;
        let code = bangumi_sync::extract_code(callback)
            .ok_or_else(|| AppError::Validation("未能从输入中解析出授权码".to_owned()))?;
        let token = bangumi_sync::http_client::exchange_code_with_client(
            &config,
            &code,
            self.config_provider.proxy().as_deref(),
        )
        .await?;
        self.token_store.save(token)?;
        tracing::info!("Bangumi 授权成功（手动回调）");
        self.bus
            .publish(AppEvent::BangumiAuthChanged { authorized: true });
        self.bangumi_status()
    }

    /// 播放器路径或参数模板变化时替换后端；两者都参与比较，避免改了模板却复用旧后端。
    fn ensure_player_backend(&self, choice: PlayerBackendChoice) -> ApiResult<()> {
        let mut guard = self
            .player_backend_path
            .lock()
            .map_err(|_| AppError::Internal("播放器后端状态不可用".to_owned()))?;
        if guard.as_ref() == Some(&choice) {
            return Ok(());
        }
        let backend: Arc<dyn PlayerBackend> = match &choice {
            PlayerBackendChoice::System => Arc::new(SystemDefaultPlayer::new()),
            PlayerBackendChoice::External(path, template) => {
                Arc::new(ExternalPlayerAdapter::new(path.clone()).with_arguments(template))
            }
        };
        self.player.set_backend(backend)?;
        *guard = Some(choice);
        Ok(())
    }

    fn require_item(&self, id: &str) -> ApiResult<LibraryItem> {
        self.repo
            .get_item(id)?
            .ok_or_else(|| AppError::Core(CoreError::ItemNotFound(id.to_owned())))
    }

    fn to_summary(&self, item: &LibraryItem) -> ItemSummaryDto {
        let probe = self.probe(item);
        summary_from(item, &probe)
    }

    /// 目录内文件名派生的系列标题（最多 2 个），用于「壳目录」匹配兜底。
    fn series_title_hints(&self, item: &LibraryItem) -> Vec<String> {
        if item.kind != ItemKind::Anime {
            return Vec::new();
        }
        let folder = PathBuf::from(&item.folder_path);
        match self
            .anime
            .dominant_series_titles(&folder, item.file_filter.as_deref(), 2)
        {
            Ok(titles) => titles,
            Err(err) => {
                tracing::warn!(path = %item.folder_path, error = %err, "读取目录内文件名失败");
                Vec::new()
            }
        }
    }

    /// 探测失败不应让列表/详情整体失败：记录告警并退化为空媒体信息。
    fn probe(&self, item: &LibraryItem) -> MediaProbe {
        let folder = PathBuf::from(&item.folder_path);
        let result = match item.kind {
            ItemKind::Anime => self.anime.probe(&folder, item.file_filter.as_deref()),
            ItemKind::Game => self.game.probe(&folder),
        };
        match result {
            Ok(probe) => probe,
            Err(err) => {
                tracing::warn!(path = %item.folder_path, error = %err, "媒体探测失败");
                MediaProbe::empty()
            }
        }
    }

    fn settings_to_dto(&self, settings: AppSettings) -> SettingsDto {
        // 内置应用时既不回传 App ID，也不回传回调地址，避免前端出现可编辑入口。
        let builtin = self.settings.builtin_bangumi_embedded();
        SettingsDto {
            anime_root: settings
                .anime_root
                .map(|path| path.to_string_lossy().to_string()),
            game_root: settings
                .game_root
                .map(|path| path.to_string_lossy().to_string()),
            metadata_cache_dir: settings.metadata_cache_dir.to_string_lossy().to_string(),
            default_metadata_cache_dir: default_metadata_dir().to_string_lossy().to_string(),
            metadata_cache_dir_is_default: settings.metadata_cache_dir_is_default,
            player_path: settings
                .player_path
                .map(|path| path.to_string_lossy().to_string()),
            player_args: settings
                .player_args
                .unwrap_or_else(|| crate::settings::DEFAULT_PLAYER_ARGS.to_owned()),
            bangumi_client_id: if builtin {
                None
            } else {
                settings.bangumi_client_id
            },
            bangumi_has_secret: if builtin {
                true
            } else {
                settings.bangumi_client_secret.is_some()
            },
            bangumi_builtin: builtin,
            bangumi_redirect_uri: if builtin {
                None
            } else {
                settings.bangumi_redirect_uri
            },
            bangumi_proxy: settings.bangumi_proxy,
            bangumi_default_redirect_uri: DEFAULT_BANGUMI_REDIRECT_URI.to_owned(),
            plugins_dir: settings
                .plugins_dir
                .map(|path| path.to_string_lossy().to_string()),
            default_plugins_dir: self.default_plugins_dir.to_string_lossy().to_string(),
        }
    }
}

/// 批量刷新时单条的结果，用于汇总与进度事件。
enum RefreshOutcome {
    Refreshed,
    Skipped,
    Failed(String),
}

/// 递归统计目录下的文件数与总字节数（跳过不可读项）。
fn dir_usage(dir: &Path) -> (usize, u64) {
    let mut files = 0usize;
    let mut bytes = 0u64;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (files, bytes);
    };
    for entry in entries.flatten() {
        let path = entry.path();
        match entry.metadata() {
            Ok(meta) if meta.is_dir() => {
                let (inner_files, inner_bytes) = dir_usage(&path);
                files += inner_files;
                bytes += inner_bytes;
            }
            Ok(meta) => {
                files += 1;
                bytes += meta.len();
            }
            Err(_) => {}
        }
    }
    (files, bytes)
}

/// 把错误压成一行短提示，供列表/横幅展示；完整信息在日志里。
fn short_reason(raw: &str, limit: usize) -> String {
    let line = raw.lines().next().unwrap_or(raw).trim();
    if line.chars().count() <= limit {
        return line.to_owned();
    }
    let mut out: String = line.chars().take(limit).collect();
    out.push('…');
    out
}

fn plugin_dto(validated: &ValidatedPlugin, enabled: bool, installed: bool) -> PluginDto {
    PluginDto {
        id: validated.manifest.id.clone(),
        name: validated.manifest.name.clone(),
        version: validated.manifest.version.clone(),
        kind: validated.kind.as_str().to_owned(),
        api_version: validated.manifest.api_version.clone(),
        enabled,
        installed,
        manifest_path: validated.manifest_path.to_string_lossy().to_string(),
        dir: validated.dir.to_string_lossy().to_string(),
        permissions: validated
            .permissions
            .iter()
            .map(|permission| permission.as_str().to_owned())
            .collect(),
        capabilities: validated.manifest.capabilities.clone(),
        mount: validated.manifest.mount.clone(),
        error: None,
    }
}

fn summary_from(item: &LibraryItem, probe: &MediaProbe) -> ItemSummaryDto {
    ItemSummaryDto {
        id: item.id.clone(),
        kind: item.kind,
        title: item.title.clone(),
        bangumi_subject_id: item.bangumi_subject_id,
        folder_path: item.folder_path.clone(),
        created_at: item.created_at.clone(),
        updated_at: item.updated_at.clone(),
        collection_status: item.collection_status,
        rating: item.rating,
        collection_updated_at: item.collection_updated_at.clone(),
        media: MediaSummaryDto {
            file_count: probe.file_count,
            episode_hint: probe.episode_hint.clone(),
        },
    }
}

fn parse_i64(raw: Option<&str>) -> Option<i64> {
    raw.and_then(|value| value.trim().parse::<i64>().ok())
}

fn binding_dto(item: &LibraryItem, config: &launcher::GameLaunchConfig) -> GameBindingDto {
    let folder = PathBuf::from(&item.folder_path);
    let resolved = config.resolved_exe(&folder);
    GameBindingDto {
        exe_path: config.exe_path.clone(),
        exe_exists: resolved.is_file(),
        resolved_exe_path: resolved.to_string_lossy().to_string(),
        args: config.args.clone(),
        working_dir: config.working_dir.clone(),
        last_launched_at: config.last_launched_at.clone(),
        last_pid: config.last_pid,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lagm-services-{}-{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        fs::create_dir_all(&dir).expect("base");
        dir
    }

    /// 测试一律注入「不写系统回收站」的删除后端：
    /// `cargo test` 每跑一次都往 `$Recycle.Bin` 丢临时目录，会让回收站迅速膨胀。
    fn test_remover() -> Arc<dyn crate::delete::FolderRemover> {
        Arc::new(crate::delete::NoRecycleBin::default())
    }

    fn services(base: &Path) -> Services {
        Services::build_with_remover(
            &base.join("library.db"),
            Arc::new(InMemoryTokenStore::new()),
            None,
            test_remover(),
        )
        .expect("services")
    }

    fn services_with_client(base: &Path, client: Arc<OfflineClient>) -> Services {
        Services::build_with_remover(
            &base.join("library.db"),
            Arc::new(InMemoryTokenStore::new()),
            Some(client),
            test_remover(),
        )
        .expect("services")
    }

    fn anime_item(services: &Services, base: &Path) -> (PathBuf, String) {
        let anime = base.join("anime");
        let show = anime.join("[Group] Test Show");
        fs::create_dir_all(&show).expect("show");
        fs::write(show.join("Test Show - 01.mkv"), b"x").expect("file");
        fs::write(show.join("Test Show - 02.mkv"), b"x").expect("file");

        services
            .save_settings(&SettingsInput {
                anime_root: Some(anime.to_string_lossy().into()),
                game_root: None,
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect("save");
        services.scan().expect("scan");
        let id = services.list_items(None).expect("list")[0].id.clone();
        (show, id)
    }

    #[test]
    fn clear_items_removes_records_but_keeps_files() {
        let base = temp_root("clear-items");
        let services = services(&base);
        let (show, _id) = anime_item(&services, &base);
        assert_eq!(services.list_items(None).expect("list").len(), 1);

        let removed = services.clear_items(None).expect("clear");
        assert_eq!(removed, 1);
        assert!(services.list_items(None).expect("list").is_empty());

        // 这个操作的承诺是「只删记录」：磁盘文件一个都不能少。
        assert!(
            show.join("Test Show - 01.mkv").is_file(),
            "清空条目不得删除磁盘文件"
        );
        assert!(show.join("Test Show - 02.mkv").is_file());

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn end_to_end_scan_bind_and_read() {
        let base = temp_root("e2e");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);

        let items = services.list_items(None).expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Test Show");
        assert_eq!(items[0].media.file_count, 2);
        assert_eq!(items[0].media.episode_hint.as_deref(), Some("E01–E02"));

        let bound = services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        assert_eq!(bound.bangumi_subject_id, Some(400602));

        let detail = services.get_item(&id).expect("detail");
        assert_eq!(detail.files.len(), 2);
        assert_eq!(detail.files[0].episode, Some(1));

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn invalid_subject_id_is_rejected() {
        let base = temp_root("subject");
        let services = services(&base);
        let err = services
            .bind_bangumi_subject("any", Some(0))
            .expect_err("应当拒绝");
        assert_eq!(err.code(), "validation_error");
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn scan_without_roots_reports_hint_instead_of_failing() {
        let base = temp_root("noroots");
        let services = services(&base);
        let summary = services.scan().expect("scan");
        assert_eq!(summary.messages.len(), 2);
        assert_eq!(summary.total_created, 0);
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn play_rejects_unknown_file() {
        let base = temp_root("nofile");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);

        let err = services
            .play_episode(&id, "missing.mkv")
            .expect_err("文件不存在");
        assert_eq!(err.code(), "item_not_found");
        fs::remove_dir_all(base).ok();
    }

    #[cfg(windows)]
    #[test]
    fn play_starts_session_and_manual_correction_updates_progress() {
        let base = temp_root("play");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);
        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: None,
                player_path: Some("C:\\Windows\\System32\\where.exe".into()),
                ..Default::default()
            })
            .expect("player");

        let session = services
            .play_episode(&id, "Test Show - 01.mkv")
            .expect("play");
        assert_eq!(session.episode, 1);
        assert!(session.pid > 0);
        assert_eq!(services.active_sessions().expect("active").len(), 1);

        let progress = services.list_progress(&id).expect("progress");
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].watched_seconds, 0);

        let corrected = services.set_progress(&id, 1, 600).expect("correct");
        assert_eq!(corrected.watched_seconds, 600);
        assert_eq!(
            services.list_progress(&id).expect("progress")[0].watched_seconds,
            600
        );

        // 让短命进程退出，确认会话被收尾。
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            services.tick().expect("tick");
            if services.active_sessions().expect("active").is_empty() {
                break;
            }
        }
        assert!(services.active_sessions().expect("active").is_empty());

        fs::remove_dir_all(base).ok();
    }

    /// 已有进度时播放应带续播起点，播放中校正也不能被会话收尾覆盖。
    #[cfg(windows)]
    #[test]
    fn play_resumes_from_progress_and_keeps_manual_correction() {
        let base = temp_root("resume");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);
        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: None,
                player_path: Some("C:\\Windows\\System32\\where.exe".into()),
                player_args: Some("\"{file}\" /seek={seconds}".into()),
                plugins_dir: None,
            })
            .expect("player");

        services.set_progress(&id, 1, 600).expect("seed");
        let session = services
            .play_episode(&id, "Test Show - 01.mkv")
            .expect("play");
        assert_eq!(session.start_seconds, 600, "应从已有进度续播");

        // 播放中再校正一次，收尾时不能被写回起点。
        services.set_progress(&id, 1, 900).expect("correct");

        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            services.tick().expect("tick");
            if services.active_sessions().expect("active").is_empty() {
                break;
            }
        }
        let row = services
            .list_progress(&id)
            .expect("progress")
            .into_iter()
            .find(|row| row.episode == 1)
            .expect("row");
        assert!(
            row.watched_seconds >= 900,
            "收尾后进度不应低于手动校正值：{row:?}"
        );

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn set_progress_rejects_zero_episode() {
        let base = temp_root("episode");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);
        let err = services.set_progress(&id, 0, 10).expect_err("集号非法");
        assert_eq!(err.code(), "validation_error");
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn game_binding_is_required_before_start() {
        let base = temp_root("gamebind");
        let services = services(&base);
        let games = base.join("games");
        let game_dir = games.join("Test Game");
        fs::create_dir_all(&game_dir).expect("game");
        fs::write(game_dir.join("game.exe"), b"x").expect("exe");

        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: Some(games.to_string_lossy().into()),
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect("save");
        services.scan().expect("scan");
        let id = services.list_items(Some(ItemKind::Game)).expect("list")[0]
            .id
            .clone();

        let status = services.game_status(&id).expect("status");
        assert!(status.binding.is_none());
        assert_eq!(
            services.start_game(&id).expect_err("未绑定").code(),
            "binding_missing"
        );

        let bound = services
            .set_game_binding(&id, &game_dir.join("game.exe").to_string_lossy())
            .expect("bind");
        let binding = bound.binding.expect("binding");
        assert_eq!(binding.exe_path, "game.exe");
        assert!(binding.exe_exists);

        fs::remove_dir_all(base).ok();
    }

    #[cfg(windows)]
    #[test]
    fn start_and_stop_tracking_of_real_process() {
        let base = temp_root("run");
        let services = services(&base);
        let games = base.join("games");
        let game_dir = games.join("Test Game");
        fs::create_dir_all(&game_dir).expect("game");

        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: Some(games.to_string_lossy().into()),
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect("save");
        // 用 where.exe 当作游戏本体：接收任意参数后立即退出。
        fs::copy(
            "C:\\Windows\\System32\\where.exe",
            game_dir.join("game.exe"),
        )
        .expect("copy exe");
        services.scan().expect("scan");
        let id = services.list_items(Some(ItemKind::Game)).expect("list")[0]
            .id
            .clone();
        services
            .set_game_binding(&id, &game_dir.join("game.exe").to_string_lossy())
            .expect("bind");

        let running = services.start_game(&id).expect("start");
        assert!(running.pid > 0);
        assert_eq!(services.running_games().expect("running").len(), 1);
        assert_eq!(
            services.game_status(&id).expect("status").running_pid,
            Some(running.pid)
        );

        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            services.tick().expect("tick");
            if services.running_games().expect("running").is_empty() {
                break;
            }
        }
        assert!(
            services.running_games().expect("running").is_empty(),
            "进程退出后应变为未运行"
        );
        assert_eq!(services.game_status(&id).expect("status").running_pid, None);

        fs::remove_dir_all(base).ok();
    }

    /// 组合根必须把 launcher 的事件原样送上主干道，前端才能靠事件刷新状态。
    #[cfg(windows)]
    #[test]
    fn launcher_events_reach_the_bus() {
        let base = temp_root("events");
        let services = services(&base);
        let games = base.join("games");
        let game_dir = games.join("Test Game");
        fs::create_dir_all(&game_dir).expect("game");
        fs::copy(
            "C:\\Windows\\System32\\where.exe",
            game_dir.join("game.exe"),
        )
        .expect("copy exe");

        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: Some(games.to_string_lossy().into()),
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect("save");
        services.scan().expect("scan");
        let id = services.list_items(Some(ItemKind::Game)).expect("list")[0]
            .id
            .clone();
        services
            .set_game_binding(&id, &game_dir.join("game.exe").to_string_lossy())
            .expect("bind");

        let mut receiver = services.bus().subscribe();
        services.start_game(&id).expect("start");

        let envelope = receiver
            .try_recv()
            .expect("启动事件")
            .to_envelope()
            .expect("信封");
        assert_eq!(envelope.topic, "launcher:game:started");
        assert_eq!(envelope.payload["itemId"], id);
        assert!(envelope.payload["pid"].as_u64().unwrap_or_default() > 0);

        let mut exited = false;
        for _ in 0..50 {
            std::thread::sleep(Duration::from_millis(100));
            services.tick().expect("tick");
            while let Ok(event) = receiver.try_recv() {
                if event.topic() == "launcher:game:exited" {
                    exited = true;
                }
            }
            if exited {
                break;
            }
        }
        assert!(exited, "进程退出后应发布 launcher:game:exited");

        fs::remove_dir_all(base).ok();
    }

    // ---- M3 同步 ----

    use bangumi_sync::{CloudCollection, CloudEpisodeProgress, OfflineClient, SubjectSearchHit};

    #[tokio::test]
    async fn set_collection_enqueues_and_push_drains() {
        let base = temp_root("sync-push");
        let client = Arc::new(OfflineClient::new(true));
        let services = services_with_client(&base, client.clone());
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");

        let dto = services.set_collection(&id, Some(3), Some(8)).expect("set");
        assert_eq!(dto.collection_status, Some(3));
        assert_eq!(dto.rating, Some(8));

        let queue = services.list_sync_queue().expect("queue");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].op, "collection");
        assert_eq!(queue[0].item_title.as_deref(), Some("Test Show"));

        let report = services.push_pending(10).await.expect("push");
        assert_eq!(report.succeeded, 1);
        assert_eq!(client.pushed().len(), 1);
        assert_eq!(services.list_sync_queue().expect("queue")[0].status, "done");

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn set_collection_without_binding_does_not_enqueue() {
        let base = temp_root("sync-unbound");
        let client = Arc::new(OfflineClient::new(true));
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);

        services.set_collection(&id, Some(2), None).expect("set");
        assert!(services.list_sync_queue().expect("queue").is_empty());

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn set_collection_rejects_out_of_range() {
        let base = temp_root("sync-range");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);
        assert_eq!(
            services
                .set_collection(&id, Some(9), None)
                .expect_err("状态越界")
                .code(),
            "validation_error"
        );
        assert_eq!(
            services
                .set_collection(&id, None, Some(11))
                .expect_err("评分越界")
                .code(),
            "validation_error"
        );
        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn manual_progress_enqueues_when_bound() {
        let base = temp_root("sync-progress");
        let client = Arc::new(OfflineClient::new(true));
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);

        // 未绑定：没有云端条目可写，不入队。
        services.set_progress(&id, 1, 600).expect("progress");
        assert!(services.list_sync_queue().expect("queue").is_empty());

        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        services.set_progress(&id, 2, 600).expect("progress");
        let queue = services.list_sync_queue().expect("queue");
        assert_eq!(queue.len(), 1, "手动校正应入队");
        assert_eq!(queue[0].op, "progress");
        assert_eq!(queue[0].item_id.as_deref(), Some(id.as_str()));

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn discard_sync_entry_removes_pending_item() {
        let base = temp_root("discard-entry");
        let client = Arc::new(OfflineClient::new(true));
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        services.set_progress(&id, 1, 300).expect("progress");

        let queue = services.list_sync_queue().expect("queue");
        assert_eq!(queue.len(), 1);
        services.discard_sync_entry(&queue[0].id).expect("discard");
        assert!(services.list_sync_queue().expect("queue").is_empty());

        // 不存在的记录要给出可读的校验错误。
        let err = services.discard_sync_entry("missing").expect_err("应拒绝");
        assert_eq!(err.code(), "validation_error");

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn session_finished_enqueues_progress() {
        let base = temp_root("sync-session");
        let client = Arc::new(OfflineClient::new(true));
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");

        // 0 秒表示没看成，不应产生推送。
        services
            .enqueue_session_progress(&id, 3, 0)
            .expect("zero session");
        assert!(services.list_sync_queue().expect("queue").is_empty());

        services
            .enqueue_session_progress(&id, 3, 720)
            .expect("session");
        let queue = services.list_sync_queue().expect("queue");
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].op, "progress");

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn pull_imports_watched_episodes_and_flags_conflicts() {
        let base = temp_root("sync-pull-progress");
        let client = Arc::new(
            OfflineClient::new(true)
                .with_cloud_collection(CloudCollection {
                    subject_id: 400602,
                    collection_status: None,
                    rating: None,
                    updated_at: None,
                })
                .with_cloud_progress(
                    400602,
                    vec![
                        // 云端「看过」而本地没有：直接导入（1 秒占位）。
                        CloudEpisodeProgress {
                            subject_id: 400602,
                            episode: 2,
                            watched_seconds: 1,
                            updated_at: None,
                        },
                        // 云端「未看」而本地看过：记冲突，不导入 0 秒行。
                        CloudEpisodeProgress {
                            subject_id: 400602,
                            episode: 1,
                            watched_seconds: 0,
                            updated_at: None,
                        },
                    ],
                ),
        );
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        services.set_progress(&id, 1, 300).expect("progress");

        let report = services.pull_all().await.expect("pull");
        assert_eq!(report.imported_progress, 1, "只导入云端看过的集");
        assert_eq!(report.conflicts, 1, "第 1 集两侧不一致应记冲突");

        let rows = services.list_progress(&id).expect("progress");
        assert!(rows.iter().any(|row| row.episode == 2));
        let conflict = services
            .list_conflicts()
            .expect("conflicts")
            .into_iter()
            .find(|row| row.field == "progress:1")
            .expect("progress conflict");
        assert_eq!(conflict.local_value.as_deref(), Some("看过"));
        assert_eq!(conflict.cloud_value.as_deref(), Some("未看"));

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn pull_all_imports_cloud_collection() {
        let base = temp_root("sync-pull");
        let client = Arc::new(
            OfflineClient::new(true).with_cloud_collection(CloudCollection {
                subject_id: 400602,
                collection_status: Some(3),
                rating: Some(8),
                updated_at: Some("2026-10-29T09:00:00Z".into()),
            }),
        );
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");

        let report = services.pull_all().await.expect("pull");
        assert_eq!(report.scanned_items, 1);
        assert_eq!(report.imported_collection, 1);
        assert_eq!(report.conflicts, 0);
        assert!(services.list_conflicts().expect("conflicts").is_empty());

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn pull_all_detects_and_resolves_conflict() {
        let base = temp_root("sync-conflict");
        let client = Arc::new(
            OfflineClient::new(true).with_cloud_collection(CloudCollection {
                subject_id: 400602,
                collection_status: Some(3),
                rating: None,
                updated_at: Some("2026-10-29T09:00:00Z".into()),
            }),
        );
        let services = services_with_client(&base, client.clone());
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        services.set_collection(&id, Some(2), None).expect("local");

        let report = services.pull_all().await.expect("pull");
        assert_eq!(report.conflicts, 1);
        let conflicts = services.list_conflicts().expect("conflicts");
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].field, "collection_status");

        let ids = vec![conflicts[0].id.clone()];
        let resolved = services.resolve_conflicts(&ids, "cloud").expect("resolve");
        assert_eq!(resolved.resolved, 1);
        assert!(services.list_conflicts().expect("conflicts").is_empty());

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn cache_metadata_writes_into_configured_dir() {
        let base = temp_root("sync-meta");
        let cache_dir = base.join("meta-cache");
        let client = Arc::new(OfflineClient::new(true).with_subject_metadata(
            400602,
            serde_json::json!({ "id": 400602, "name": "テスト" }),
        ));
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: Some(cache_dir.to_string_lossy().into()),
                player_path: None,
                ..Default::default()
            })
            .expect("settings");
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");

        let dto = services.cache_metadata(&id).await.expect("cache");
        assert_eq!(dto.subject_id, 400602);
        assert!(PathBuf::from(&dto.cached_path).is_file());
        assert!(dto.cached_path.contains("400602"));

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn sync_stats_reports_authorization_and_counts() {
        let base = temp_root("sync-stats");
        let client = Arc::new(OfflineClient::new(false));
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        services.set_collection(&id, Some(2), None).expect("set");

        let stats = services.sync_stats().expect("stats");
        assert!(!stats.authorized);
        assert_eq!(stats.pending, 1);

        // 未授权时推送应报错。
        assert_eq!(
            services.push_pending(10).await.expect_err("未授权").code(),
            "sync_not_authorized"
        );

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn auto_match_binds_caches_and_pulls() {
        let base = temp_root("auto-match");
        let meta_dir = base.join("meta");
        let client = Arc::new(
            OfflineClient::new(true)
                .with_search_hits(vec![SubjectSearchHit {
                    id: 400602,
                    name: "Test Show".into(),
                    name_cn: Some("测试番".into()),
                    score: None,
                }])
                .with_cloud_collection(CloudCollection {
                    subject_id: 400602,
                    collection_status: Some(3),
                    rating: Some(8),
                    updated_at: Some("2026-10-29T09:00:00Z".into()),
                })
                .with_subject_metadata(
                    400602,
                    serde_json::json!({"id": 400602, "name": "Test Show"}),
                ),
        );
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .save_settings(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: Some(meta_dir.to_string_lossy().into()),
                player_path: None,
                ..Default::default()
            })
            .expect("settings");

        let report = services.auto_match_and_pull().await.expect("auto match");
        assert_eq!(report.unbound_scanned, 1);
        assert_eq!(report.bound, 1);
        assert_eq!(report.metadata_cached, 1);
        assert_eq!(report.pulled, 1);

        let detail = services.get_item(&id).expect("detail");
        assert_eq!(detail.item.bangumi_subject_id, Some(400602));
        assert!(meta_dir.join("400602").join("subject.json").is_file());

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn auto_match_leaves_unmatched_items_unbound() {
        let base = temp_root("auto-none");
        let client = Arc::new(
            OfflineClient::new(true).with_search_hits(vec![SubjectSearchHit {
                id: 1,
                name: "完全不相干".into(),
                name_cn: Some("毫不相干".into()),
                score: None,
            }]),
        );
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);

        let report = services.auto_match_and_pull().await.expect("auto match");
        assert_eq!(report.unbound_scanned, 1);
        assert_eq!(report.bound, 0);

        let detail = services.get_item(&id).expect("detail");
        assert_eq!(detail.item.bangumi_subject_id, None);

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn search_bangumi_returns_hits() {
        let base = temp_root("search-hits");
        let client = Arc::new(
            OfflineClient::new(true).with_search_hits(vec![SubjectSearchHit {
                id: 42,
                name: "White Album 2".into(),
                name_cn: Some("白色相簿2".into()),
                score: Some(8.1),
            }]),
        );
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);

        let hits = services.search_bangumi(&id, None).await.expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 42);
        assert_eq!(hits[0].name_cn.as_deref(), Some("白色相簿2"));

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn refresh_item_pulls_and_caches() {
        let base = temp_root("refresh-item");
        let meta_dir = base.join("meta");
        let client = Arc::new(
            OfflineClient::new(true)
                .with_cloud_collection(CloudCollection {
                    subject_id: 400602,
                    collection_status: Some(2),
                    rating: Some(9),
                    updated_at: None,
                })
                .with_subject_metadata(400602, serde_json::json!({ "id": 400602 })),
        );
        let services = services_with_client(&base, client);
        let (_show, id) = anime_item(&services, &base);
        services
            .bind_bangumi_subject(&id, Some(400602))
            .expect("bind");
        services
            .save_settings(&SettingsInput {
                metadata_cache_dir: Some(meta_dir.to_string_lossy().into()),
                ..Default::default()
            })
            .expect("settings");

        services.refresh_item(&id).await.expect("refresh");

        let stored = services.get_item(&id).expect("detail");
        assert_eq!(stored.item.collection_status, Some(2));
        assert_eq!(stored.item.rating, Some(9));
        assert!(meta_dir.join("400602").join("subject.json").is_file());

        fs::remove_dir_all(base).ok();
    }

    #[tokio::test]
    async fn bangumi_config_and_status_flow() {
        let base = temp_root("bangumi-status");
        let client = Arc::new(OfflineClient::new(true));
        let services = services_with_client(&base, client);

        let status = services.bangumi_status().expect("status");
        assert!(!status.authorized);
        // 内置应用：未做任何配置也已有可用凭据；非内置时初始应为未配置。
        assert_eq!(status.has_credentials, status.using_builtin_app);

        let saved_input = BangumiConfigInput {
            client_id: "bgmabc".into(),
            client_secret: "s".into(),
            redirect_uri: "http://127.0.0.1:27432/callback".into(),
            proxy: String::new(),
        };

        if status.using_builtin_app {
            // 内置时凭据不回传前端，且拒绝覆盖写入。
            assert!(status.client_id.is_none());
            assert!(status.redirect_uri.is_none());
            assert_eq!(
                services
                    .save_bangumi_config(&saved_input)
                    .expect_err("内置应用不允许改凭据")
                    .code(),
                "validation_error"
            );

            let dto = services.get_settings().expect("settings");
            assert!(dto.bangumi_builtin);
            assert!(dto.bangumi_client_id.is_none());
            assert!(dto.bangumi_redirect_uri.is_none());
            assert!(dto.bangumi_has_secret);
        } else {
            let saved = services.save_bangumi_config(&saved_input).expect("save");
            assert!(saved.has_credentials);
            assert_eq!(saved.client_id.as_deref(), Some("bgmabc"));
            assert_eq!(
                saved.redirect_uri.as_deref(),
                Some("http://127.0.0.1:27432/callback")
            );

            // 设置 DTO 只暴露「是否已保存 secret」，不回传明文。
            let dto = services.get_settings().expect("settings");
            assert!(!dto.bangumi_builtin);
            assert!(dto.bangumi_has_secret);
            assert_eq!(dto.bangumi_client_id.as_deref(), Some("bgmabc"));
        }

        assert_eq!(
            services
                .get_settings()
                .expect("settings")
                .bangumi_default_redirect_uri,
            "http://127.0.0.1:27432/callback"
        );

        let logged_out = services.bangumi_logout().expect("logout");
        assert!(!logged_out.authorized);

        fs::remove_dir_all(base).ok();
    }

    fn write_ui_plugin_with(
        root: &Path,
        dir_name: &str,
        id: &str,
        permissions: &[&str],
    ) -> std::path::PathBuf {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).expect("plugin dir");
        let manifest = serde_json::json!({
            "id": id,
            "name": "示例面板",
            "version": "0.1.0",
            "kind": "ui",
            "apiVersion": "1",
            "entry": "main.js",
            "capabilities": [],
            "permissions": permissions,
            "mount": "library.item.detail.panel"
        });
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).expect("manifest");
        std::fs::write(dir.join("main.js"), b"export default { mount() {} };").expect("entry");
        dir
    }

    fn write_ui_plugin(root: &Path, dir_name: &str) -> (std::path::PathBuf, String) {
        let dir = write_ui_plugin_with(root, dir_name, "com.example.panel", &["read:library"]);
        (dir, "com.example.panel".to_owned())
    }

    #[test]
    fn plugin_discovered_registered_enabled_and_uninstalled() {
        let base = temp_root("plugins-flow");
        // 插件放在 <app data>/plugins 下，Services::new 时会自动登记（默认禁用）。
        let plugin_root = base.join("plugins");
        std::fs::create_dir_all(&plugin_root).expect("root");
        write_ui_plugin(&plugin_root, "panel");

        let services = services(&base);
        let overview = services.plugin_overview().expect("overview");
        assert_eq!(overview.plugins.len(), 1);
        let plugin = &overview.plugins[0];
        assert_eq!(plugin.id, "com.example.panel");
        assert!(plugin.installed);
        assert!(!plugin.enabled);
        assert_eq!(plugin.kind, "ui");
        assert_eq!(plugin.permissions, vec!["read:library".to_owned()]);

        let enabled = services
            .plugin_set_enabled("com.example.panel", true)
            .expect("enable");
        assert!(enabled.plugins[0].enabled);

        // 卸载只移除注册表记录，文件仍在 → 重新以「未安装」出现。
        let after = services
            .plugin_uninstall("com.example.panel")
            .expect("uninstall");
        assert_eq!(after.plugins.len(), 1);
        assert!(!after.plugins[0].installed);
        assert!(!after.plugins[0].enabled);

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_install_rejects_invalid_manifest() {
        let base = temp_root("plugins-invalid");
        let dir = base.join("broken");
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::json!({
                "id": "com.example.bad",
                "name": "坏插件",
                "version": "0.1.0",
                "kind": "ui",
                "apiVersion": "9",
                "entry": "main.js"
            })
            .to_string(),
        )
        .expect("manifest");
        std::fs::write(dir.join("main.js"), b"x").expect("entry");

        let services = services(&base);
        let err = services
            .plugin_install(dir.to_str().expect("path"))
            .expect_err("应拒绝");
        assert_eq!(err.code(), "plugin_error");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_root_follows_setting() {
        let base = temp_root("plugin-root-setting");
        let custom = base.join("custom-plugins");
        let services = services(&base);
        services
            .save_settings(&SettingsInput {
                plugins_dir: Some(custom.to_string_lossy().into()),
                ..Default::default()
            })
            .expect("save");

        let overview = services.plugin_overview().expect("overview");
        assert_eq!(overview.root, custom.to_string_lossy());
        assert!(custom.is_dir());

        services
            .save_settings(&SettingsInput {
                plugins_dir: Some(String::new()),
                ..Default::default()
            })
            .expect("clear");
        let restored = services.plugin_overview().expect("overview");
        assert!(restored.root.ends_with("plugins"));

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_ui_entry_requires_enabled_ui() {
        let base = temp_root("plugin-entry");
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).expect("root");
        write_ui_plugin(&root, "panel");

        let services = services(&base);
        let err = services
            .plugin_ui_entry("com.example.panel")
            .expect_err("未启用应拒绝");
        assert_eq!(err.code(), "validation_error");

        services
            .plugin_set_enabled("com.example.panel", true)
            .expect("enable");
        let entry = services
            .plugin_ui_entry("com.example.panel")
            .expect("entry");
        assert_eq!(entry.mount, "library.item.detail.panel");
        assert!(entry.source.contains("export default"));
        assert_eq!(entry.api_version, "1");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_permissions_are_enforced() {
        let base = temp_root("plugin-perm");
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).expect("root");
        write_ui_plugin_with(&root, "noperm", "com.example.noperm", &[]);

        let services = services(&base);
        services
            .plugin_set_enabled("com.example.noperm", true)
            .expect("enable");

        let err = services
            .plugin_read_items("com.example.noperm")
            .expect_err("缺 read:library 应拒绝");
        assert!(err.to_string().contains("read:library"));

        let err = services
            .plugin_emit("com.example.noperm", "hello", serde_json::Value::Null)
            .expect_err("缺 emit:events 应拒绝");
        assert!(err.to_string().contains("emit:events"));

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_emit_publishes_to_bus() {
        let base = temp_root("plugin-emit");
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).expect("root");
        write_ui_plugin_with(&root, "emitter", "com.example.emitter", &["emit:events"]);

        let services = services(&base);
        services
            .plugin_set_enabled("com.example.emitter", true)
            .expect("enable");
        let mut receiver = services.bus().subscribe();

        services
            .plugin_emit(
                "com.example.emitter",
                "hello",
                serde_json::json!({ "x": 1 }),
            )
            .expect("emit");

        let event = receiver.try_recv().expect("应收到事件");
        assert_eq!(event.topic(), "plugin:event:emitted");
        let envelope = event.to_envelope().expect("envelope");
        assert_eq!(envelope.payload["id"], "com.example.emitter");
        assert_eq!(envelope.payload["name"], "hello");
        assert_eq!(envelope.payload["payload"]["x"], 1);

        fs::remove_dir_all(base).ok();
    }

    fn write_capability_plugin(
        root: &Path,
        dir_name: &str,
        id: &str,
        permissions: &[&str],
        module: &str,
    ) -> std::path::PathBuf {
        let dir = root.join(dir_name);
        std::fs::create_dir_all(&dir).expect("plugin dir");
        let manifest = serde_json::json!({
            "id": id,
            "name": "示例能力插件",
            "version": "0.1.0",
            "kind": "capability",
            "apiVersion": "1",
            "entry": "main.wasm",
            "capabilities": ["scanner.anime"],
            "permissions": permissions
        });
        std::fs::write(dir.join("manifest.json"), manifest.to_string()).expect("manifest");
        // 沙箱同时接受 wasm 二进制与 wat 文本，测试里直接用文本省掉构建步骤。
        std::fs::write(dir.join("main.wasm"), module).expect("module");
        dir
    }

    #[test]
    fn plugin_invoke_runs_demo_scanner() {
        // 直接登记仓库里的示例插件，保证交付物本身可执行。
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/demo-scanner");
        let base = temp_root("plugin-invoke-demo");
        let services = Arc::new(services(&base));
        services
            .plugin_install(example.to_str().expect("path"))
            .expect("install");
        services
            .plugin_set_enabled("com.lagm.demo-scanner", true)
            .expect("enable");

        let result = services
            .plugin_invoke(
                "com.lagm.demo-scanner",
                serde_json::json!({ "file": "[Group] 夏日重现 - 12 [1080p].mkv" }),
            )
            .expect("invoke");

        assert_eq!(result.id, "com.lagm.demo-scanner");
        assert_eq!(result.output["episode"], 12);
        assert_eq!(result.output["season"], 1);
        assert_eq!(result.output["title"], "夏日重现");
        assert!(!result.logs.is_empty(), "插件日志应被回传");
        assert!(result.logs[0].contains("[info]"));

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_invoke_requires_enabled_capability() {
        let base = temp_root("plugin-invoke-kind");
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).expect("root");
        write_ui_plugin(&root, "panel");
        let services = Arc::new(services(&base));
        services
            .plugin_set_enabled("com.example.panel", true)
            .expect("enable");

        let err = services
            .plugin_invoke("com.example.panel", serde_json::Value::Null)
            .expect_err("UI 插件不能执行");
        assert!(err.to_string().contains("capability"), "实际：{err}");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_invoke_stops_runaway_module() {
        let base = temp_root("plugin-invoke-timeout");
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).expect("root");
        write_capability_plugin(
            &root,
            "spin",
            "com.example.spin",
            &[],
            r#"
(module
  (memory (export "memory") 1)
  (func (export "alloc") (param $len i32) (result i32) (i32.const 1024))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (loop $spin (br $spin))
    (i64.const 0)))
"#,
        );

        let services = Arc::new(services(&base));
        services
            .plugin_set_enabled("com.example.spin", true)
            .expect("enable");
        let err = services
            .plugin_invoke("com.example.spin", serde_json::Value::Null)
            .expect_err("死循环应被中止");
        assert_eq!(err.code(), "plugin_error");
        assert!(err.to_string().contains("指令预算"), "实际：{err}");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugin_invoke_exposes_items_only_with_permission() {
        let base = temp_root("plugin-invoke-items");
        let root = base.join("plugins");
        std::fs::create_dir_all(&root).expect("root");
        let module = r#"
(module
  (import "lagm" "read_items" (func $read_items (result i64)))
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $len)))
    (local.get $ptr))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (call $read_items)))
"#;
        write_capability_plugin(
            &root,
            "reader",
            "com.example.reader",
            &["read:library"],
            module,
        );
        write_capability_plugin(&root, "blind", "com.example.blind", &[], module);

        let services = Arc::new(services(&base));
        let (_show, _id) = anime_item(&services, &base);
        services
            .plugin_set_enabled("com.example.reader", true)
            .expect("enable");
        services
            .plugin_set_enabled("com.example.blind", true)
            .expect("enable");

        let allowed = services
            .plugin_invoke("com.example.reader", serde_json::Value::Null)
            .expect("有权限应放行");
        assert!(allowed
            .output
            .as_array()
            .is_some_and(|list| !list.is_empty()));

        let err = services
            .plugin_invoke("com.example.blind", serde_json::Value::Null)
            .expect_err("缺权限应拒绝");
        assert!(err.to_string().contains("read:library"), "实际：{err}");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn open_in_explorer_rejects_missing_target() {
        let base = temp_root("explorer-guard");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);

        // 不存在的文件：直接返回校验错误，不会真的拉起资源管理器。
        let err = services
            .open_in_explorer(&id, Some("nope.mkv"))
            .expect_err("文件不存在应拒绝");
        assert_eq!(err.code(), "validation_error");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn cache_stats_and_clear_work_on_cache_dir() {
        let base = temp_root("cache-stats");
        let services = services(&base);
        let cache_dir = base.join("meta");
        std::fs::create_dir_all(cache_dir.join("100")).expect("subject a");
        std::fs::create_dir_all(cache_dir.join("200")).expect("subject b");
        fs::write(cache_dir.join("100").join("subject.json"), b"{\"id\":100}").expect("file a");
        fs::write(cache_dir.join("200").join("cover.txt"), b"0123456789").expect("file b");

        services
            .save_settings(&SettingsInput {
                metadata_cache_dir: Some(cache_dir.to_string_lossy().into()),
                ..Default::default()
            })
            .expect("save");

        let stats = services.metadata_cache_stats().expect("stats");
        assert_eq!(stats.subject_dirs, 2);
        assert_eq!(stats.file_count, 2);
        assert_eq!(stats.total_bytes, 10 + 10);

        let cleared = services.clear_metadata_cache().expect("clear");
        assert_eq!(cleared.removed_dirs, 2);
        assert_eq!(cleared.freed_bytes, 10 + 10);
        assert!(services.metadata_cache_stats().expect("stats").subject_dirs == 0);

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn trash_items_reports_success_and_failure_per_item() {
        let base = temp_root("batch-trash");
        let services = services(&base);
        let (show, id) = anime_item(&services, &base);

        let report = services
            .trash_items(&[id.clone(), "missing-id".to_owned()])
            .expect("batch trash");
        assert_eq!(report.deleted, 1);
        assert_eq!(report.failed, 1, "不存在的条目应计入失败");
        assert!(!show.exists());
        assert!(services.get_item(&id).is_err());

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn open_external_rejects_non_http_urls() {
        let base = temp_root("open-external");
        let services = services(&base);
        // 只校验协议白名单，不真的拉起浏览器。
        for bad in [
            "file:///C:/Windows/System32/calc.exe",
            "javascript:alert(1)",
            "C:/x.exe",
        ] {
            let err = services.open_external(bad).expect_err("应拒绝");
            assert_eq!(err.code(), "validation_error", "{bad} 不应通过");
        }
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn play_next_episode_stops_at_last_episode() {
        let base = temp_root("next-episode");
        let services = services(&base);
        let (_show, id) = anime_item(&services, &base);

        // 只有 01 / 02 两集：从第 2 集往后没有了，应该返回 None 而不是报错。
        assert!(services
            .play_next_episode(&id, 2)
            .expect("查下一集")
            .is_none());

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn delete_item_trash_removes_files_and_records() {
        let base = temp_root("delete-trash");
        let services = services(&base);
        let (show, id) = anime_item(&services, &base);
        services.set_progress(&id, 1, 300).expect("progress");
        let mut receiver = services.bus().subscribe();

        let report = services
            .delete_item(&id, DeleteMode::Trash, None)
            .expect("删除");

        assert_eq!(report.item_id, id);
        assert_eq!(report.mode, "trash");
        assert!(report.files_removed);
        assert!(!show.exists(), "目录应已被移入回收站");
        assert!(services.get_item(&id).is_err(), "记录应被删除");
        assert!(
            services.list_progress(&id).expect("progress").is_empty(),
            "进度应随条目级联删除"
        );

        let mut topics = Vec::new();
        while let Ok(event) = receiver.try_recv() {
            topics.push(event.topic().to_owned());
        }
        assert!(
            topics.iter().any(|topic| topic == "library:item:deleted"),
            "实际事件：{topics:?}"
        );

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn delete_item_permanent_requires_title_confirmation() {
        let base = temp_root("delete-confirm");
        let services = services(&base);
        let (show, id) = anime_item(&services, &base);

        let err = services
            .delete_item(&id, DeleteMode::Permanent, None)
            .expect_err("缺少确认词应拒绝");
        assert_eq!(err.code(), "validation_error");

        let err = services
            .delete_item(&id, DeleteMode::Permanent, Some("别的标题"))
            .expect_err("确认词不符应拒绝");
        assert_eq!(err.code(), "validation_error");
        assert!(show.exists(), "被拒绝时不能动文件");
        assert!(services.get_item(&id).is_ok(), "被拒绝时不能删记录");

        let report = services
            .delete_item(&id, DeleteMode::Permanent, Some("Test Show"))
            .expect("永久删除");
        assert_eq!(report.mode, "permanent");
        assert!(!show.exists(), "永久删除后目录不应存在");
        assert!(services.get_item(&id).is_err());

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn delete_item_rejects_folder_outside_root() {
        let base = temp_root("delete-outside");
        let services = services(&base);
        let (show, id) = anime_item(&services, &base);

        // 把动画根目录改到别处：条目目录落在根外，必须拒绝，避免误删任意路径。
        let other = base.join("other-root");
        fs::create_dir_all(&other).expect("other");
        services
            .save_settings(&SettingsInput {
                anime_root: Some(other.to_string_lossy().into()),
                ..Default::default()
            })
            .expect("save");

        let err = services
            .delete_item(&id, DeleteMode::Permanent, Some("Test Show"))
            .expect_err("根目录外应拒绝");
        assert_eq!(err.code(), "validation_error");
        assert!(show.exists());
        assert!(services.get_item(&id).is_ok());

        fs::remove_dir_all(base).ok();
    }
}
