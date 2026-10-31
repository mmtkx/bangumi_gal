use std::sync::Arc;

use launcher::RunningGame;
use library_core::model::ItemKind;
use player_adapter::{ActiveSession, PlayingSession};
use tauri::State;

use crate::dirs;
use crate::dto::{
    AccountDto, AutoMatchReportDto, BangumiStatusDto, BatchDeleteDto, BatchRefreshDto,
    CacheClearDto, CacheStatsDto, CollectionDto, ConflictDto, DeleteItemDto, DirListingDto,
    FileEntryDto, GameStatusDto, ImportReportDto, ItemDetailDto, ItemSummaryDto, MetadataCacheDto,
    PluginEntryDto, PluginInvocationDto, PluginOverviewDto, ProgressDto, PullReportDto,
    PushReportDto, ResolveReportDto, ScanSummaryDto, SettingsDto, SubjectPreviewDto,
    SubjectSearchHitDto, SyncQueueDto, SyncStatsDto,
};
use crate::error::{ApiResult, AppError};
use crate::logs::LogInfoDto;
use crate::services::Services;
use crate::settings::{BangumiConfigInput, SettingsInput};

/// 所有 command 都是「异步外壳 + 阻塞内核」：rusqlite 与文件系统遍历都是阻塞操作，
/// 放到 spawn_blocking 线程池，避免占用异步运行时线程（ADR-012）。
async fn run_blocking<T, F>(work: F) -> ApiResult<T>
where
    T: Send + 'static,
    F: FnOnce() -> ApiResult<T> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|err| AppError::Join(err.to_string()))?
}

#[tauri::command]
pub async fn get_settings(services: State<'_, Arc<Services>>) -> ApiResult<SettingsDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.get_settings()).await
}

#[tauri::command]
pub async fn save_settings(
    services: State<'_, Arc<Services>>,
    input: SettingsInput,
) -> ApiResult<SettingsDto> {
    let services = Arc::clone(services.inner());
    let worker = Arc::clone(&services);
    let (dto, roots_changed) = run_blocking(move || {
        let before = worker.get_settings()?;
        let after = worker.save_settings(&input)?;
        // 只有「根目录」变化才值得重扫：改播放器路径、插件目录都不需要。
        let changed = before.anime_root != after.anime_root || before.game_root != after.game_root;
        Ok((after, changed))
    })
    .await?;

    if roots_changed && (dto.anime_root.is_some() || dto.game_root.is_some()) {
        services.spawn_auto_import();
    }
    Ok(dto)
}

#[tauri::command]
pub async fn scan_library(services: State<'_, Arc<Services>>) -> ApiResult<ScanSummaryDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.scan()).await
}

#[tauri::command]
pub async fn list_items(
    services: State<'_, Arc<Services>>,
    kind: Option<ItemKind>,
) -> ApiResult<Vec<ItemSummaryDto>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.list_items(kind)).await
}

#[tauri::command]
pub async fn get_item(services: State<'_, Arc<Services>>, id: String) -> ApiResult<ItemDetailDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.get_item(&id)).await
}

/// 手动绑定 Bangumi 条目 ID（M1 只落库 + 触发占位事件，不调用 API）。
#[tauri::command]
pub async fn bind_bangumi_subject(
    services: State<'_, Arc<Services>>,
    item_id: String,
    subject_id: Option<i64>,
) -> ApiResult<ItemSummaryDto> {
    let services = Arc::clone(services.inner());
    let refresh_id = item_id.clone();
    let bind_services = Arc::clone(&services);
    let item =
        run_blocking(move || bind_services.bind_bangumi_subject(&item_id, subject_id)).await?;
    // 绑定成功后自动拉取云端收藏并缓存元数据（后台执行，不阻塞界面）。
    if item.bangumi_subject_id.is_some() {
        let refresh_services = Arc::clone(&services);
        tauri::async_runtime::spawn(async move {
            if let Err(err) = refresh_services.refresh_item(&refresh_id).await {
                tracing::warn!(item = %refresh_id, error = %err, "绑定后拉取失败");
            }
        });
    }
    Ok(item)
}

