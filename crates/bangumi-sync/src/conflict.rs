use std::collections::BTreeMap;

use library_core::model::{conflict_field, NewConflict};

use crate::types::{CloudCollection, CloudEpisodeProgress, LocalCollection, LocalProgress};

/// 比较收藏状态与评分，产出冲突记录。
///
/// 规则：仅当两侧都有值且不同才记为冲突；一侧为空视为「待填充/待推送」，
/// 不构成冲突（由引擎按方向直接导入或入队）。
pub fn detect_collection_conflicts(
    local: &LocalCollection,
    cloud: &CloudCollection,
) -> Vec<NewConflict> {
    let mut conflicts = Vec::new();

    if let (Some(local_status), Some(cloud_status)) =
        (local.collection_status, cloud.collection_status)
    {
        if local_status != cloud_status {
            conflicts.push(NewConflict {
                item_id: Some(local.item_id.clone()),
                field: conflict_field::COLLECTION_STATUS.to_owned(),
                local_value: Some(local_status.to_string()),
                cloud_value: Some(cloud_status.to_string()),
                local_ts: local.collection_updated_at.clone(),
                cloud_ts: cloud.updated_at.clone(),
            });
        }
    }

    if let (Some(local_rating), Some(cloud_rating)) = (local.rating, cloud.rating) {
        if local_rating != cloud_rating {
            conflicts.push(NewConflict {
                item_id: Some(local.item_id.clone()),
                field: conflict_field::RATING.to_owned(),
                local_value: Some(local_rating.to_string()),
                cloud_value: Some(cloud_rating.to_string()),
                local_ts: local.collection_updated_at.clone(),
                cloud_ts: cloud.updated_at.clone(),
            });
        }
    }

    conflicts
}

/// 比较单集进度，产出冲突记录。
///
/// Bangumi 的单集进度是布尔状态（看过 / 未看），本地记的是秒数，两者不可直接比大小：
/// 只要两侧都「看过」，秒数差异不构成冲突；只有「看过 / 未看」不一致才需要裁决。
/// 冲突值统一写成 `看过` / `未看`，界面上比数字更直观。
pub fn detect_progress_conflicts(
    local: &[LocalProgress],
    cloud: &[CloudEpisodeProgress],
) -> Vec<NewConflict> {
    let local_by_episode: BTreeMap<u32, &LocalProgress> =
        local.iter().map(|row| (row.episode, row)).collect();
    let cloud_by_episode: BTreeMap<u32, &CloudEpisodeProgress> =
        cloud.iter().map(|row| (row.episode, row)).collect();

    let mut conflicts = Vec::new();
    for (episode, cloud_row) in &cloud_by_episode {
        let Some(local_row) = local_by_episode.get(episode) else {
            // 只有一侧有记录：按方向直接导入或推送，不算冲突。
            continue;
        };
        let local_watched = local_row.watched_seconds > 0;
        let cloud_watched = cloud_row.watched_seconds > 0;
        if local_watched == cloud_watched {
            continue;
        }
        conflicts.push(NewConflict {
            item_id: Some(local_row.item_id.clone()),
            field: format!("{}:{}", conflict_field::PROGRESS_PREFIX, episode),
            local_value: Some(watched_label(local_watched).to_owned()),
            cloud_value: Some(watched_label(cloud_watched).to_owned()),
            local_ts: local_row.updated_at.clone(),
            cloud_ts: cloud_row.updated_at.clone(),
        });
    }
    conflicts
}

