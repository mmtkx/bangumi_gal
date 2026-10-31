use serde::{Deserialize, Serialize};

/// 本地收藏快照：参与冲突比较的字段集合（进度另见 `LocalProgress`）。
///
/// 由 app-services 从 `library_item` 组装；本 crate 只负责比较与推送，不直接读库。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalCollection {
    pub item_id: String,
    /// Bangumi 条目 ID；未绑定的条目不能参与同步。
    pub subject_id: i64,
    pub collection_status: Option<i64>,
    pub rating: Option<i64>,
    /// 收藏信息最后一次本地变更时间（`library_item.collection_updated_at`）。
    pub collection_updated_at: Option<String>,
}

/// 云端收藏快照（Bangumi `/v0/users/-/collections/{subject_id}` 的投影）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudCollection {
    pub subject_id: i64,
    pub collection_status: Option<i64>,
    pub rating: Option<i64>,
    /// 云端最后更新时间，用于判断哪一侧更新。
    pub updated_at: Option<String>,
}

/// 本地单集进度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalProgress {
    pub item_id: String,
    pub subject_id: i64,
    pub episode: u32,
    pub watched_seconds: u64,
    pub updated_at: Option<String>,
}

/// 云端单集进度。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudEpisodeProgress {
    pub subject_id: i64,
    pub episode: u32,
    pub watched_seconds: u64,
    pub updated_at: Option<String>,
}

/// 当前登录账号的资料（`GET /v0/me`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountProfile {
    pub id: i64,
    pub username: String,
    #[serde(default)]
    pub nickname: String,
    /// 头像直链（Bangumi 的 CDN），可能缺失。
    #[serde(default)]
    pub avatar_url: Option<String>,
}

/// 关联条目（`GET /v0/subjects/{id}/subjects` 的一项）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelatedSubject {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub name_cn: String,
    /// 关系描述，如「续集」「前传」「角色」。
    #[serde(default)]
    pub relation: String,
    #[serde(default, rename = "type")]
    pub subject_type: i64,
}

/// 搜索命中的 Bangumi 条目（`POST /v0/search/subjects` 的投影）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubjectSearchHit {
    pub id: i64,
    /// 原名（多为日文）。
    #[serde(default)]
    pub name: String,
    /// 中文名，可能缺失。
    #[serde(default)]
    pub name_cn: Option<String>,
    /// Bangumi 站内评分（0-10），不代表匹配度。
    #[serde(default)]
    pub score: Option<f64>,
}

/// 推送收藏状态+评分的载荷，序列化后存入 `sync_queue.payload_json`。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionPayload {
    pub item_id: String,
    pub subject_id: i64,
    pub collection_status: Option<i64>,
    pub rating: Option<i64>,
}

/// 推送单集进度的载荷。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressPayload {
    pub item_id: String,
    pub subject_id: i64,
    pub episode: u32,
    pub watched_seconds: u64,
}

impl CollectionPayload {
    pub fn to_json(&self) -> crate::SyncResult<String> {
        serde_json::to_string(self).map_err(|err| crate::SyncError::Payload(err.to_string()))
    }

    pub fn from_json(raw: &str) -> crate::SyncResult<Self> {
        serde_json::from_str(raw).map_err(|err| crate::SyncError::Payload(err.to_string()))
    }
}

impl ProgressPayload {
    pub fn to_json(&self) -> crate::SyncResult<String> {
        serde_json::to_string(self).map_err(|err| crate::SyncError::Payload(err.to_string()))
    }

    pub fn from_json(raw: &str) -> crate::SyncResult<Self> {
        serde_json::from_str(raw).map_err(|err| crate::SyncError::Payload(err.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_payload_round_trips() {
        let payload = CollectionPayload {
            item_id: "abc".into(),
            subject_id: 400602,
            collection_status: Some(3),
            rating: Some(8),
        };
        let json = payload.to_json().expect("json");
        assert_eq!(CollectionPayload::from_json(&json).expect("back"), payload);
    }

    #[test]
    fn progress_payload_round_trips() {
        let payload = ProgressPayload {
            item_id: "abc".into(),
            subject_id: 400602,
            episode: 5,
            watched_seconds: 1420,
        };
        let json = payload.to_json().expect("json");
        assert_eq!(ProgressPayload::from_json(&json).expect("back"), payload);
    }

    #[test]
    fn invalid_json_reports_payload_error() {
        let err = ProgressPayload::from_json("not-json").expect_err("应当失败");
        assert_eq!(err.code(), "sync_payload_error");
    }
}