#[tauri::command]
pub async fn list_subdirs(path: Option<String>) -> ApiResult<DirListingDto> {
    run_blocking(move || dirs::list_subdirs(path.as_deref())).await
}

#[tauri::command]
pub async fn list_exe_files(path: String) -> ApiResult<Vec<FileEntryDto>> {
    run_blocking(move || dirs::list_exe_files(&path)).await
}

#[tauri::command]
pub async fn game_status(
    services: State<'_, Arc<Services>>,
    item_id: String,
) -> ApiResult<GameStatusDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.game_status(&item_id)).await
}

#[tauri::command]
pub async fn set_game_binding(
    services: State<'_, Arc<Services>>,
    item_id: String,
    exe_path: String,
) -> ApiResult<GameStatusDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.set_game_binding(&item_id, &exe_path)).await
}

/// 启动游戏。未绑定可执行文件时返回 code = binding_missing，由前端弹选择器。
#[tauri::command]
pub async fn start_game(
    services: State<'_, Arc<Services>>,
    item_id: String,
) -> ApiResult<RunningGame> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.start_game(&item_id)).await
}

#[tauri::command]
pub async fn running_games(services: State<'_, Arc<Services>>) -> ApiResult<Vec<RunningGame>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.running_games()).await
}

#[tauri::command]
pub async fn active_sessions(services: State<'_, Arc<Services>>) -> ApiResult<Vec<ActiveSession>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.active_sessions()).await
}

/// 播放条目内的单个文件（逐集播放）。
#[tauri::command]
pub async fn play_episode(
    services: State<'_, Arc<Services>>,
    item_id: String,
    file_name: String,
) -> ApiResult<PlayingSession> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.play_episode(&item_id, &file_name)).await
}

/// 播放「指定集之后」的第一集；已是最后一集时返回 null。
#[tauri::command]
pub async fn play_next_episode(
    services: State<'_, Arc<Services>>,
    item_id: String,
    episode: u32,
) -> ApiResult<Option<PlayingSession>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.play_next_episode(&item_id, episode)).await
}

/// 批量取已缓存的封面（data URL，key = itemId），并触发后台补齐缺失项。
#[tauri::command]
pub async fn item_covers(
    services: State<'_, Arc<Services>>,
    ids: Vec<String>,
) -> ApiResult<std::collections::HashMap<String, String>> {
    let services = Arc::clone(services.inner());
    let cached = {
        let services = Arc::clone(&services);
        let ids = ids.clone();
        run_blocking(move || services.item_covers_cached(&ids)).await?
    };
    // 缺封面的条目在后台慢慢补，界面先用缓存顶着。
    let missing: Vec<String> = ids
        .into_iter()
        .filter(|id| !cached.contains_key(id))
        .collect();
    if !missing.is_empty() {
        services.spawn_cover_prefetch(missing);
    }
    Ok(cached)
}

/// 条目封面（data URL）；未绑定或无封面时返回 null。
#[tauri::command]
pub async fn item_cover(
    services: State<'_, Arc<Services>>,
    item_id: String,
) -> ApiResult<Option<String>> {
    let services = Arc::clone(services.inner());
    services.item_cover(&item_id).await
}

#[tauri::command]
pub async fn list_progress(
    services: State<'_, Arc<Services>>,
    item_id: String,
) -> ApiResult<Vec<ProgressDto>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.list_progress(&item_id)).await
}

/// 手动校正进度。
#[tauri::command]
pub async fn set_progress(
    services: State<'_, Arc<Services>>,
    item_id: String,
    episode: u32,
    watched_seconds: u64,
) -> ApiResult<ProgressDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.set_progress(&item_id, episode, watched_seconds)).await
}

// ---- M3 同步 ----

/// 设置收藏状态 / 评分（落库并暂存到推送队列）。
#[tauri::command]
pub async fn set_collection(
    services: State<'_, Arc<Services>>,
    item_id: String,
    collection_status: Option<i64>,
    rating: Option<i64>,
) -> ApiResult<CollectionDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.set_collection(&item_id, collection_status, rating)).await
}

