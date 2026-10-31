use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

use crate::error::{SyncError, SyncResult};
use crate::types::{
    AccountProfile, CloudCollection, CloudEpisodeProgress, CollectionPayload, ProgressPayload,
    RelatedSubject, SubjectSearchHit,
};

/// 异步返回的装箱 Future，避免为 async trait 引入额外依赖。
pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Bangumi API 客户端抽象。
///
/// 生产实现基于 reqwest（M3 联网阶段），骨架阶段由 `OfflineClient` 实现，
/// 让同步引擎的编排、限速、冲突逻辑可以在无网络环境下完整测试。
pub trait BangumiClient: Send + Sync {
    /// 是否已持有可用 token。
    fn is_authorized(&self) -> bool;

    /// 主动续期一次，忽略剩余有效期。
    ///
    /// Bangumi 的 access/refresh token 都是 7 天且随刷新一起轮换，
    /// 只在「临近过期」才刷新会让 refresh token 一路老化：用户隔一周再开应用
    /// 就必须重新授权。启动时强制刷一次即可把 7 天窗口重置。
    /// 未持有 token 或实现不支持续期时为空操作。
    fn force_refresh(&self) -> BoxFut<'_, SyncResult<()>> {
        Box::pin(async { Ok(()) })
    }

    /// 拉取指定条目的云端收藏状态与评分。
    fn fetch_collection(&self, subject_id: i64) -> BoxFut<'_, SyncResult<CloudCollection>>;

    /// 推送收藏状态与评分。
    fn push_collection(&self, payload: CollectionPayload) -> BoxFut<'_, SyncResult<()>>;

    /// 拉取指定条目的云端单集进度。
    fn fetch_episode_progress(
        &self,
        subject_id: i64,
    ) -> BoxFut<'_, SyncResult<Vec<CloudEpisodeProgress>>>;

    /// 推送单集进度。
    fn push_episode_progress(&self, payload: ProgressPayload) -> BoxFut<'_, SyncResult<()>>;

    /// 拉取条目元数据（标题、简介、封面等），用于本地缓存（plan.md 6.6 / ADR-009）。
    fn fetch_subject_metadata(&self, subject_id: i64) -> BoxFut<'_, SyncResult<serde_json::Value>>;

    /// 读取当前登录账号的资料（昵称、头像）。
    fn fetch_me(&self) -> BoxFut<'_, SyncResult<AccountProfile>>;

    /// 下载任意公开图片（封面、头像），返回原始字节。不带 token。
    fn fetch_bytes(&self, url: String) -> BoxFut<'_, SyncResult<Vec<u8>>>;

    /// 读取条目的关联条目（续集 / 前传 / 同系列等）。
    fn fetch_related_subjects(
        &self,
        subject_id: i64,
    ) -> BoxFut<'_, SyncResult<Vec<RelatedSubject>>>;

    /// 按关键词搜索条目。`subject_type`：2 动画 / 4 游戏（Bangumi 类型编号）。
    fn search_subjects(
        &self,
        keyword: String,
        subject_type: i64,
    ) -> BoxFut<'_, SyncResult<Vec<SubjectSearchHit>>>;
}

/// 一次推送调用的记录，供测试断言。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushCall {
    Collection(CollectionPayload),
    Progress(ProgressPayload),
}

/// 离线桩客户端：不发网络请求，按预置数据回答拉取、记录推送。
///
/// 用途：
/// - 让 `SyncEngine` 在 M3 骨架阶段即可端到端测试（拉取比较、推送状态机、限速）。
/// - 通过 `fail_subject` 模拟 API 失败，验证重试与退避。
#[derive(Default)]
pub struct OfflineClient {
    authorized: bool,
    cloud_collections: Mutex<HashMap<i64, CloudCollection>>,
    cloud_progress: Mutex<HashMap<i64, Vec<CloudEpisodeProgress>>>,
    metadata: Mutex<HashMap<i64, serde_json::Value>>,
    search_hits: Mutex<Vec<SubjectSearchHit>>,
    account: Option<AccountProfile>,
    fail_subjects: Mutex<HashSet<i64>>,
    pushes: Mutex<Vec<PushCall>>,
}

impl OfflineClient {
    pub fn new(authorized: bool) -> Self {
        Self {
            authorized,
            ..Default::default()
        }
    }

    /// 预置某条目的云端收藏状态。
    pub fn with_cloud_collection(self, collection: CloudCollection) -> Self {
        self.cloud_collections
            .lock()
            .expect("cloud collections")
            .insert(collection.subject_id, collection);
        self
    }

    /// 预置某条目的云端进度。
    pub fn with_cloud_progress(self, subject_id: i64, rows: Vec<CloudEpisodeProgress>) -> Self {
        self.cloud_progress
            .lock()
            .expect("cloud progress")
            .insert(subject_id, rows);
        self
    }

    /// 预置某条目的元数据。
    pub fn with_subject_metadata(self, subject_id: i64, metadata: serde_json::Value) -> Self {
        self.metadata
            .lock()
            .expect("metadata")
            .insert(subject_id, metadata);
        self
    }

    /// 预置搜索命中结果。
    pub fn with_account(self, account: AccountProfile) -> Self {
        Self {
            account: Some(account),
            ..self
        }
    }

    pub fn with_search_hits(self, hits: Vec<SubjectSearchHit>) -> Self {
        *self.search_hits.lock().expect("search hits") = hits;
        self
    }

    /// 让指定条目的推送失败（模拟网络/限流错误）。
    pub fn fail_subject(&self, subject_id: i64) {
        self.fail_subjects
            .lock()
            .expect("fail subjects")
            .insert(subject_id);
    }

