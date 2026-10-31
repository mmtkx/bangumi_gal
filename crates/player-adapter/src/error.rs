use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, PlayerError>;

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("尚未配置外部播放器")]
    NotConfigured,

    #[error("播放器不可用：{path}（{reason}）")]
    InvalidPlayer { path: PathBuf, reason: String },

    #[error("媒体文件不存在：{0}")]
    MediaNotFound(PathBuf),

    #[error("未找到 .{0} 文件类型的系统默认打开方式，请在设置里指定播放器")]
    DefaultHandlerNotFound(String),

    #[error("启动播放器失败：{path}（{reason}）")]
    Spawn { path: PathBuf, reason: String },

    #[error("当前播放器后端不支持 {0}")]
    Unsupported(&'static str),

    #[error("播放会话表不可用")]
    LockPoisoned,

    #[error("进度写入失败：{0}")]
    Core(#[from] library_core::CoreError),
}

impl PlayerError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "player_not_configured",
            Self::InvalidPlayer { .. } => "player_invalid",
            Self::MediaNotFound(_) => "media_not_found",
            Self::DefaultHandlerNotFound(_) => "player_default_handler_not_found",
            Self::Spawn { .. } => "player_spawn_failed",
            Self::Unsupported(_) => "player_unsupported",
            Self::LockPoisoned => "internal_error",
            Self::Core(err) => err.code(),
        }
    }
}