#[tauri::command]
pub async fn list_sync_queue(services: State<'_, Arc<Services>>) -> ApiResult<Vec<SyncQueueDto>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.list_sync_queue()).await
}

#[tauri::command]
pub async fn sync_stats(services: State<'_, Arc<Services>>) -> ApiResult<SyncStatsDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.sync_stats()).await
}

/// 手动触发推送。内部按限速逐条发送，故直接 await（不再套 spawn_blocking）。
#[tauri::command]
pub async fn push_pending(
    services: State<'_, Arc<Services>>,
    limit: Option<usize>,
) -> ApiResult<PushReportDto> {
    let services = Arc::clone(services.inner());
    services.push_pending(limit.unwrap_or(20)).await
}

/// 拉取所有已绑定条目的云端数据并与本地比较。
#[tauri::command]
pub async fn sync_pull(services: State<'_, Arc<Services>>) -> ApiResult<PullReportDto> {
    let services = Arc::clone(services.inner());
    services.pull_all().await
}

#[tauri::command]
pub async fn list_conflicts(services: State<'_, Arc<Services>>) -> ApiResult<Vec<ConflictDto>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.list_conflicts()).await
}

/// 批量裁决冲突；`choice` 为 `local` 或 `cloud`。
#[tauri::command]
pub async fn resolve_conflicts(
    services: State<'_, Arc<Services>>,
    conflict_ids: Vec<String>,
    choice: String,
) -> ApiResult<ResolveReportDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.resolve_conflicts(&conflict_ids, &choice)).await
}

/// 抓取并缓存条目元数据到可配置目录。
#[tauri::command]
pub async fn cache_metadata(
    services: State<'_, Arc<Services>>,
    item_id: String,
) -> ApiResult<MetadataCacheDto> {
    let services = Arc::clone(services.inner());
    services.cache_metadata(&item_id).await
}

// ---- Bangumi OAuth ----

#[tauri::command]
pub async fn bangumi_status(services: State<'_, Arc<Services>>) -> ApiResult<BangumiStatusDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.bangumi_status()).await
}

/// 保存 App ID / App Secret / 回调地址（三者全空则清除）。
#[tauri::command]
pub async fn save_bangumi_config(
    services: State<'_, Arc<Services>>,
    input: BangumiConfigInput,
) -> ApiResult<BangumiStatusDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.save_bangumi_config(&input)).await
}

/// 启动完整 OAuth 授权：打开浏览器并等待本地回调（最长 180 秒）。
#[tauri::command]
pub async fn bangumi_authorize(services: State<'_, Arc<Services>>) -> ApiResult<BangumiStatusDto> {
    let services = Arc::clone(services.inner());
    services.bangumi_authorize().await
}

#[tauri::command]
pub async fn bangumi_logout(services: State<'_, Arc<Services>>) -> ApiResult<BangumiStatusDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.bangumi_logout()).await
}

/// 兜底：粘贴浏览器回调地址（含 code）或授权码完成授权。
#[tauri::command]
pub async fn bangumi_complete_manual(
    services: State<'_, Arc<Services>>,
    callback: String,
) -> ApiResult<BangumiStatusDto> {
    let services = Arc::clone(services.inner());
    services.bangumi_complete_manual(&callback).await
}

/// 自动匹配未绑定条目：搜索 Bangumi → 绑定 → 缓存元数据 → 拉取云端。
#[tauri::command]
pub async fn auto_match(services: State<'_, Arc<Services>>) -> ApiResult<AutoMatchReportDto> {
    let services = Arc::clone(services.inner());
    services.auto_match_and_pull().await
}

/// 手动搜索 Bangumi 条目（详情页绑定用）；`keyword` 为空时用条目标题。
#[tauri::command]
pub async fn search_bangumi(
    services: State<'_, Arc<Services>>,
    item_id: String,
    keyword: Option<String>,
) -> ApiResult<Vec<SubjectSearchHitDto>> {
    let services = Arc::clone(services.inner());
    services.search_bangumi(&item_id, keyword).await
}

