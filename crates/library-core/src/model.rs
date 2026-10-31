use serde::{Deserialize, Serialize};

use crate::error::{CoreError, Result};

/// 条目类型，对应 `library_item.kind`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ItemKind {
    Anime,
    Game,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Anime => "anime",
            Self::Game => "game",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "anime" => Ok(Self::Anime),
            "game" => Ok(Self::Game),
            other => Err(CoreError::InvalidKind(other.to_owned())),
        }
    }

    /// 中文展示名，用于事件与日志，不参与持久化。
    pub fn label(self) -> &'static str {
        match self {
            Self::Anime => "动画",
            Self::Game => "游戏",
        }
    }
}

/// `library_item` 一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub id: String,
    pub kind: ItemKind,
    pub title: String,
    pub bangumi_subject_id: Option<i64>,
    pub folder_path: String,
    pub created_at: String,
    pub updated_at: String,
    /// Bangumi 收藏状态：1 想看 / 2 看过 / 3 在看 / 4 搁置 / 5 抛弃（0002 迁移新增）。
    pub collection_status: Option<i64>,
    /// Bangumi 评分 0-10，NULL 表示未评分（0002 迁移新增）。
    pub rating: Option<i64>,
    /// 收藏信息最后一次变更时间，冲突比较用（0002 迁移新增）。
    pub collection_updated_at: Option<String>,
    /// 目录内分组键（0003 迁移新增）。
    ///
    /// 根目录直接放视频时，一个目录会有多个条目，用该字段区分各自拥有的文件；
    /// `None` 表示整个目录都属于这个条目（原有的目录型条目）。
    pub file_filter: Option<String>,
}

/// 扫描器交给仓储写入的最小字段集合。
///
/// 不含 `id` 与 `bangumi_subject_id`：前者由仓储生成，后者只允许用户手动绑定，重新扫描不得覆盖。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewItem {
    pub kind: ItemKind,
    pub title: String,
    pub folder_path: String,
    /// 目录内分组键；`None` 表示独占整个目录。
    ///
    /// 根目录下直接放视频时，同一目录里会有多个条目，靠它区分文件归属，
    /// 并参与唯一索引（`folder_path` + `file_filter`）。
    pub file_filter: Option<String>,
}

/// 扫描写入的结果，供上报扫描统计。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpsertOutcome {
    Created(LibraryItem),
    Updated(LibraryItem),
    Unchanged(LibraryItem),
}

/// 一次扫描的统计结果。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanReport {
    pub kind: ItemKind,
    pub root: String,
    pub folders_seen: usize,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
}

impl ScanReport {
    pub fn new(kind: ItemKind, root: &std::path::Path) -> Self {
        Self {
            kind,
            root: root.to_string_lossy().to_string(),
            folders_seen: 0,
            created: 0,
            updated: 0,
            unchanged: 0,
        }
    }

    pub fn record(&mut self, outcome: &UpsertOutcome) {
        match outcome {
            UpsertOutcome::Created(_) => self.created += 1,
            UpsertOutcome::Updated(_) => self.updated += 1,
            UpsertOutcome::Unchanged(_) => self.unchanged += 1,
        }
    }

    pub fn total_items(&self) -> usize {
        self.created + self.updated + self.unchanged
    }
}

/// 条目内一个可播放/可启动的具体文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaFile {
    /// 相对条目目录的路径（`/` 分隔）。同一集在多个压制组下会指向不同路径。
    pub name: String,
    pub size_bytes: u64,
    pub episode: Option<u32>,
    /// 来源目录（相对条目目录，如 `Season 2`、`[VCB-Studio] ...`）；直属文件为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
}

/// 按需探测磁盘得到的条目媒体摘要。
///
/// plan.md 5.1 未给条目表留集数/文件字段，故文件信息在读取时实时探测，
/// 数据库只保存稳定索引，避免扫描后文件变动导致 DB 与磁盘不一致。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaProbe {
    pub file_count: usize,
    pub episode_hint: Option<String>,
    pub files: Vec<MediaFile>,
}

