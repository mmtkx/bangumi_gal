/// bangumi-sync 的错误类型。
///
/// 不直接复用 `library_core::CoreError`：同步相关的网络/授权失败与数据库错误语义不同，
/// 应用层需要一个统一的 `AppError` 聚合点。
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("尚未完成 Bangumi 授权，缺少 token")]
    NotAuthorized,

    #[error("Bangumi API 调用失败：{0}")]
    Api(String),

    #[error("推送队列项不存在：{0}")]
    QueueEntryNotFound(String),

    #[error("同步载荷解析失败：{0}")]
    Payload(String),

    #[error("同步引擎内部错误：{0}")]
    Internal(String),

    #[error("同步仓储操作失败：{0}")]
    Storage(#[from] library_core::CoreError),
}

impl SyncError {
    /// 稳定错误码，前端据此分支。
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotAuthorized => "sync_not_authorized",
            Self::Api(_) => "sync_api_error",
            Self::QueueEntryNotFound(_) => "sync_queue_entry_not_found",
            Self::Payload(_) => "sync_payload_error",
            Self::Internal(_) => "sync_internal_error",
            Self::Storage(_) => "sync_storage_error",
        }
    }
}

pub type SyncResult<T> = std::result::Result<T, SyncError>;
