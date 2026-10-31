//! 「系统默认播放器」后端。
//!
//! 用户没有指定播放器时，按文件扩展名查注册表里的默认关联程序，
//! 再**直接启动那个 exe**，而不是走 `start` / `explorer`。
//!
//! 这么做是为了拿到真实播放器进程的 pid：`cmd /C start` 这类转发进程会立刻退出，
//! 会话会被判定为「已结束」，进度就永远是 0。只有拿到播放器自己的 pid，
//! 才能像外部播放器那样按进程存活判断播放结束。

use std::path::{Path, PathBuf};
use std::process::Command;

use tokio::sync::broadcast;

use crate::backend::{PlayerBackend, PlayerState};
use crate::error::{PlayerError, Result};
use crate::external::split_quoted;

/// Windows：让播放器独立于宿主进程组。
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 系统默认播放器后端：`pause` / `seek` 不支持，进度由会话时长估算。
pub struct SystemDefaultPlayer {
    states: broadcast::Sender<PlayerState>,
    /// 上一次 play 是否成功续播（识别到支持跳转的播放器才为真）。
    last_resume: std::sync::atomic::AtomicBool,
}

impl SystemDefaultPlayer {
    pub fn new() -> Self {
        let (states, _) = broadcast::channel(16);
        Self {
            states,
            last_resume: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl Default for SystemDefaultPlayer {
    fn default() -> Self {
        Self::new()
    }
}

impl PlayerBackend for SystemDefaultPlayer {
    fn name(&self) -> &'static str {
        "system-default"
    }

    fn play(&self, uri: &Path, start_seconds: u64) -> Result<u32> {
        if !uri.is_file() {
            return Err(PlayerError::MediaNotFound(uri.to_path_buf()));
        }
        let (exe, registry_args) = resolve_launch_command(uri)?;
        if !exe.is_file() {
            return Err(PlayerError::InvalidPlayer {
                path: exe,
                reason: "关联程序不存在".to_owned(),
            });
        }

        // 认得出是哪个播放器就自己拼续播参数（PotPlayer/VLC/mpv/MPC 都支持），
        // 认不出来只能用注册表给的命令行：那种情况下无法跳转，续播如实标记为未生效。
        let file = uri.to_string_lossy().to_string();
        let (args, resume) = match known_player_args(&exe, &file, start_seconds) {
            Some(args) => (args, start_seconds > 0),
            None => (registry_args, false),
        };
        self.last_resume
            .store(resume, std::sync::atomic::Ordering::Relaxed);

        let mut command = Command::new(&exe);
        command.args(&args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(CREATE_NEW_PROCESS_GROUP);
        }
        let child = command.spawn().map_err(|err| PlayerError::Spawn {
            path: exe.clone(),
            reason: err.to_string(),
        })?;
        Ok(child.id())
    }

    fn pause(&self) -> Result<()> {
        Err(PlayerError::Unsupported("暂停"))
    }

    fn seek(&self, _seconds: u64) -> Result<()> {
        Err(PlayerError::Unsupported("跳转"))
    }

    fn resume_applied(&self) -> bool {
        self.last_resume.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn get_position(&self, _pid: u32) -> Result<Option<u64>> {
        // 没有回传通道，进度按「续播起点 + 本次时长」估算。
        Ok(None)
    }

    fn on_state_change(&self) -> broadcast::Receiver<PlayerState> {
        self.states.subscribe()
    }
}

/// 解析出「用哪个 exe、带什么参数」来打开这个文件。
///
/// Windows：`HKCU\...\FileExts\.ext\UserChoice` → ProgId →
/// `HKCR\<ProgId>\shell\open\command`。
/// 解析不出来时直接报错：宁可提示去设置里指定播放器，也不要静默用 `start`
/// 让会话立刻结束、进度写成 0。
fn resolve_launch_command(uri: &Path) -> Result<(PathBuf, Vec<String>)> {
    let file = uri.to_string_lossy().to_string();
    let ext = uri
        .extension()
        .map(|value| value.to_string_lossy().to_lowercase())
        .ok_or_else(|| PlayerError::DefaultHandlerNotFound(file.clone()))?;

    #[cfg(windows)]
    {
        let command = windows::open_command_for(&ext)
            .ok_or_else(|| PlayerError::DefaultHandlerNotFound(format!(".{ext}")))?;
        let mut tokens = split_quoted(&command);
        if tokens.is_empty() {
            return Err(PlayerError::DefaultHandlerNotFound(format!(".{ext}")));
        }
        let exe = PathBuf::from(tokens.remove(0));
        let args = fill_placeholders(tokens, &file);
        Ok((exe, args))
    }

    #[cfg(not(windows))]
    {
        let _ = file;
        Err(PlayerError::DefaultHandlerNotFound(format!(".{ext}")))
    }
}

/// 把 `%1` / `%L` 之类的占位符换成真实路径；没有占位符时把路径追加到最后。
fn fill_placeholders(tokens: Vec<String>, file: &str) -> Vec<String> {
    let has_placeholder = tokens.iter().any(|token| is_placeholder(token));
    let mut args: Vec<String> = tokens
        .into_iter()
        .map(|token| {
            if is_placeholder(&token) {
                file.to_owned()
            } else {
                token
                    .replace("%1", file)
                    .replace("%L", file)
                    .replace("%l", file)
            }
        })
        .collect();
    if !has_placeholder {
        args.push(file.to_owned());
    }
    args
}

/// 已知播放器的启动参数：返回 None 表示不认识，只能按注册表命令直接打开（无法跳转）。
///
/// 常见播放器的跳转语法各不相同，写错会直接报错或忽略参数，因此只覆盖确定支持的几种。
fn known_player_args(exe: &Path, file: &str, seconds: u64) -> Option<Vec<String>> {
    let name = exe.file_name()?.to_string_lossy().to_lowercase();

    if name.contains("potplayer") {
        // `/new` 必须在文件前面，否则 `/seek` 会被忽略（同设置页 PotPlayer 预设）。
        let mut args = vec!["/new".to_owned(), file.to_owned()];
        if seconds > 0 {
            args.push(format!("/seek={seconds}"));
        }
        return Some(args);
    }

    if name == "vlc.exe" {
        let mut args = Vec::new();
        if seconds > 0 {
            args.push(format!("--start-time={seconds}"));
        }
        args.push(file.to_owned());
        return Some(args);
    }

    if name == "mpv.exe" {
        let mut args = Vec::new();
        if seconds > 0 {
            args.push(format!("--start={seconds}"));
        }
        args.push(file.to_owned());
        return Some(args);
    }

    // MPC-HC / MPC-BE 的 /start 单位是毫秒。
    if name.starts_with("mpc-hc") || name.starts_with("mpc-be") {
        let mut args = vec![file.to_owned()];
        if seconds > 0 {
            args.push("/start".to_owned());
            args.push((seconds.saturating_mul(1000)).to_string());
        }
        return Some(args);
    }

    None
}

fn is_placeholder(token: &str) -> bool {
    matches!(token, "%1" | "%L" | "%l" | "\"%1\"")
}

#[cfg(windows)]
mod windows {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    /// 查扩展名的默认打开命令（未展开的原始字符串）。
    pub fn open_command_for(ext: &str) -> Option<String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let user_choice = hkcu
            .open_subkey(format!(
                r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\.{ext}\UserChoice"
            ))
            .ok()
            .and_then(|key| key.get_value::<String, _>("ProgId").ok());

        let prog_id = user_choice.or_else(|| {
            // 没有 UserChoice 时退回系统级关联。
            let classes = RegKey::predef(winreg::enums::HKEY_CLASSES_ROOT);
            classes
                .open_subkey(format!(".{ext}"))
                .ok()
                .and_then(|key| key.get_value::<String, _>("").ok())
        })?;

        let classes = RegKey::predef(winreg::enums::HKEY_CLASSES_ROOT);
        classes
            .open_subkey(format!(r"{prog_id}\shell\open\command"))
            .ok()
            .and_then(|key| key.get_value::<String, _>("").ok())
            .filter(|command| !command.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_players_get_seek_arguments() {
        // PotPlayer：`/new` 必须在文件前，否则 /seek 被忽略。
        assert_eq!(
            known_player_args(Path::new(r"C://P//PotPlayerMini64.exe"), "a.mkv", 600).unwrap(),
            vec![
                "/new".to_string(),
                "a.mkv".to_string(),
                "/seek=600".to_string()
            ]
        );
        // 起点为 0 时不带 seek 参数。
        assert_eq!(
            known_player_args(Path::new("PotPlayerMini64.exe"), "a.mkv", 0).unwrap(),
            vec!["/new".to_string(), "a.mkv".to_string()]
        );
        // VLC / mpv / MPC-BE 的语法各不相同，逐个别写错。
        assert_eq!(
            known_player_args(Path::new("vlc.exe"), "a.mkv", 30).unwrap(),
            vec!["--start-time=30".to_string(), "a.mkv".to_string()]
        );
        assert_eq!(
            known_player_args(Path::new("mpv.exe"), "a.mkv", 30).unwrap(),
            vec!["--start=30".to_string(), "a.mkv".to_string()]
        );
        // MPC 的 /start 单位是毫秒。
        assert_eq!(
            known_player_args(Path::new("mpc-be64.exe"), "a.mkv", 2).unwrap(),
            vec![
                "a.mkv".to_string(),
                "/start".to_string(),
                "2000".to_string()
            ]
        );
        // 不认识的播放器不能瞎拼参数。
        assert_eq!(
            known_player_args(Path::new("unknown.exe"), "a.mkv", 30),
            None
        );
    }

    #[test]
    fn placeholders_are_replaced_or_appended() {
        // `"%1"` 形态（注册表里最常见）。
        let args = fill_placeholders(vec!["%1".into()], r"C:\a b\1.mkv");
        assert_eq!(args, vec![r"C:\a b\1.mkv".to_string()]);

        // 带前缀参数的形态：`--play "%L"`。
        let args = fill_placeholders(vec!["--play".into(), "%L".into()], "x.mkv");
        assert_eq!(args, vec!["--play".to_string(), "x.mkv".to_string()]);

        // 没有占位符时补到末尾，否则播放器收不到文件。
        let args = fill_placeholders(vec!["--fullscreen".into()], "x.mkv");
        assert_eq!(args, vec!["--fullscreen".to_string(), "x.mkv".to_string()]);
    }
}
