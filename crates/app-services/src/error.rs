//! 应用层错误。
//!
//! 库层错误用 thiserror 各自定义，到应用层统一收敛为 `AppError`，
//! 并以 `{ code, message }` 序列化给前端（message 面向用户可读）。

use library_core::CoreError;
use serde::ser::SerializeStruct;
use serde::{Serialize, Serializer};
use thiserror::Error;

pub type ApiResult<T> = std::result::Result<T, AppError>;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Core(#[from] CoreError),

    #[error(transparent)]
    Launcher(#[from] launcher::LauncherError),

    #[error(transparent)]
    Player(#[from] player_adapter::PlayerError),

    #[error(transparent)]
    Sync(#[from] bangumi_sync::SyncError),

    #[error(transparent)]
    Plugin(#[from] plugin_host::PluginError),

    #[error("后台任务执行失败：{0}")]
    Join(String),

    #[error("参数校验失败：{0}")]
    Validation(String),

    #[error("扫描失败：{0}")]
    Scan(String),

    #[error("内部错误：{0}")]
    Internal(String),
}

impl AppError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Core(err) => err.code(),
            Self::Launcher(err) => err.code(),
            Self::Player(err) => err.code(),
            Self::Sync(err) => err.code(),
            Self::Plugin(_) => "plugin_error",
            Self::Join(_) => "join_error",
            Self::Validation(_) => "validation_error",
            Self::Scan(_) => "scan_error",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("AppError", 2)?;
        state.serialize_field("code", self.code())?;
        state.serialize_field("message", &self.to_string())?;
        state.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_code_and_message() {
        let error = AppError::Validation("动画根目录不能为空".into());
        let value = serde_json::to_value(&error).expect("serialize");
        assert_eq!(value["code"], "validation_error");
        assert_eq!(value["message"], "参数校验失败：动画根目录不能为空");
    }

    #[test]
    fn core_error_code_is_forwarded() {
        let error = AppError::Core(CoreError::ItemNotFound("x".into()));
        assert_eq!(error.code(), "item_not_found");
    }
}
