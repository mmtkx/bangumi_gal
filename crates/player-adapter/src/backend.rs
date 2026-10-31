use std::path::Path;

use serde::Serialize;
use tokio::sync::broadcast;

use crate::error::Result;

/// 播放器状态。供未来内置播放器通过 `on_state_change` 推送，外部播放器只有启动事件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayerState {
    Idle,
    Playing,
    Stopped,
}

/// 播放后端抽象（ADR-002）。
///
/// 上层只依赖本 trait：首版是 `ExternalPlayerAdapter`，未来内置播放器新增实现即可，
/// 不改动 app-services 与前端。
pub trait PlayerBackend: Send + Sync {
    fn name(&self) -> &'static str;

    /// 打开媒体文件，返回播放器进程 pid。
    ///
    /// `start_seconds` 为断点续播位置：外部播放器靠启动参数定位，内置播放器可直接 seek。
    fn play(&self, uri: &Path, start_seconds: u64) -> Result<u32>;

    fn pause(&self) -> Result<()>;

    fn seek(&self, seconds: u64) -> Result<()>;

    /// 精确播放位置。外部播放器无法回传，返回 None（进度改由会话时长估算）。
    fn get_position(&self, pid: u32) -> Result<Option<u64>>;

    fn on_state_change(&self) -> broadcast::Receiver<PlayerState>;

    /// 上一次 `play` 是否**真的**从 `start_seconds` 开始播放。
    ///
    /// 传了起点不等于续播成功：外部播放器要求参数模板里含 `{seconds}`，
    /// 系统默认播放器还要看能否识别出支持跳转的程序。前端据此决定提示语，
    /// 避免用户以为「从 12:30 继续」而实际从头播了。
    fn resume_applied(&self) -> bool {
        false
    }
}

/// 一次已启动的播放会话。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayingSession {
    pub item_id: String,
    pub episode: u32,
    pub file_name: String,
    pub uri: String,
    pub pid: u32,
    pub started_at: String,
    /// 本次会话的起始位置（秒），0 表示从头播放。
    pub start_seconds: u64,
    /// 起点是否真的生效（false 表示播放器不支持跳转，实际从头播放）。
    pub resume_applied: bool,
}

/// 运行中的会话快照，供前端在页面加载时恢复状态。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveSession {
    pub item_id: String,
    pub episode: u32,
    pub file_name: String,
    pub pid: u32,
    pub started_at: String,
    /// 当前播放位置（秒）：能读到播放器上报就取真实值，否则为估算值。
    pub position_seconds: u64,
    pub start_seconds: u64,
}