/// 立即拉取单个条目的云端收藏与进度。
#[tauri::command]
pub async fn pull_item_now(
    services: State<'_, Arc<Services>>,
    item_id: String,
) -> ApiResult<ItemSummaryDto> {
    let services = Arc::clone(services.inner());
    services.pull_item_now(&item_id).await
}

// ---- 同步队列（M5 迭代） ----

/// 丢弃一条待推送记录（只删本地队列）。
#[tauri::command]
pub async fn discard_sync_entry(services: State<'_, Arc<Services>>, id: String) -> ApiResult<bool> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.discard_sync_entry(&id)).await
}

// ---- 元数据缓存与日志目录（M5 迭代） ----

#[tauri::command]
pub async fn metadata_cache_stats(services: State<'_, Arc<Services>>) -> ApiResult<CacheStatsDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.metadata_cache_stats()).await
}

#[tauri::command]
pub async fn clear_metadata_cache(services: State<'_, Arc<Services>>) -> ApiResult<CacheClearDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.clear_metadata_cache()).await
}

#[tauri::command]
pub async fn open_log_dir(services: State<'_, Arc<Services>>) -> ApiResult<()> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.open_log_dir()).await
}

// ---- 拖入导入（M5 迭代） ----

/// 导入拖进窗口的目录 / zip：识别类型 → 剪切到对应根目录 → 重新扫描。
#[tauri::command]
pub async fn import_drop(
    services: State<'_, Arc<Services>>,
    paths: Vec<String>,
) -> ApiResult<ImportReportDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.import_drop(&paths)).await
}

// ---- 搜索结果预览（M5 迭代） ----

/// 搜索结果右侧预览：条目概览、标签、关联条目与封面。
#[tauri::command]
pub async fn bangumi_subject_preview(
    services: State<'_, Arc<Services>>,
    subject_id: i64,
) -> ApiResult<SubjectPreviewDto> {
    let services = Arc::clone(services.inner());
    services.bangumi_subject_preview(subject_id).await
}

/// 用系统默认浏览器打开链接（只接受 http/https）。
#[tauri::command]
pub async fn open_external(services: State<'_, Arc<Services>>, url: String) -> ApiResult<()> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.open_external(&url)).await
}

// ---- 账号（M5 迭代） ----

/// 当前登录账号的资料（昵称、头像、ID）。
#[tauri::command]
pub async fn bangumi_account(services: State<'_, Arc<Services>>) -> ApiResult<AccountDto> {
    let services = Arc::clone(services.inner());
    services.bangumi_account().await
}

// ---- 批量操作（M5 迭代） ----

/// 批量刷新已绑定条目的元数据与收藏状态。
#[tauri::command]
pub async fn refresh_metadata_batch(
    services: State<'_, Arc<Services>>,
    ids: Vec<String>,
) -> ApiResult<BatchRefreshDto> {
    let services = Arc::clone(services.inner());
    services.refresh_metadata_batch(&ids).await
}

/// 批量移入回收站（永久删除必须逐条确认）。
#[tauri::command]
pub async fn trash_items(
    services: State<'_, Arc<Services>>,
    ids: Vec<String>,
) -> ApiResult<BatchDeleteDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.trash_items(&ids)).await
}

// ---- 资源定位（M5 迭代） ----

/// 在资源管理器里打开条目目录；传 file_name 时改为选中该文件。
#[tauri::command]
pub async fn open_in_explorer(
    services: State<'_, Arc<Services>>,
    item_id: String,
    file_name: Option<String>,
) -> ApiResult<()> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.open_in_explorer(&item_id, file_name.as_deref())).await
}

// ---- 日志（M5） ----

/// 前端就绪打点：脚本在 WebView 里跑通（含 CSP 生效后）才会调用一次。
#[tauri::command]
pub async fn ui_ready() -> ApiResult<()> {
    tracing::info!("界面已就绪");
    Ok(())
}

