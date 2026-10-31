//! 播放器后端抽象（ADR-002）、外部播放器适配与播放会话进度估算。
//!
//! 依赖规则：只依赖 library-core（ProgressRepository / EventPublisher / 事件类型）。
//! 上层（app-services）只通过 `Player` 与 `PlayerBackend` 使用本 crate，
//! 未来内置播放器新增 `EmbeddedMpvAdapter` 即可替换后端，不改上层。

mod backend;
mod error;
mod external;
/// PotPlayer 播放状态读取（Windows 专用）。公开以便诊断与集成测试。
#[cfg(windows)]
pub mod potplayer;
mod session;
mod system;

pub use backend::{ActiveSession, PlayerBackend, PlayerState, PlayingSession};
pub use error::{PlayerError, Result};
pub use external::{build_arguments, ExternalPlayerAdapter, FILE_PLACEHOLDER, SECONDS_PLACEHOLDER};
#[cfg(windows)]
pub use potplayer::{is_potplayer, PlaybackState};
pub use session::Player;
pub use system::SystemDefaultPlayer;
