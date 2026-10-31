use std::path::PathBuf;

use thiserror::Error;

/// library-core 的公共结果类型。
pub type Result<T> = std::result::Result<T, CoreError>;

/// 库层统一错误，应用层通过 `AppError` 聚合。
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("数据库操作失败：{0}")]
    Database(#[from] rusqlite::Error),

    #[error("数据库初始化失败：{0}")]
    DatabaseInit(String),

    #[error("数据库连接池状态异常")]
    PoolPoisoned,

    #[error("路径不可用：{path}（{reason}）")]
    PathUnavailable { path: PathBuf, reason: String },

    #[error("目录不存在或不是目录：{0}")]
    NotADirectory(PathBuf),

    #[error("动画根目录与游戏根目录不能是同一个目录：{0}")]
    RootsIdentical(PathBuf),

    #[error("动画根目录 {anime} 与游戏根目录 {game} 互为父子目录")]
    RootsNested { anime: PathBuf, game: PathBuf },

    #[error("设置项无效：{0}")]
    InvalidSetting(String),

    #[error("条目不存在：{0}")]
    ItemNotFound(String),

    #[error("事件主题格式非法：{0}（要求 <domain>:<entity>:<action>，小写字母/数字/连字符）")]
    InvalidTopic(String),

    #[error("条目类型非法：{0}")]
    InvalidKind(String),

    #[error("同步状态非法：{0}")]
    InvalidSyncStatus(String),

    #[error("正则表达式编译失败：{0}")]
    Regex(String),

    #[error("序列化失败：{0}")]
    Serialize(String),

    #[error("文件系统访问失败：{path}（{reason}）")]
    Io { path: PathBuf, reason: String },

    #[error("时间格式处理失败：{0}")]
    Time(String),
}

impl CoreError {
    /// 生成稳定的错误码，供前端做分支判断。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(_) | Self::DatabaseInit(_) => "db_error",
            Self::PoolPoisoned => "db_pool_error",
            Self::PathUnavailable { .. } | Self::NotADirectory(_) | Self::Io { .. } => "fs_error",
            Self::RootsIdentical(_) | Self::RootsNested { .. } => "roots_overlap",
            Self::InvalidSetting(_) => "invalid_setting",
            Self::ItemNotFound(_) => "item_not_found",
            Self::InvalidTopic(_) => "invalid_topic",
            Self::InvalidKind(_) => "invalid_kind",
            Self::InvalidSyncStatus(_) => "invalid_sync_status",
            Self::Regex(_) => "regex_error",
            Self::Serialize(_) | Self::Time(_) => "internal_error",
        }
    }
}