/// 日志目录与文件清单，供设置页展示与排查。
#[tauri::command]
pub async fn log_location() -> ApiResult<LogInfoDto> {
    let dir = crate::logs::default_log_dir();
    run_blocking(move || Ok(crate::logs::log_info(&dir))).await
}

// ---- 删除资源（M5） ----

/// 删除条目资源：`mode` 为 `trash`（默认回收站）或 `permanent`（需 confirm 等于条目标题）。
#[tauri::command]
pub async fn delete_item(
    services: State<'_, Arc<Services>>,
    id: String,
    mode: Option<String>,
    confirm: Option<String>,
) -> ApiResult<DeleteItemDto> {
    let mode = crate::delete::DeleteMode::parse(mode.as_deref().unwrap_or("trash"))
        .ok_or_else(|| AppError::Validation("删除方式只能是 trash 或 permanent".to_owned()))?;
    let services = Arc::clone(services.inner());
    run_blocking(move || services.delete_item(&id, mode, confirm.as_deref())).await
}

/// 清空条目记录（可按类型）。只删数据库，磁盘文件一概不碰。
#[tauri::command]
pub async fn clear_items(
    services: State<'_, Arc<Services>>,
    kind: Option<String>,
) -> ApiResult<usize> {
    let parsed = match kind.as_deref() {
        Some(value) => Some(ItemKind::parse(value)?),
        None => None,
    };
    let services = Arc::clone(services.inner());
    run_blocking(move || services.clear_items(parsed)).await
}

// ---- 插件（M4 阶段 1） ----
#[tauri::command]
pub async fn plugin_list(services: State<'_, Arc<Services>>) -> ApiResult<PluginOverviewDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_overview()).await
}

/// 登记一个插件目录（不复制文件）。
#[tauri::command]
pub async fn plugin_install(
    services: State<'_, Arc<Services>>,
    path: String,
) -> ApiResult<PluginOverviewDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_install(&path)).await
}

#[tauri::command]
pub async fn plugin_set_enabled(
    services: State<'_, Arc<Services>>,
    id: String,
    enabled: bool,
) -> ApiResult<PluginOverviewDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_set_enabled(&id, enabled)).await
}

#[tauri::command]
pub async fn plugin_uninstall(
    services: State<'_, Arc<Services>>,
    id: String,
) -> ApiResult<PluginOverviewDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_uninstall(&id)).await
}

// ---- UI 插件宿主 API（权限由后端校验） ----

/// 取 UI 插件入口源码；前端用 Blob URL 动态 import。
#[tauri::command]
pub async fn plugin_ui_entry(
    services: State<'_, Arc<Services>>,
    id: String,
) -> ApiResult<PluginEntryDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_ui_entry(&id)).await
}

#[tauri::command]
pub async fn plugin_read_items(
    services: State<'_, Arc<Services>>,
    id: String,
) -> ApiResult<Vec<ItemSummaryDto>> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_read_items(&id)).await
}

#[tauri::command]
pub async fn plugin_emit(
    services: State<'_, Arc<Services>>,
    id: String,
    name: String,
    payload: Option<serde_json::Value>,
) -> ApiResult<()> {
    let services = Arc::clone(services.inner());
    run_blocking(move || {
        services.plugin_emit(&id, &name, payload.unwrap_or(serde_json::Value::Null))
    })
    .await
}

#[tauri::command]
pub async fn plugin_ui_unloaded(services: State<'_, Arc<Services>>, id: String) -> ApiResult<()> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_ui_unloaded(&id)).await
}

// ---- 能力插件（WASM 沙箱） ----

/// 在沙箱里执行一次能力插件，`input` 原样作为 JSON 传给插件。
#[tauri::command]
pub async fn plugin_invoke(
    services: State<'_, Arc<Services>>,
    id: String,
    input: Option<serde_json::Value>,
) -> ApiResult<PluginInvocationDto> {
    let services = Arc::clone(services.inner());
    run_blocking(move || services.plugin_invoke(&id, input.unwrap_or(serde_json::Value::Null)))
        .await
}
