use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{LauncherError, Result};

/// 条目文件夹内的绑定文件名（ADR-003）。
pub const BINDING_FILE_NAME: &str = ".gamelauncher.json";

fn default_working_dir() -> String {
    ".".to_owned()
}

/// .gamelauncher.json 的结构，字段与 plan.md 5.2 一致。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GameLaunchConfig {
    /// 相对条目文件夹的路径优先；也允许绝对路径（例如游戏安装在另一个盘）。
    pub exe_path: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_working_dir")]
    pub working_dir: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_launched_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_pid: Option<u32>,
}

impl GameLaunchConfig {
    pub fn new(exe_path: impl Into<String>) -> Self {
        Self {
            exe_path: exe_path.into(),
            args: Vec::new(),
            working_dir: default_working_dir(),
            last_launched_at: None,
            last_pid: None,
        }
    }

    pub fn binding_path(folder: &Path) -> PathBuf {
        folder.join(BINDING_FILE_NAME)
    }

    /// 读取绑定文件；不存在返回 None（首次启动的正常状态）。
    pub fn read(folder: &Path) -> Result<Option<Self>> {
        let path = Self::binding_path(folder);
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(LauncherError::BindingIo {
                    path,
                    reason: err.to_string(),
                })
            }
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|err| LauncherError::BindingFormat {
                path,
                reason: err.to_string(),
            })
    }

    /// 原子写入：先写临时文件再改名，避免进程中断留下半个 JSON。
    pub fn write(&self, folder: &Path) -> Result<()> {
        let path = Self::binding_path(folder);
        let tmp = folder.join(format!("{BINDING_FILE_NAME}.tmp"));
        let json =
            serde_json::to_string_pretty(self).map_err(|err| LauncherError::BindingFormat {
                path: path.clone(),
                reason: err.to_string(),
            })?;
        std::fs::write(&tmp, json).map_err(|err| LauncherError::BindingIo {
            path: tmp.clone(),
            reason: err.to_string(),
        })?;
        std::fs::rename(&tmp, &path).map_err(|err| LauncherError::BindingIo {
            path: path.clone(),
            reason: err.to_string(),
        })
    }

    pub fn resolved_exe(&self, folder: &Path) -> PathBuf {
        let candidate = Path::new(&self.exe_path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            folder.join(candidate)
        }
    }

    pub fn resolved_working_dir(&self, folder: &Path) -> PathBuf {
        let candidate = Path::new(&self.working_dir);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            folder.join(candidate)
        }
    }

    /// 校验可执行文件存在，返回解析后的绝对路径。
    pub fn validate(&self, folder: &Path) -> Result<PathBuf> {
        let exe = self.resolved_exe(folder);
        let is_exe = exe
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if !is_exe {
            return Err(LauncherError::BindingFormat {
                path: Self::binding_path(folder),
                reason: format!("exePath 不是 .exe：{}", self.exe_path),
            });
        }
        if !exe.is_file() {
            return Err(LauncherError::ExeNotFound(exe));
        }
        Ok(exe)
    }
}

/// 用户选中的 exe 是否位于条目文件夹内；在内则存相对路径，否则存绝对路径。
pub fn normalize_exe_path(folder: &Path, exe: &Path) -> String {
    let folder_norm = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_path_buf());
    let exe_norm = exe.canonicalize().unwrap_or_else(|_| exe.to_path_buf());
    match exe_norm.strip_prefix(&folder_norm) {
        Ok(relative) => relative.to_string_lossy().to_string(),
        Err(_) => exe_norm.to_string_lossy().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lagm-launcher-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn write_then_read_round_trip() {
        let dir = temp_dir("roundtrip");
        let mut config = GameLaunchConfig::new("bin/Game.exe");
        config.args = vec!["-windowed".into()];
        config.last_pid = Some(4321);
        config.write(&dir).expect("write");

        let loaded = GameLaunchConfig::read(&dir).expect("read").expect("exists");
        assert_eq!(loaded.exe_path, "bin/Game.exe");
        assert_eq!(loaded.args, vec!["-windowed".to_owned()]);
        assert_eq!(loaded.working_dir, ".");
        assert_eq!(loaded.last_pid, Some(4321));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn missing_file_is_none_and_broken_file_is_error() {
        let dir = temp_dir("broken");
        assert!(GameLaunchConfig::read(&dir).expect("read").is_none());

        std::fs::write(GameLaunchConfig::binding_path(&dir), "{ not json").expect("write");
        let err = GameLaunchConfig::read(&dir).expect_err("应当报错");
        assert_eq!(err.code(), "binding_file_error");

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn validate_rejects_missing_exe_and_non_exe() {
        let dir = temp_dir("validate");
        let mut config = GameLaunchConfig::new("Game.exe");
        assert!(matches!(
            config.validate(&dir).expect_err("不存在"),
            LauncherError::ExeNotFound(_)
        ));

        std::fs::write(dir.join("Game.exe"), b"x").expect("exe");
        assert!(config.validate(&dir).is_ok());

        config.exe_path = "readme.txt".to_owned();
        assert!(matches!(
            config.validate(&dir).expect_err("非 exe"),
            LauncherError::BindingFormat { .. }
        ));

        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn normalize_keeps_relative_path_inside_folder() {
        let dir = temp_dir("normalize");
        std::fs::create_dir_all(dir.join("bin")).expect("bin");
        let exe = dir.join("bin").join("Game.exe");
        std::fs::write(&exe, b"x").expect("exe");

        let stored = normalize_exe_path(&dir, &exe);
        assert!(stored.contains("Game.exe"));
        assert!(!stored.starts_with(&*dir.to_string_lossy()), "{stored}");

        let outside = std::env::temp_dir().join("elsewhere.exe");
        let stored_outside = normalize_exe_path(&dir, &outside);
        assert!(Path::new(&stored_outside).is_absolute());

        std::fs::remove_dir_all(dir).ok();
    }
}