    /// 清除失败注入。
    pub fn clear_failures(&self) {
        self.fail_subjects.lock().expect("fail subjects").clear();
    }

    /// 已记录的推送调用。
    pub fn pushed(&self) -> Vec<PushCall> {
        self.pushes.lock().expect("pushes").clone()
    }

    fn should_fail(&self, subject_id: i64) -> bool {
        self.fail_subjects
            .lock()
            .map(|set| set.contains(&subject_id))
            .unwrap_or(false)
    }
}

impl BangumiClient for OfflineClient {
    fn is_authorized(&self) -> bool {
        self.authorized
    }

    fn fetch_collection(&self, subject_id: i64) -> BoxFut<'_, SyncResult<CloudCollection>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            let guard = self
                .cloud_collections
                .lock()
                .map_err(|_| SyncError::Internal("云端收藏缓存锁不可用".to_owned()))?;
            Ok(guard.get(&subject_id).cloned().unwrap_or(CloudCollection {
                subject_id,
                collection_status: None,
                rating: None,
                updated_at: None,
            }))
        })
    }

    fn push_collection(&self, payload: CollectionPayload) -> BoxFut<'_, SyncResult<()>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            if self.should_fail(payload.subject_id) {
                return Err(SyncError::Api(format!(
                    "注入失败：subject {}",
                    payload.subject_id
                )));
            }
            self.pushes
                .lock()
                .map_err(|_| SyncError::Internal("推送记录锁不可用".to_owned()))?
                .push(PushCall::Collection(payload));
            Ok(())
        })
    }

    fn fetch_episode_progress(
        &self,
        subject_id: i64,
    ) -> BoxFut<'_, SyncResult<Vec<CloudEpisodeProgress>>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            let guard = self
                .cloud_progress
                .lock()
                .map_err(|_| SyncError::Internal("云端进度缓存锁不可用".to_owned()))?;
            Ok(guard.get(&subject_id).cloned().unwrap_or_default())
        })
    }

    fn push_episode_progress(&self, payload: ProgressPayload) -> BoxFut<'_, SyncResult<()>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            if self.should_fail(payload.subject_id) {
                return Err(SyncError::Api(format!(
                    "注入失败：subject {}",
                    payload.subject_id
                )));
            }
            self.pushes
                .lock()
                .map_err(|_| SyncError::Internal("推送记录锁不可用".to_owned()))?
                .push(PushCall::Progress(payload));
            Ok(())
        })
    }

    fn fetch_related_subjects(
        &self,
        _subject_id: i64,
    ) -> BoxFut<'_, SyncResult<Vec<RelatedSubject>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    fn fetch_bytes(&self, _url: String) -> BoxFut<'_, SyncResult<Vec<u8>>> {
        Box::pin(async move {
            Err(SyncError::Internal("离线客户端不提供图片下载".to_owned()))
        })
    }

    fn fetch_me(&self) -> BoxFut<'_, SyncResult<AccountProfile>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            self.account
                .clone()
                .ok_or_else(|| SyncError::Internal("离线客户端没有预置账号资料".to_owned()))
        })
    }

    fn fetch_subject_metadata(&self, subject_id: i64) -> BoxFut<'_, SyncResult<serde_json::Value>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            let guard = self
                .metadata
                .lock()
                .map_err(|_| SyncError::Internal("元数据缓存锁不可用".to_owned()))?;
            Ok(guard
                .get(&subject_id)
                .cloned()
                .unwrap_or_else(|| serde_json::json!({ "id": subject_id })))
        })
    }

    fn search_subjects(
        &self,
        _keyword: String,
        _subject_type: i64,
    ) -> BoxFut<'_, SyncResult<Vec<SubjectSearchHit>>> {
        Box::pin(async move {
            if !self.authorized {
                return Err(SyncError::NotAuthorized);
            }
            Ok(self
                .search_hits
                .lock()
                .map_err(|_| SyncError::Internal("搜索缓存锁不可用".to_owned()))?
                .clone())
        })
    }
}

#[cfg(test)]
mod tests {

    use super::*;

    #[tokio::test]
    async fn unauthorized_client_rejects_calls() {
        let client = OfflineClient::new(false);
        assert!(!client.is_authorized());
        let err = client.fetch_collection(1).await.expect_err("应当拒绝");
        assert_eq!(err.code(), "sync_not_authorized");
    }

    #[tokio::test]
    async fn fetch_unknown_subject_returns_empty_collection() {
        let client = OfflineClient::new(true);
        let collection = client.fetch_collection(42).await.expect("fetch");
        assert_eq!(collection.subject_id, 42);
        assert!(collection.collection_status.is_none());
    }

    #[tokio::test]
    async fn push_records_call() {
        let client = OfflineClient::new(true);
        client
            .push_collection(CollectionPayload {
                item_id: "i".into(),
                subject_id: 7,
                collection_status: Some(2),
                rating: Some(9),
            })
            .await
            .expect("push");
        assert_eq!(client.pushed().len(), 1);
        assert!(matches!(client.pushed()[0], PushCall::Collection(_)));
    }

    #[tokio::test]
    async fn injected_failure_surfaces_api_error() {
        let client = OfflineClient::new(true);
        client.fail_subject(7);
        let err = client
            .push_episode_progress(ProgressPayload {
                item_id: "i".into(),
                subject_id: 7,
                episode: 1,
                watched_seconds: 10,
            })
            .await
            .expect_err("应当失败");
        assert_eq!(err.code(), "sync_api_error");
    }
}
