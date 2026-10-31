use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::sync::broadcast;

use crate::backend::{PlayerBackend, PlayerState};
use crate::error::{PlayerError, Result};

/// Windows：让播放器独立于宿主进程组，避免被宿主控制台事件带走。
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 参数模板的占位符。`{file}` 必填（缺失时自动把文件路径追加为最后一个参数）。
pub const FILE_PLACEHOLDER: &str = "{file}";
/// 断点续播秒数占位符；模板里没有它时播放器会从头开始。
pub const SECONDS_PLACEHOLDER: &str = "{seconds}";

/// 把参数模板拆成 argv，支持双引号包裹含空格的片段。
///
/// 例：`"{file}" /seek={seconds}` → [文件路径（单个参数）, `/seek=600`]
pub fn build_arguments(template: &str, file: &Path, start_seconds: u64) -> Vec<String> {
    let file_text = file.to_string_lossy().to_string();
    let seconds_text = start_seconds.to_string();

    let mut args: Vec<String> = split_quoted(template)
        .into_iter()
        .map(|token| {
            token
                .replace(FILE_PLACEHOLDER, &file_text)
                .replace(SECONDS_PLACEHOLDER, &seconds_text)
        })
        .collect();

    if !template.contains(FILE_PLACEHOLDER) {
        args.push(file_text);
    }
    args
}

