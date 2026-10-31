use library_core::model::{ItemKind, MediaFile, ScanReport};
use serde::Serialize;

/// 设置页数据。`defaultMetadataCacheDir` 让前端能展示「恢复默认」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsDto {
    pub anime_root: Option<String>,
    pub game_root: Option<String>,
    pub metadata_cache_dir: String,
    pub default_metadata_cache_dir: String,
    pub metadata_cache_dir_is_default: bool,
    pub player_path: Option<String>,
    /// 生效中的播放器参数模板（已应用默认值）。
    pub player_args: String,
    /// Bangumi App ID（`client_id`），未配置为 None。
    pub bangumi_client_id: Option<String>,
    /// OAuth 回调地址，未配置为 None。
    pub bangumi_redirect_uri: Option<String>,
    /// 显式代理地址，未配置为 None（此时自动探测系统代理）。
    pub bangumi_proxy: Option<String>,
    /// 是否已保存 App Secret（只回传布尔值，绝不回传明文）。
    pub bangumi_has_secret: bool,
    /// 当前构建是否内置了 Bangumi 应用；为 true 时凭据不可见也不可修改。
    pub bangumi_builtin: bool,
    /// 默认回调地址，供前端一键填入。
    pub bangumi_default_redirect_uri: String,
    /// 自定义插件目录，未配置为 None。
    pub plugins_dir: Option<String>,
    /// 默认插件目录（应用数据目录下的 plugins）。
    pub default_plugins_dir: String,
}

/// 列表页的媒体摘要（不含完整文件列表，避免列表响应过大）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaSummaryDto {
    pub file_count: usize,
    pub episode_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemSummaryDto {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    pub bangumi_subject_id: Option<i64>,
    pub folder_path: String,
    pub created_at: String,
    pub updated_at: String,
    pub collection_status: Option<i64>,
    pub rating: Option<i64>,
    pub collection_updated_at: Option<String>,
    pub media: MediaSummaryDto,
}

/// 详情页数据：条目 + 实时探测到的文件列表。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemDetailDto {
    pub item: ItemSummaryDto,
    pub files: Vec<MediaFile>,
}

/// 一次扫描的结果。单个根目录失败不影响另一个，失败原因放在 `messages`。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanSummaryDto {
    pub anime: Option<ScanReport>,
    pub game: Option<ScanReport>,
    pub total_created: usize,
    pub total_updated: usize,
    pub total_unchanged: usize,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirEntryDto {
    pub name: String,
    pub path: String,
}

/// 目录浏览结果，供设置页选择目录（前端不直接访问文件系统）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirListingDto {
    pub path: Option<String>,
    pub parent: Option<String>,
    pub entries: Vec<DirEntryDto>,
}

/// 可执行文件条目，供 exe 选择器使用。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileEntryDto {
    pub name: String,
    pub path: String,
    pub size_bytes: u64,
}

/// .gamelauncher.json 的前端视图，附带解析后的绝对路径与存在性，避免 UI 自己拼路径。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameBindingDto {
    pub exe_path: String,
    pub resolved_exe_path: String,
    pub exe_exists: bool,
    pub args: Vec<String>,
    pub working_dir: String,
    pub last_launched_at: Option<String>,
    pub last_pid: Option<u32>,
}

/// 游戏条目的运行态 + 绑定信息，详情页一次取回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GameStatusDto {
    pub running_pid: Option<u32>,
    pub binding: Option<GameBindingDto>,
}

/// 单集进度。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressDto {
    pub episode: u32,
    pub watched_seconds: u64,
    pub source: String,
    pub updated_at: String,
}

/// 收藏状态 / 评分（M3）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDto {
    pub item_id: String,
    pub collection_status: Option<i64>,
    pub rating: Option<i64>,
    pub collection_updated_at: Option<String>,
}

/// 同步队列一项的前端视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncQueueDto {
    pub id: String,
    pub item_id: Option<String>,
    pub item_title: Option<String>,
    pub op: String,
    pub status: String,
    pub attempts: i64,
    pub next_retry_at: Option<String>,
    pub created_at: String,
}

/// 同步概览。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatsDto {
    /// 是否已持有可用授权（OAuth 待接入，骨架阶段通常为 false）。
    pub authorized: bool,
    pub pending: i64,
    pub failed: i64,
    pub done: i64,
    pub unresolved_conflicts: usize,
}

/// 冲突一项的前端视图。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictDto {
    pub id: String,
    pub item_id: Option<String>,
    pub item_title: Option<String>,
    pub field: String,
    pub local_value: Option<String>,
    pub cloud_value: Option<String>,
    pub local_ts: Option<String>,
    pub cloud_ts: Option<String>,
}

/// 推送结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PushReportDto {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

/// 拉取结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullReportDto {
    pub scanned_items: usize,
    pub conflicts: usize,
    pub imported_collection: usize,
    pub imported_progress: usize,
    pub messages: Vec<String>,
}

/// 冲突裁决结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveReportDto {
    pub resolved: usize,
}

/// 元数据缓存结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataCacheDto {
    pub subject_id: i64,
    pub cached_path: String,
}

/// Bangumi 授权状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiStatusDto {
    /// 是否已持有 token。
    pub authorized: bool,
    /// token 过期时间（RFC3339），未知为 None。
    pub expires_at: Option<String>,
    /// 是否已保存 client_id / secret / redirect_uri。
    pub has_credentials: bool,
    pub client_id: Option<String>,
    pub redirect_uri: Option<String>,
    /// 凭据来自软件内置应用（用户没有填写自己的）。
    pub using_builtin_app: bool,
}