/// 冲突值的人读标签，同时被 app-services 用于解析裁决结果。
pub fn watched_label(watched: bool) -> &'static str {
    if watched {
        "看过"
    } else {
        "未看"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_collection(status: Option<i64>, rating: Option<i64>) -> LocalCollection {
        LocalCollection {
            item_id: "item-1".into(),
            subject_id: 400602,
            collection_status: status,
            rating,
            collection_updated_at: Some("2026-10-29T10:00:00Z".into()),
        }
    }

    fn cloud_collection(status: Option<i64>, rating: Option<i64>) -> CloudCollection {
        CloudCollection {
            subject_id: 400602,
            collection_status: status,
            rating,
            updated_at: Some("2026-10-29T09:00:00Z".into()),
        }
    }

    #[test]
    fn differing_collection_status_is_a_conflict() {
        let conflicts = detect_collection_conflicts(
            &local_collection(Some(3), None),
            &cloud_collection(Some(2), None),
        );
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].field, "collection_status");
        assert_eq!(conflicts[0].local_value.as_deref(), Some("3"));
        assert_eq!(conflicts[0].cloud_value.as_deref(), Some("2"));
    }

    #[test]
    fn missing_side_is_not_a_conflict() {
        // 本地有、云端空 → 待推送，不是冲突。
        assert!(detect_collection_conflicts(
            &local_collection(Some(2), Some(9)),
            &cloud_collection(None, None)
        )
        .is_empty());
        // 本地空、云端有 → 待导入，不是冲突。
        assert!(detect_collection_conflicts(
            &local_collection(None, None),
            &cloud_collection(Some(2), Some(9))
        )
        .is_empty());
    }

    #[test]
    fn equal_values_are_not_conflicts() {
        assert!(detect_collection_conflicts(
            &local_collection(Some(2), Some(9)),
            &cloud_collection(Some(2), Some(9))
        )
        .is_empty());
    }

    #[test]
    fn both_watched_is_not_a_conflict_even_with_different_seconds() {
        // 云端只有布尔状态（映射成 1 秒占位），本地 600 秒也算「看过」。
        let local = vec![LocalProgress {
            item_id: "item-1".into(),
            subject_id: 1,
            episode: 1,
            watched_seconds: 600,
            updated_at: None,
        }];
        let cloud = vec![CloudEpisodeProgress {
            subject_id: 1,
            episode: 1,
            watched_seconds: 1,
            updated_at: None,
        }];
        assert!(detect_progress_conflicts(&local, &cloud).is_empty());
    }

    #[test]
    fn watched_locally_but_unwatched_on_cloud_is_a_conflict() {
        let local = vec![LocalProgress {
            item_id: "item-1".into(),
            subject_id: 1,
            episode: 5,
            watched_seconds: 100,
            updated_at: Some("2026-10-29T10:00:00Z".into()),
        }];
        let cloud = vec![CloudEpisodeProgress {
            subject_id: 1,
            episode: 5,
            watched_seconds: 0,
            updated_at: Some("2026-10-29T09:00:00Z".into()),
        }];
        let conflicts = detect_progress_conflicts(&local, &cloud);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].field, "progress:5");
        assert_eq!(conflicts[0].local_value.as_deref(), Some("看过"));
        assert_eq!(conflicts[0].cloud_value.as_deref(), Some("未看"));
    }

    #[test]
    fn episode_only_on_one_side_is_not_a_conflict() {
        let local = vec![LocalProgress {
            item_id: "item-1".into(),
            subject_id: 1,
            episode: 1,
            watched_seconds: 100,
            updated_at: None,
        }];
        let cloud = vec![CloudEpisodeProgress {
            subject_id: 1,
            episode: 2,
            watched_seconds: 500,
            updated_at: None,
        }];
        assert!(detect_progress_conflicts(&local, &cloud).is_empty());
    }

    #[test]
    fn explicit_unwatched_on_one_side_is_a_conflict() {
        // 本地把进度清零（明确「未看」），云端仍标着看过。
        let local = vec![LocalProgress {
            item_id: "item-1".into(),
            subject_id: 1,
            episode: 3,
            watched_seconds: 0,
            updated_at: None,
        }];
        let cloud = vec![CloudEpisodeProgress {
            subject_id: 1,
            episode: 3,
            watched_seconds: 1,
            updated_at: None,
        }];
        let conflicts = detect_progress_conflicts(&local, &cloud);
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].local_value.as_deref(), Some("未看"));
        assert_eq!(conflicts[0].cloud_value.as_deref(), Some("看过"));
    }
}
