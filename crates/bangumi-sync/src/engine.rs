use std::sync::Arc;
use std::time::Duration;

use chrono::SecondsFormat;
use library_core::model::{Conflict, ProgressSource, SyncStatus};
use library_core::{
    ConflictRepository, LibraryRepository, ProgressRepository, SyncQueueRepository,
};

use crate::client::BangumiClient;
use crate::conflict::{detect_collection_conflicts, detect_progress_conflicts};
use crate::error::{SyncError, SyncResult};
use crate::rate_limit::{backoff_delay, RateLimiter};
use crate::types::{CollectionPayload, LocalCollection, LocalProgress, ProgressPayload};

/// 单条目拉取结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullReport {
    pub item_id: String,
    pub subject_id: i64,
    /// 本次新增/刷新的冲突记录（供上层发 `sync:conflict:detected` 事件）。
    pub conflicts: Vec<Conflict>,
    /// 是否从云端补齐了收藏状态或评分。
    pub imported_collection: bool,
    /// 从云端导入的单集进度条数。
    pub imported_progress: usize,
}

/// 一次推送的结果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushReport {
    pub attempted: usize,
    pub succeeded: usize,
    pub failed: usize,
}

/// 同步引擎：拉取比较与队列推送的唯一编排点。
///
/// 所有外部依赖都是 trait 对象，网络实现可替换、离线桩可测试；
/// 限速与重试策略集中在引擎内，调用方（app-services）只触发动作。
pub struct SyncEngine {
    client: Arc<dyn BangumiClient>,
    queue: Arc<dyn SyncQueueRepository>,
    conflicts: Arc<dyn ConflictRepository>,
    library: Arc<dyn LibraryRepository>,
    progress: Arc<dyn ProgressRepository>,
    limiter: RateLimiter,
    backoff_base: Duration,
    backoff_cap: Duration,
}

impl SyncEngine {
    pub fn new(
        client: Arc<dyn BangumiClient>,
        queue: Arc<dyn SyncQueueRepository>,
        conflicts: Arc<dyn ConflictRepository>,
        library: Arc<dyn LibraryRepository>,
        progress: Arc<dyn ProgressRepository>,
    ) -> Self {
        Self {
            client,
            queue,
            conflicts,
            library,
            progress,
            limiter: RateLimiter::default_interval(),
            backoff_base: Duration::from_secs(3),
            backoff_cap: Duration::from_secs(600),
        }
    }

    /// 覆盖限速间隔（测试用）。
    pub fn with_limiter(mut self, limiter: RateLimiter) -> Self {
        self.limiter = limiter;
        self
    }

    /// 覆盖退避参数（测试用）。
    pub fn with_backoff(mut self, base: Duration, cap: Duration) -> Self {
        self.backoff_base = base;
        self.backoff_cap = cap;
        self
    }

    /// 拉取单个条目的云端收藏与进度，入库、记冲突。
    pub async fn pull_item(
        &self,
        local: &LocalCollection,
        local_progress: &[LocalProgress],
    ) -> SyncResult<PullReport> {
        if !self.client.is_authorized() {
            return Err(SyncError::NotAuthorized);
        }

        let mut report = PullReport {
            item_id: local.item_id.clone(),
            subject_id: local.subject_id,
            ..Default::default()
        };

        // 收藏状态 / 评分。
        let cloud_collection = self.client.fetch_collection(local.subject_id).await?;
        let collection_conflicts = detect_collection_conflicts(local, &cloud_collection);
        for conflict in &collection_conflicts {
            report
                .conflicts
                .push(self.conflicts.upsert_for_item(conflict)?);
        }
        let effective_status = local
            .collection_status
            .or(cloud_collection.collection_status);
        let effective_rating = local.rating.or(cloud_collection.rating);
        if effective_status != local.collection_status || effective_rating != local.rating {
            self.library
                .set_collection(&local.item_id, effective_status, effective_rating)?;
            report.imported_collection = true;
        }

        // 单集进度。
        let cloud_progress = self.client.fetch_episode_progress(local.subject_id).await?;
        let progress_conflicts = detect_progress_conflicts(local_progress, &cloud_progress);
        for conflict in &progress_conflicts {
            report
                .conflicts
                .push(self.conflicts.upsert_for_item(conflict)?);
        }
        let local_episodes: std::collections::HashSet<u32> =
            local_progress.iter().map(|row| row.episode).collect();
        for row in &cloud_progress {
            if row.watched_seconds == 0 {
                // 云端「未看」不落库：整部番会给每一集写一行 0 秒，纯噪声。
                // 「本地看过 / 云端未看」的分歧已由冲突记录承接。
                continue;
            }
            if local_episodes.contains(&row.episode) {
                // 本地已有该集：保留本地进度，差异已由冲突记录承接。
                continue;
            }
            self.progress.upsert(
                &local.item_id,
                row.episode,
                row.watched_seconds,
                ProgressSource::Cloud,
            )?;
            report.imported_progress += 1;
        }

        tracing::info!(
            item = %report.item_id,
            subject = report.subject_id,
            conflicts = report.conflicts.len(),
            imported_progress = report.imported_progress,
            "拉取条目完成"
        );
        Ok(report)
    }