/// 自动匹配与首次拉取的结果（scan 之后运行）。
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoMatchReportDto {
    /// 本次检查的未绑定条目数。
    pub unbound_scanned: usize,
    /// 自动绑定成功的条目数。
    pub bound: usize,
    /// 成功缓存元数据的条目数。
    pub metadata_cached: usize,
    /// 拉取到的已绑定条目数。
    pub pulled: usize,
    /// 产生的冲突数。
    pub conflicts: usize,
    /// 未能自动绑定的条目及原因（供用户手动处理）。
    pub unmatched: Vec<String>,
    pub messages: Vec<String>,
}

/// Bangumi 搜索结果的一项（手动绑定用）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectSearchHitDto {
    pub id: i64,
    pub name: String,
    pub name_cn: Option<String>,
    /// Bangumi 站内评分，仅作参考。
    pub score: Option<f64>,
}

/// 插件列表项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginDto {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `ui` 或 `capability`。
    pub kind: String,
    pub api_version: String,
    pub enabled: bool,
    /// 是否已登记进注册表。
    pub installed: bool,
    pub manifest_path: String,
    /// 插件目录（安装/重新登记时使用）。
    pub dir: String,
    pub permissions: Vec<String>,
    pub capabilities: Vec<String>,
    /// UI 插件挂载点。
    pub mount: Option<String>,
    /// 清单或入口文件异常时的说明。
    pub error: Option<String>,
}

/// UI 插件入口源码（由设置页/详情页动态加载）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginEntryDto {
    pub id: String,
    pub api_version: String,
    pub mount: String,
    /// ESM 源码；前端用 Blob URL 动态 import。
    pub source: String,
}

/// 插件总览（含根目录，便于界面提示）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginOverviewDto {
    pub root: String,
    pub plugins: Vec<PluginDto>,
}

/// 元数据缓存占用统计。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheStatsDto {
    pub dir: String,
    /// 缓存目录下的条目子目录数量（每个 subject 一个）。
    pub subject_dirs: usize,
    pub file_count: usize,
    pub total_bytes: u64,
}

/// 清理缓存的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheClearDto {
    pub removed_dirs: usize,
    pub freed_bytes: u64,
}

/// 批量刷新元数据的结果。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatchRefreshDto {
    pub refreshed: usize,
    /// 未绑定 Bangumi、跳过刷新的条目数。
    pub skipped: usize,
    pub failed: usize,
    pub messages: Vec<String>,
}

/// 批量移入回收站的结果。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BatchDeleteDto {
    pub deleted: usize,
    pub failed: usize,
    pub messages: Vec<String>,
}

/// 拖入导入：成功搬到根目录的一项。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportedItemDto {
    pub source: String,
    pub target: String,
    pub name: String,
    pub kind: ItemKind,
}

/// 拖入导入的结果。
#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ImportReportDto {
    pub imported: Vec<ImportedItemDto>,
    /// 逐条失败原因（路径不可识别、目标已存在等）。
    pub failed: Vec<String>,
    /// 因缺少根目录配置而跳过的项。
    pub skipped: Vec<String>,
}

/// 搜索结果右侧预览用的条目概览。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectPreviewDto {
    pub id: i64,
    pub name: String,
    pub name_cn: String,
    pub summary: String,
    /// 1 书籍 / 2 动画 / 3 音乐 / 4 游戏 / 6 三次元。
    pub subject_type: i64,
    pub date: Option<String>,
    pub eps: Option<i64>,
    pub score: Option<f64>,
    pub rank: Option<i64>,
    pub rating_total: i64,
    pub tags: Vec<PreviewTagDto>,
    pub related: Vec<PreviewRelatedDto>,
    /// 封面 data URL；下载失败为 None（不影响其它字段）。
    pub cover: Option<String>,
    /// 站内页面，供「在浏览器打开」。
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewTagDto {
    pub name: String,
    pub count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRelatedDto {
    pub id: i64,
    pub name: String,
    pub name_cn: String,
    pub relation: String,
    pub subject_type: i64,
}

/// 当前登录的 Bangumi 账号。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountDto {
    pub authorized: bool,
    pub id: Option<i64>,
    pub username: Option<String>,
    pub nickname: Option<String>,
    /// 头像直链（Bangumi CDN）。
    pub avatar_url: Option<String>,
    pub expires_at: Option<String>,
}

/// 删除资源的执行结果（plan.md 6.5）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteItemDto {
    pub item_id: String,
    pub title: String,
    /// `trash`（回收站）或 `permanent`（不可恢复）。
    pub mode: String,
    pub folder_path: String,
    /// 磁盘上是否真的删掉了目录；目录已不存在时为 false（只清理了记录）。
    pub files_removed: bool,
}

/// 能力插件一次执行的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginInvocationDto {
    pub id: String,
    /// 插件返回的 JSON（已在沙箱侧校验可解析）。
    pub output: serde_json::Value,
    /// 插件通过宿主日志 API 输出的行（受总量上限约束）。
    pub logs: Vec<String>,
    pub duration_ms: u64,
    /// 消耗的指令预算，便于判断插件是否接近上限。
    pub fuel_used: u64,
}
