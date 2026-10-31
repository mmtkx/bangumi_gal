use std::fmt;
use std::sync::Arc;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::error::{CoreError, Result};
use crate::model::ItemKind;
use crate::time::now_iso8601;

/// 主干道事件主题的唯一定义处（plan.md 3.3）。
///
/// 命名规则：`<domain>:<entity>:<action>`，全小写。
/// 已实现事件集中在此；规划中（M2-M4）的事件只登记名字，不提前实现。
pub mod topic {
    pub const LIBRARY_ITEM_CREATED: &str = "library:item:created";
    pub const LIBRARY_ITEM_UPDATED: &str = "library:item:updated";
    pub const LIBRARY_ITEM_DELETED: &str = "library:item:deleted";
    pub const LIBRARY_ITEM_BOUND: &str = "library:item:bound";

    pub const SCANNER_JOB_STARTED: &str = "scanner:job:started";
    pub const SCANNER_JOB_FINISHED: &str = "scanner:job:finished";
    pub const SCANNER_JOB_FAILED: &str = "scanner:job:failed";

    pub const LAUNCHER_GAME_STARTED: &str = "launcher:game:started";
    pub const LAUNCHER_GAME_EXITED: &str = "launcher:game:exited";
    pub const PLAYER_SESSION_STARTED: &str = "player:session:started";
    pub const PLAYER_SESSION_PROGRESS: &str = "player:session:progress";
    /// plan.md 3.3 未列出：会话结束需要独立主题，否则前端无法区分「进度更新」与「已停止」。
    pub const PLAYER_SESSION_FINISHED: &str = "player:session:finished";

    /// 批量刷新元数据的进度（plan.md 3.3 未列出：批量操作需要可见的进度反馈）。
    pub const METADATA_BATCH_PROGRESS: &str = "metadata:batch:progress";
    /// 某个条目的封面已下载并缓存，前端可重新取图。
    pub const METADATA_COVER_READY: &str = "metadata:cover:ready";

    // 以下为 plan.md 3.3 已登记、尚未实现的主题，保留命名以免后续改名破坏订阅方。
    pub const SYNC_QUEUE_ENQUEUED: &str = "sync:queue:enqueued";
    pub const SYNC_CONFLICT_DETECTED: &str = "sync:conflict:detected";
    pub const PLUGIN_LIFECYCLE_LOADED: &str = "plugin:lifecycle:loaded";
    pub const PLUGIN_LIFECYCLE_UNLOADED: &str = "plugin:lifecycle:unloaded";
    /// 插件经宿主 API 主动发出的事件，统一汇聚到该主题。
    pub const PLUGIN_EVENT: &str = "plugin:event:emitted";
    /// Bangumi 授权状态变化（登录成功 / 退出登录 / token 失效）。
    pub const BANGUMI_AUTH_CHANGED: &str = "bangumi:auth:changed";
}

/// 校验过的事件主题。
///
/// 只在解析外部字符串时构造，保证总线上的主题一定符合命名规则。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Topic(String);