    /// 按限速推送一批待发送项。
    ///
    /// 遇到未授权直接返回错误，剩余队列保持 pending；单条失败按指数退避写回 next_retry_at。
    pub async fn push_pending(&self, limit: usize) -> SyncResult<PushReport> {
        if !self.client.is_authorized() {
            return Err(SyncError::NotAuthorized);
        }

        let entries = self.queue.list_pending(limit)?;
        let mut report = PushReport::default();

        for entry in entries {
            self.limiter.acquire().await;
            let sending = self.queue.mark_sending(&entry.id)?;
            report.attempted += 1;

            let result = match entry.op.as_str() {
                library_core::model::sync_op::COLLECTION => {
                    let payload = CollectionPayload::from_json(&entry.payload_json)?;
                    self.client.push_collection(payload).await
                }
                library_core::model::sync_op::PROGRESS => {
                    let payload = ProgressPayload::from_json(&entry.payload_json)?;
                    self.client.push_episode_progress(payload).await
                }
                other => Err(SyncError::Internal(format!("未知同步操作：{other}"))),
            };

            match result {
                Ok(()) => {
                    self.queue.mark_done(&entry.id)?;
                    report.succeeded += 1;
                }
                Err(SyncError::NotAuthorized) => {
                    // 授权失效时把本条放回 pending，不累加退避，交由上层提示重新授权。
                    self.queue
                        .mark_failed(&entry.id, Some(&retry_at(self.backoff_base)))?;
                    return Err(SyncError::NotAuthorized);
                }
                Err(err) => {
                    let attempts = sending.attempts.max(1);
                    let delay = backoff_delay(attempts, self.backoff_base, self.backoff_cap);
                    self.queue.mark_failed(&entry.id, Some(&retry_at(delay)))?;
                    report.failed += 1;
                    tracing::warn!(
                        entry = %entry.id,
                        op = %entry.op,
                        attempts,
                        delay_ms = delay.as_millis() as u64,
                        error = %err,
                        "推送失败，已安排重试"
                    );
                }
            }
        }

        if report.attempted > 0 {
            tracing::info!(
                attempted = report.attempted,
                succeeded = report.succeeded,
                failed = report.failed,
                "本轮推送结束"
            );
        }
        Ok(report)
    }

    /// 队列与冲突的统计（供设置页/同步页展示）。
    pub fn stats(&self) -> SyncResult<SyncStats> {
        Ok(SyncStats {
            pending: self.queue.count_by_status(SyncStatus::Pending)?,
            failed: self.queue.count_by_status(SyncStatus::Failed)?,
            done: self.queue.count_by_status(SyncStatus::Done)?,
            unresolved_conflicts: self.conflicts.list_unresolved()?.len(),
        })
    }
}

/// 同步概览统计。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub pending: i64,
    pub failed: i64,
    pub done: i64,
    pub unresolved_conflicts: usize,
}