pub(crate) fn split_quoted(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for ch in input.chars() {
        match ch {
            '"' => in_quotes = !in_quotes,
            ' ' | '\t' if !in_quotes => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// 外部播放器后端：把媒体文件交给用户配置的播放器可执行文件。
///
/// 外部播放器不提供回传通道，因此：
/// - `get_position` 返回 None，进度 = 续播起点 + 本次会话时长
/// - `pause` / `seek` 明确返回不支持，而不是静默忽略
/// - 断点续播依赖参数模板里的 `{seconds}`（各播放器语法不同，见设置页预设）
pub struct ExternalPlayerAdapter {
    exe: PathBuf,
    args_template: String,
    states: broadcast::Sender<PlayerState>,
    /// 上一次 play 是否成功续播，供会话与前端判断。
    last_resume: std::sync::atomic::AtomicBool,
}

impl ExternalPlayerAdapter {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        let (states, _) = broadcast::channel(16);
        Self {
            exe: exe.into(),
            args_template: FILE_PLACEHOLDER.to_owned(),
            states,
            last_resume: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// 设置启动参数模板，例如 PotPlayer 的 `"{file}" /seek={seconds} /new`。
    pub fn with_arguments(mut self, template: &str) -> Self {
        self.args_template = if template.trim().is_empty() {
            FILE_PLACEHOLDER.to_owned()
        } else {
            template.to_owned()
        };
        self
    }

    pub fn executable(&self) -> &Path {
        &self.exe
    }

    pub fn arguments_template(&self) -> &str {
        &self.args_template
    }
}

impl PlayerBackend for ExternalPlayerAdapter {
    fn name(&self) -> &'static str {
        "external"
    }

    fn play(&self, uri: &Path, start_seconds: u64) -> Result<u32> {
        if !uri.is_file() {
            return Err(PlayerError::MediaNotFound(uri.to_path_buf()));
        }
        if !self.exe.is_file() {
            return Err(PlayerError::InvalidPlayer {
                path: self.exe.clone(),
                reason: "播放器文件不存在".to_owned(),
            });
        }

        // 模板里没有 {seconds} 时播放器根本不跳转，续播就是假的。
        self.last_resume.store(
            start_seconds > 0 && self.args_template.contains(SECONDS_PLACEHOLDER),
            std::sync::atomic::Ordering::Relaxed,
        );

        let mut command = Command::new(&self.exe);
        command.args(build_arguments(&self.args_template, uri, start_seconds));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }

        let child = command.spawn().map_err(|err| PlayerError::Spawn {
            path: self.exe.clone(),
            reason: err.to_string(),
        })?;

        // 只有启动状态是外部播放器能确认的；结束由会话监控经事件总线广播。
        let _ = self.states.send(PlayerState::Playing);
        Ok(child.id())
    }

    fn pause(&self) -> Result<()> {
        Err(PlayerError::Unsupported("pause"))
    }

    fn seek(&self, _seconds: u64) -> Result<()> {
        Err(PlayerError::Unsupported("seek"))
    }

    fn resume_applied(&self) -> bool {
        self.last_resume.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn get_position(&self, pid: u32) -> Result<Option<u64>> {
        // PotPlayer 通过窗口消息上报真实位置（含拖动进度条后的位置）。
        #[cfg(windows)]
        if crate::potplayer::is_potplayer(&self.exe) {
            return Ok(crate::potplayer::query(pid).map(|state| state.position_ms / 1000));
        }

        // 其他播放器没有回传通道，交由会话按运行时长估算。
        let _ = pid;
        Ok(None)
    }

    fn on_state_change(&self) -> broadcast::Receiver<PlayerState> {
        self.states.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_potplayer_arguments_with_seek() {
        // /new 必须在文件前面，否则 PotPlayer 会忽略 /seek（实测）。
        let args = build_arguments(
            "/new \"{file}\" /seek={seconds}",
            Path::new("D:/anime/Show - 01.mkv"),
            754,
        );
        assert_eq!(
            args,
            vec![
                "/new".to_owned(),
                "D:/anime/Show - 01.mkv".to_owned(),
                "/seek=754".to_owned()
            ]
        );
    }

    #[test]
    fn builds_mpv_style_arguments() {
        let args = build_arguments("--start={seconds} \"{file}\"", Path::new("D:/a/b.mkv"), 0);
        assert_eq!(args, vec!["--start=0".to_owned(), "D:/a/b.mkv".to_owned()]);
    }

    #[test]
    fn appends_file_when_placeholder_missing() {
        let args = build_arguments("/seek={seconds}", Path::new("D:/a/b.mkv"), 30);
        assert_eq!(args, vec!["/seek=30".to_owned(), "D:/a/b.mkv".to_owned()]);
    }

    #[test]
    fn empty_template_falls_back_to_file_only() {
        let adapter = ExternalPlayerAdapter::new("C:\\x\\player.exe").with_arguments("   ");
        assert_eq!(adapter.arguments_template(), FILE_PLACEHOLDER);
    }

    #[test]
    fn rejects_missing_media_before_spawning() {
        let adapter = ExternalPlayerAdapter::new("C:\\Windows\\System32\\cmd.exe");
        let err = adapter
            .play(Path::new("D:/nope/missing.mkv"), 0)
            .expect_err("应当拒绝");
        assert_eq!(err.code(), "media_not_found");
    }

    #[test]
    fn rejects_missing_player_executable() {
        let adapter = ExternalPlayerAdapter::new("D:/nope/player.exe");
        let media = std::env::temp_dir().join(format!("lagm-media-{}.mkv", std::process::id()));
        std::fs::write(&media, b"x").expect("media");
        let err = adapter.play(&media, 0).expect_err("应当拒绝");
        assert_eq!(err.code(), "player_invalid");
        std::fs::remove_file(media).ok();
    }

    #[test]
    fn pause_and_seek_report_unsupported() {
        let adapter = ExternalPlayerAdapter::new("C:\\Windows\\System32\\cmd.exe");
        assert_eq!(
            adapter.pause().expect_err("pause").code(),
            "player_unsupported"
        );
        assert_eq!(
            adapter.seek(10).expect_err("seek").code(),
            "player_unsupported"
        );
        assert!(adapter.get_position(1).expect("position").is_none());
    }

    #[cfg(windows)]
    #[test]
    fn play_spawns_process_and_emits_playing_state() {
        let media = std::env::temp_dir().join(format!("lagm-play-{}.mkv", std::process::id()));
        std::fs::write(&media, b"x").expect("media");
        // 用 where.exe 充当播放器：接受任意参数后立即退出，不留后台进程。
        let adapter = ExternalPlayerAdapter::new("C:\\Windows\\System32\\where.exe");
        let mut states = adapter.on_state_change();
        let pid = adapter.play(&media, 0).expect("spawn");
        assert!(pid > 0);
        let state = states.try_recv().expect("state");
        assert_eq!(state, PlayerState::Playing);
        std::fs::remove_file(media).ok();
    }
}