impl Topic {
    pub fn parse(raw: &str) -> Result<Self> {
        let segments: Vec<&str> = raw.split(':').collect();
        if segments.len() != 3 {
            return Err(CoreError::InvalidTopic(raw.to_owned()));
        }
        let valid = segments.iter().all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        });
        if !valid {
            return Err(CoreError::InvalidTopic(raw.to_owned()));
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn domain(&self) -> &str {
        self.0.split(':').next().unwrap_or_default()
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl TryFrom<&str> for Topic {
    type Error = CoreError;

    fn try_from(value: &str) -> Result<Self> {
        Self::parse(value)
    }
}

/// 主干道事件。每个变体对应一个固定主题，禁止运行时拼主题名。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "topic")]
pub enum AppEvent {
    #[serde(rename = "library:item:created")]
    LibraryItemCreated {
        id: String,
        kind: ItemKind,
        title: String,
        folder_path: String,
    },

    #[serde(rename = "library:item:updated")]
    LibraryItemUpdated {
        id: String,
        kind: ItemKind,
        title: String,
        folder_path: String,
    },

    #[serde(rename = "library:item:deleted")]
    LibraryItemDeleted {
        id: String,
        kind: ItemKind,
        title: String,
    },

    #[serde(rename = "library:item:bound")]
    LibraryItemBound {
        id: String,
        bangumi_subject_id: Option<i64>,
    },

    #[serde(rename = "scanner:job:started")]
    ScannerJobStarted { kind: ItemKind, root: String },

    #[serde(rename = "scanner:job:finished")]
    ScannerJobFinished {
        kind: ItemKind,
        root: String,
        created: usize,
        updated: usize,
        unchanged: usize,
    },

    #[serde(rename = "scanner:job:failed")]
    ScannerJobFailed {
        kind: ItemKind,
        root: String,
        message: String,
    },

    #[serde(rename = "launcher:game:started")]
    LauncherGameStarted {
        item_id: String,
        exe_path: String,
        pid: u32,
    },

    #[serde(rename = "launcher:game:exited")]
    LauncherGameExited { item_id: String, pid: u32 },

    #[serde(rename = "player:session:started")]
    PlayerSessionStarted {
        item_id: String,
        episode: u32,
        file_name: String,
        pid: u32,
    },

    #[serde(rename = "player:session:progress")]
    PlayerSessionProgress {
        item_id: String,
        episode: u32,
        watched_seconds: u64,
    },

    #[serde(rename = "player:session:finished")]
    PlayerSessionFinished {
        item_id: String,
        episode: u32,
        watched_seconds: u64,
    },

    /// 批量刷新元数据时逐条推进的进度。
    #[serde(rename = "metadata:batch:progress")]
    MetadataBatchProgress {
        done: usize,
        total: usize,
        item_id: String,
        ok: bool,
    },

    /// 封面缓存完成，前端收到后重新读取该条目的封面。
    #[serde(rename = "metadata:cover:ready")]
    MetadataCoverReady { item_id: String },

    #[serde(rename = "sync:queue:enqueued")]
    SyncQueueEnqueued {
        id: String,
        item_id: Option<String>,
        op: String,
    },

    #[serde(rename = "sync:conflict:detected")]
    SyncConflictDetected { item_id: String, field: String },

    #[serde(rename = "plugin:lifecycle:loaded")]
    PluginLifecycleLoaded { id: String },

    #[serde(rename = "plugin:lifecycle:unloaded")]
    PluginLifecycleUnloaded { id: String },

    /// 插件通过宿主 API 发出的事件；`name` 由插件自定义。
    #[serde(rename = "plugin:event:emitted")]
    PluginEmitted {
        id: String,
        name: String,
        payload: serde_json::Value,
    },

    /// 授权状态变化：侧栏头像、账号页都据此刷新。
    #[serde(rename = "bangumi:auth:changed")]
    BangumiAuthChanged { authorized: bool },
}

impl AppEvent {
    pub fn topic(&self) -> &'static str {
        match self {
            Self::LibraryItemCreated { .. } => topic::LIBRARY_ITEM_CREATED,
            Self::LibraryItemUpdated { .. } => topic::LIBRARY_ITEM_UPDATED,
            Self::LibraryItemDeleted { .. } => topic::LIBRARY_ITEM_DELETED,
            Self::LibraryItemBound { .. } => topic::LIBRARY_ITEM_BOUND,
            Self::ScannerJobStarted { .. } => topic::SCANNER_JOB_STARTED,
            Self::ScannerJobFinished { .. } => topic::SCANNER_JOB_FINISHED,
            Self::ScannerJobFailed { .. } => topic::SCANNER_JOB_FAILED,
            Self::LauncherGameStarted { .. } => topic::LAUNCHER_GAME_STARTED,
            Self::LauncherGameExited { .. } => topic::LAUNCHER_GAME_EXITED,
            Self::PlayerSessionStarted { .. } => topic::PLAYER_SESSION_STARTED,
            Self::PlayerSessionProgress { .. } => topic::PLAYER_SESSION_PROGRESS,
            Self::PlayerSessionFinished { .. } => topic::PLAYER_SESSION_FINISHED,
            Self::MetadataBatchProgress { .. } => topic::METADATA_BATCH_PROGRESS,
            Self::MetadataCoverReady { .. } => topic::METADATA_COVER_READY,
            Self::SyncQueueEnqueued { .. } => topic::SYNC_QUEUE_ENQUEUED,
            Self::SyncConflictDetected { .. } => topic::SYNC_CONFLICT_DETECTED,
            Self::PluginLifecycleLoaded { .. } => topic::PLUGIN_LIFECYCLE_LOADED,
            Self::PluginLifecycleUnloaded { .. } => topic::PLUGIN_LIFECYCLE_UNLOADED,
            Self::PluginEmitted { .. } => topic::PLUGIN_EVENT,
            Self::BangumiAuthChanged { .. } => topic::BANGUMI_AUTH_CHANGED,
        }
    }

    /// 转成前端/插件可直接消费的信封：主题平铺在 `payload` 之外，字段名统一 camelCase。
    pub fn to_envelope(&self) -> Result<EventEnvelope> {
        let mut payload =
            serde_json::to_value(self).map_err(|err| CoreError::Serialize(err.to_string()))?;
        if let Some(map) = payload.as_object_mut() {
            map.remove("topic");
        }
        camel_case_keys(&mut payload);
        Ok(EventEnvelope {
            topic: self.topic().to_owned(),
            payload,
            emitted_at: now_iso8601(),
        })
    }
}

/// 事件字段在 Rust 侧是 snake_case，跨到前端统一转 camelCase，与 command DTO 保持一致。
fn camel_case_keys(value: &mut serde_json::Value) {
    let Some(map) = value.as_object_mut() else {
        return;
    };
    // serde_json::Map 没有 drain，先整体取出再重建。
    let original = std::mem::take(map);
    for (key, val) in original {
        map.insert(snake_to_camel(&key), val);
    }
}

fn snake_to_camel(key: &str) -> String {
    if !key.contains('_') {
        return key.to_owned();
    }
    let mut out = String::with_capacity(key.len());
    let mut uppercase_next = false;
    for ch in key.chars() {
        if ch == '_' {
            uppercase_next = true;
            continue;
        }
        if uppercase_next {
            out.extend(ch.to_uppercase());
            uppercase_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// 事件信封：总线内部传事件本体，跨进程/前端传信封。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub topic: String,
    pub payload: serde_json::Value,
    pub emitted_at: String,
}

/// 进程内主干道事件总线（ADR-011）。
///
/// 使用 broadcast：订阅方各自持有游标，慢订阅者丢旧事件而不是阻塞发布方。
#[derive(Clone)]
pub struct EventBus {
    sender: broadcast::Sender<Arc<AppEvent>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::with_capacity(256)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        let (sender, _) = broadcast::channel(capacity.max(16));
        Self { sender }
    }

    /// 发布事件。没有订阅者时不视为错误，返回 0。
    pub fn publish(&self, event: AppEvent) -> usize {
        self.sender.send(Arc::new(event)).unwrap_or(0)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<AppEvent>> {
        self.sender.subscribe()
    }

    pub fn subscriber_count(&self) -> usize {
        self.sender.receiver_count()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

/// 事件发布能力的最小接口。
///
/// scanner 等功能 crate 只依赖这个 trait，就能在不依赖具体总线实现的前提下发事件，
/// 也让单元测试可以注入空实现。
pub trait EventPublisher: Send + Sync {
    fn publish(&self, event: AppEvent) -> usize;
}

impl EventPublisher for EventBus {
    fn publish(&self, event: AppEvent) -> usize {
        EventBus::publish(self, event)
    }
}

/// 不做任何事的发布器，用于测试与离线路径。
pub struct NoopPublisher;

impl EventPublisher for NoopPublisher {
    fn publish(&self, _event: AppEvent) -> usize {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_events() -> Vec<AppEvent> {
        vec![
            AppEvent::LibraryItemCreated {
                id: "1".into(),
                kind: ItemKind::Anime,
                title: "t".into(),
                folder_path: "D:/a".into(),
            },
            AppEvent::LibraryItemUpdated {
                id: "1".into(),
                kind: ItemKind::Anime,
                title: "t".into(),
                folder_path: "D:/a".into(),
            },
            AppEvent::LibraryItemDeleted {
                id: "1".into(),
                kind: ItemKind::Game,
                title: "t".into(),
            },
            AppEvent::LibraryItemBound {
                id: "1".into(),
                bangumi_subject_id: Some(42),
            },
            AppEvent::ScannerJobStarted {
                kind: ItemKind::Anime,
                root: "D:/a".into(),
            },
            AppEvent::ScannerJobFinished {
                kind: ItemKind::Anime,
                root: "D:/a".into(),
                created: 1,
                updated: 0,
                unchanged: 0,
            },
            AppEvent::ScannerJobFailed {
                kind: ItemKind::Game,
                root: "D:/g".into(),
                message: "boom".into(),
            },
            AppEvent::LauncherGameStarted {
                item_id: "1".into(),
                exe_path: "D:/g/game.exe".into(),
                pid: 4242,
            },
            AppEvent::LauncherGameExited {
                item_id: "1".into(),
                pid: 4242,
            },
            AppEvent::PlayerSessionStarted {
                item_id: "2".into(),
                episode: 1,
                file_name: "01.mkv".into(),
                pid: 99,
            },
            AppEvent::PlayerSessionProgress {
                item_id: "2".into(),
                episode: 1,
                watched_seconds: 30,
            },
            AppEvent::PlayerSessionFinished {
                item_id: "2".into(),
                episode: 1,
                watched_seconds: 60,
            },
            AppEvent::MetadataBatchProgress {
                done: 1,
                total: 2,
                item_id: "2".into(),
                ok: true,
            },
            AppEvent::MetadataCoverReady {
                item_id: "2".into(),
            },
            AppEvent::SyncQueueEnqueued {
                id: "q".into(),
                item_id: Some("2".into()),
                op: "progress".into(),
            },
            AppEvent::SyncConflictDetected {
                item_id: "2".into(),
                field: "rating".into(),
            },
            AppEvent::PluginLifecycleLoaded {
                id: "com.example".into(),
            },
            AppEvent::PluginLifecycleUnloaded {
                id: "com.example".into(),
            },
            AppEvent::PluginEmitted {
                id: "com.example".into(),
                name: "hello".into(),
                payload: serde_json::json!({ "x": 1 }),
            },
        ]
    }

    #[test]
    fn every_event_topic_follows_naming_rule() {
        for event in sample_events() {
            Topic::parse(event.topic()).expect("主题必须符合 <domain>:<entity>:<action>");
        }
    }

    #[test]
    fn topic_rejects_malformed_names() {
        for bad in [
            "library.item.created",
            "library:item",
            "Library:Item:Created",
            "::",
            "",
        ] {
            assert!(Topic::parse(bad).is_err(), "{bad} 不应通过校验");
        }
        let topic = Topic::parse("library:item:created").expect("valid");
        assert_eq!(topic.domain(), "library");
        assert_eq!(topic.to_string(), "library:item:created");
    }

    #[test]
    fn envelope_lifts_topic_out_of_payload() {
        let event = AppEvent::LibraryItemBound {
            id: "abc".into(),
            bangumi_subject_id: Some(7),
        };
        let envelope = event.to_envelope().expect("envelope");
        assert_eq!(envelope.topic, "library:item:bound");
        assert!(envelope.payload.get("topic").is_none());
        assert_eq!(envelope.payload["id"], "abc");
        assert_eq!(envelope.payload["bangumiSubjectId"], 7);
    }

    #[test]
    fn envelope_uses_camel_case_keys() {
        let event = AppEvent::LauncherGameStarted {
            item_id: "x".into(),
            exe_path: "D:/g/game.exe".into(),
            pid: 7,
        };
        let envelope = event.to_envelope().expect("envelope");
        assert_eq!(envelope.payload["itemId"], "x");
        assert_eq!(envelope.payload["exePath"], "D:/g/game.exe");
        assert!(envelope.payload.get("item_id").is_none());
    }

    #[tokio::test]
    async fn publish_reaches_subscriber() {
        let bus = EventBus::new();
        let mut rx = bus.subscribe();
        let event = AppEvent::LibraryItemDeleted {
            id: "x".into(),
            kind: ItemKind::Game,
            title: "t".into(),
        };
        assert_eq!(bus.publish(event), 1);
        let received = rx.recv().await.expect("event");
        assert_eq!(received.topic(), topic::LIBRARY_ITEM_DELETED);
    }

    #[tokio::test]
    async fn publish_without_subscriber_is_not_an_error() {
        let bus = EventBus::new();
        assert_eq!(
            bus.publish(AppEvent::LibraryItemBound {
                id: "x".into(),
                bangumi_subject_id: None
            }),
            0
        );
    }
}
