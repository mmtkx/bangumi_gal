use std::path::PathBuf;

use thiserror::Error;

pub type Result<T> = std::result::Result<T, LauncherError>;

#[derive(Debug, Error)]
pub enum LauncherError {
    #[error("绑定文件读写失败：{path}（{reason}）")]
    BindingIo { path: PathBuf, reason: String },

    #[error("绑定文件格式非法：{path}（{reason}）")]
    BindingFormat { path: PathBuf, reason: String },

    #[error("该游戏尚未绑定可执行文件")]
    BindingMissing,

    #[error("可执行文件不存在：{0}")]
    ExeNotFound(PathBuf),

    #[error("启动进程失败：{path}（{reason}）")]
    Spawn { path: PathBuf, reason: String },

    #[error("该条目已在运行（pid {0}）")]
    AlreadyRunning(u32),

    #[error("条目 {0} 不是游戏类型，不能启动")]
    NotAGame(String),

    #[error("运行状态表不可用")]
    LockPoisoned,

    #[error("数据库操作失败：{0}")]
    Core(#[from] library_core::CoreError),
}

impl LauncherError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::BindingIo { .. } | Self::BindingFormat { .. } => "binding_file_error",
            Self::BindingMissing => "binding_missing",
            Self::ExeNotFound(_) => "exe_not_found",
            Self::Spawn { .. } => "spawn_failed",
            Self::AlreadyRunning(_) => "already_running",
            Self::NotAGame(_) => "not_a_game",
            Self::LockPoisoned => "internal_error",
            Self::Core(err) => err.code(),
        }
    }
}