impl MediaProbe {
    pub fn empty() -> Self {
        Self {
            file_count: 0,
            episode_hint: None,
            files: Vec::new(),
        }
    }
}

/// `progress` 一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub item_id: String,
    pub episode: i64,
    pub watched_seconds: i64,
    /// `local` 或 `cloud`。
    pub source: String,
    pub updated_at: String,
}

/// 进度来源，对应 `progress.source`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProgressSource {
    Local,
    Cloud,
}

impl ProgressSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Cloud => "cloud",
        }
    }
}

/// `game_binding_index` 一行，索引自条目文件夹内的 .gamelauncher.json。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameBindingIndex {
    pub item_id: String,
    pub exe_path: String,
    pub last_launched_at: Option<String>,
    pub last_pid: Option<i64>,
}

/// `sync_queue.status`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SyncStatus {
    Pending,
    Sending,
    Failed,
    Done,
}

impl SyncStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Sending => "sending",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }

    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "pending" => Ok(Self::Pending),
            "sending" => Ok(Self::Sending),
            "failed" => Ok(Self::Failed),
            "done" => Ok(Self::Done),
            other => Err(CoreError::InvalidSyncStatus(other.to_owned())),
        }
    }
}

/// sync_queue.op 的取值常量（plan.md 5.1：op 为字符串）。
pub mod sync_op {
    /// 推送收藏状态 + 评分。
    pub const COLLECTION: &str = "collection";
    /// 推送单集观看进度。
    pub const PROGRESS: &str = "progress";
}

/// conflict.field 的取值常量。
pub mod conflict_field {
    pub const COLLECTION_STATUS: &str = "collection_status";
    pub const RATING: &str = "rating";
    /// 进度冲突按 episode 区分，field 形如 `progress:5`（见 conflict_for_progress）。
    pub const PROGRESS_PREFIX: &str = "progress";
}

/// `sync_queue` 一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncQueueEntry {
    pub id: String,
    pub item_id: Option<String>,
    pub op: String,
    pub payload_json: String,
    pub status: SyncStatus,
    pub attempts: i64,
    pub next_retry_at: Option<String>,
    pub created_at: String,
}

/// `conflict` 一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Conflict {
    pub id: String,
    pub item_id: Option<String>,
    pub field: String,
    pub local_value: Option<String>,
    pub cloud_value: Option<String>,
    pub local_ts: Option<String>,
    pub cloud_ts: Option<String>,
    pub resolved: bool,
}

/// 新建冲突的入参。`id` 与 `resolved` 由仓储生成。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewConflict {
    pub item_id: Option<String>,
    pub field: String,
    pub local_value: Option<String>,
    pub cloud_value: Option<String>,
    pub local_ts: Option<String>,
    pub cloud_ts: Option<String>,
}

/// `metadata_index` 一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MetadataIndex {
    pub subject_id: i64,
    pub cached_path: String,
    pub fetched_at: String,
}

/// `plugin_registry` 一行。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRegistryEntry {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub manifest_path: String,
    pub enabled: bool,
    pub installed_at: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_kind_round_trip() {
        for kind in [ItemKind::Anime, ItemKind::Game] {
            assert_eq!(ItemKind::parse(kind.as_str()).expect("kind"), kind);
        }
        assert!(ItemKind::parse("movie").is_err());
    }

    #[test]
    fn scan_report_counts_by_outcome() {
        let mut report = ScanReport::new(ItemKind::Anime, std::path::Path::new("D:/anime"));
        let item = LibraryItem {
            id: "id".into(),
            kind: ItemKind::Anime,
            title: "t".into(),
            bangumi_subject_id: None,
            folder_path: "D:/anime/a".into(),
            created_at: "now".into(),
            updated_at: "now".into(),
            collection_status: None,
            rating: None,
            collection_updated_at: None,
            file_filter: None,
        };
        report.record(&UpsertOutcome::Created(item.clone()));
        report.record(&UpsertOutcome::Unchanged(item));
        assert_eq!(report.created, 1);
        assert_eq!(report.unchanged, 1);
        assert_eq!(report.total_items(), 2);
    }
}