fn retry_at(delay: Duration) -> String {
    let seconds = i64::try_from(delay.as_secs()).unwrap_or(i64::MAX);
    (chrono::Utc::now() + chrono::Duration::seconds(seconds))
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_core::model::{ItemKind, NewItem, UpsertOutcome};
    use library_core::{
        SqliteConflictRepository, SqliteProgressRepository, SqliteRepository,
        SqliteSyncQueueRepository,
    };

    use crate::client::OfflineClient;
    use crate::types::{CloudCollection, CloudEpisodeProgress};

    struct Fixture {
        db: Arc<library_core::Db>,
        id: String,
    }

    impl Fixture {
        fn repo(&self) -> SqliteRepository {
            SqliteRepository::new(Arc::clone(&self.db))
        }

        fn progress(&self) -> SqliteProgressRepository {
            SqliteProgressRepository::new(Arc::clone(&self.db))
        }

        fn queue(&self) -> SqliteSyncQueueRepository {
            SqliteSyncQueueRepository::new(Arc::clone(&self.db))
        }

        fn conflicts(&self) -> SqliteConflictRepository {
            SqliteConflictRepository::new(Arc::clone(&self.db))
        }
    }

    fn fixture() -> Fixture {
        let db = Arc::new(library_core::Db::open_in_memory().expect("db"));
        db.migrate().expect("migrate");
        let repo = SqliteRepository::new(Arc::clone(&db));
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
        repo.bind_bangumi_subject(&id, Some(400602)).expect("bind");
        Fixture { db, id }
    }

    fn engine(fx: &Fixture, client: Arc<OfflineClient>) -> SyncEngine {
        SyncEngine::new(
            client,
            Arc::new(fx.queue()),
            Arc::new(fx.conflicts()),
            Arc::new(fx.repo()),
            Arc::new(fx.progress()),
        )
        .with_limiter(RateLimiter::new(Duration::from_millis(1)))
        .with_backoff(Duration::from_secs(3), Duration::from_secs(60))
    }

    fn local(fx: &Fixture) -> LocalCollection {
        let item = fx.repo().get_item(&fx.id).expect("get").expect("exists");
        LocalCollection {
            item_id: item.id,
            subject_id: item.bangumi_subject_id.unwrap_or_default(),
            collection_status: item.collection_status,
            rating: item.rating,
            collection_updated_at: item.collection_updated_at,
        }
    }

    #[tokio::test]
    async fn pull_imports_missing_collection_and_progress() {
        let fx = fixture();
        let client = Arc::new(
            OfflineClient::new(true)
                .with_cloud_collection(CloudCollection {
                    subject_id: 400602,
                    collection_status: Some(3),
                    rating: Some(8),
                    updated_at: Some("2026-10-29T09:00:00Z".into()),
                })
                .with_cloud_progress(
                    400602,
                    vec![CloudEpisodeProgress {
                        subject_id: 400602,
                        episode: 1,
                        watched_seconds: 600,
                        updated_at: None,
                    }],
                ),
        );
        let engine = engine(&fx, client);
        let report = engine.pull_item(&local(&fx), &[]).await.expect("pull");

        assert!(report.conflicts.is_empty());
        assert!(report.imported_collection);
        assert_eq!(report.imported_progress, 1);

        let stored = fx.repo().get_item(&fx.id).expect("get").expect("exists");
        assert_eq!(stored.collection_status, Some(3));
        assert_eq!(stored.rating, Some(8));
        let progress = fx.progress().list_by_item(&fx.id).expect("progress");
        assert_eq!(progress.len(), 1);
        assert_eq!(progress[0].source, "cloud");
    }

    #[tokio::test]
    async fn pull_records_conflict_without_overwriting_local() {
        let fx = fixture();
        fx.repo()
            .set_collection(&fx.id, Some(2), Some(6))
            .expect("local");
        let client = Arc::new(
            OfflineClient::new(true).with_cloud_collection(CloudCollection {
                subject_id: 400602,
                collection_status: Some(3),
                rating: Some(6),
                updated_at: Some("2026-10-29T09:00:00Z".into()),
            }),
        );
        let engine = engine(&fx, client);
        let report = engine.pull_item(&local(&fx), &[]).await.expect("pull");

        assert_eq!(report.conflicts.len(), 1);
        assert_eq!(report.conflicts[0].field, "collection_status");
        assert!(!report.imported_collection);

        // 本地值保持不变。
        let stored = fx.repo().get_item(&fx.id).expect("get").expect("exists");
        assert_eq!(stored.collection_status, Some(2));
        assert_eq!(fx.conflicts().list_unresolved().expect("list").len(), 1);
    }

    #[tokio::test]
    async fn push_drains_queue_and_records_calls() {
        let fx = fixture();
        fx.queue()
            .enqueue(
                Some(&fx.id),
                "collection",
                &CollectionPayload {
                    item_id: fx.id.clone(),
                    subject_id: 400602,
                    collection_status: Some(3),
                    rating: Some(8),
                }
                .to_json()
                .expect("json"),
            )
            .expect("enqueue");
        let client = Arc::new(OfflineClient::new(true));
        let engine = engine(&fx, client.clone());
        let report = engine.push_pending(10).await.expect("push");

        assert_eq!(report.attempted, 1);
        assert_eq!(report.succeeded, 1);
        assert_eq!(client.pushed().len(), 1);
        assert_eq!(
            fx.queue().count_by_status(SyncStatus::Done).expect("done"),
            1
        );
    }

    #[tokio::test]
    async fn push_failure_schedules_retry_with_backoff() {
        let fx = fixture();
        fx.queue()
            .enqueue(
                Some(&fx.id),
                "progress",
                &ProgressPayload {
                    item_id: fx.id.clone(),
                    subject_id: 400602,
                    episode: 1,
                    watched_seconds: 300,
                }
                .to_json()
                .expect("json"),
            )
            .expect("enqueue");
        let client = Arc::new(OfflineClient::new(true));
        client.fail_subject(400602);
        let engine = engine(&fx, client);
        let report = engine.push_pending(10).await.expect("push");

        assert_eq!(report.attempted, 1);
        assert_eq!(report.failed, 1);
        let failed = fx
            .queue()
            .count_by_status(SyncStatus::Failed)
            .expect("failed");
        assert_eq!(failed, 1);
    }

    #[tokio::test]
    async fn push_without_authorization_errors_and_keeps_pending() {
        let fx = fixture();
        fx.queue()
            .enqueue(Some(&fx.id), "collection", "{}")
            .expect("enqueue");
        let client = Arc::new(OfflineClient::new(false));
        let engine = engine(&fx, client);
        let err = engine.push_pending(10).await.expect_err("未授权");
        assert_eq!(err.code(), "sync_not_authorized");
        assert_eq!(
            fx.queue()
                .count_by_status(SyncStatus::Pending)
                .expect("pending"),
            1
        );
    }

    #[tokio::test]
    async fn stats_reports_counts() {
        let fx = fixture();
        fx.queue()
            .enqueue(Some(&fx.id), "collection", "{}")
            .expect("e");
        let client = Arc::new(OfflineClient::new(true));
        let engine = engine(&fx, client);
        let stats = engine.stats().expect("stats");
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.unresolved_conflicts, 0);
    }
}
