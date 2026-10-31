use std::path::Path;

use library_core::{CoreError, Result};
use regex::Regex;

/// 单次探测最多返回的文件数。
pub const MAX_PROBE_FILES: usize = 500;

const SKIP_DIR_NAMES: &[&str] = &["$recycle.bin", "system volume information", "node_modules"];

pub struct FileInfo {
    pub name: String,
    pub size_bytes: u64,
}

/// 安装器、卸载器、运行库安装程序都不代表「这是游戏」，命中即不作为可启动候选。
fn noise_pattern() -> Result<Regex> {
    Regex::new(
        r"(?i)^(?:unins\d*|uninstall[^\\/]*|uninst|setup|install|vcredist[^\\/]*|dxsetup|dxwebsetup|oalinst|ue4prereqsetup[^\\/]*|dotnet[^\\/]*|crashreport[^\\/]*|unitycrashhandler[^\\/]*)\.exe$",
    )
    .map_err(|err| CoreError::Regex(err.to_string()))
}

pub fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with('.')
        || SKIP_DIR_NAMES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

fn is_exe(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

/// 收集目录的直接子可执行文件（不递归）。
///
/// 递归交给扫描主循环：向下找到最近的含 exe 目录，那里才是条目根。
pub fn collect_direct_exes(dir: &Path, noise: &Regex) -> Result<Vec<FileInfo>> {
    let entries = std::fs::read_dir(dir).map_err(|err| CoreError::Io {
        path: dir.to_path_buf(),
        reason: err.to_string(),
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "跳过无法读取的目录项");
                continue;
            }
        };
        let path = entry.path();
        if !is_exe(&path) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if noise.is_match(&name) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        files.push(FileInfo {
            name,
            size_bytes: metadata.len(),
        });
    }

    files.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(files)
}

/// 游戏扫描器持有的可执行文件过滤规则，构造一次后复用。
pub struct ExeFilter {
    noise: Regex,
}

impl ExeFilter {
    pub fn new() -> Result<Self> {
        Ok(Self {
            noise: noise_pattern()?,
        })
    }

    pub fn collect(&self, dir: &Path) -> Result<Vec<FileInfo>> {
        collect_direct_exes(dir, &self.noise)
    }

    /// 深度受限的递归收集：条目目录本身不含 exe 时（例如 exe 在更深的子目录）使用。
    pub fn collect_nested(&self, dir: &Path, max_depth: usize) -> Result<Vec<FileInfo>> {
        if !dir.is_dir() {
            return Err(CoreError::NotADirectory(dir.to_path_buf()));
        }
        let mut files = Vec::new();
        for entry in walkdir::WalkDir::new(dir)
            .min_depth(1)
            .max_depth(max_depth)
            .follow_links(false)
        {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(error = %err, "跳过无法访问的目录");
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if !is_exe(path) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if self.noise.is_match(&name) {
                continue;
            }
            let Ok(metadata) = entry.metadata() else {
                continue;
            };
            files.push(FileInfo {
                name,
                size_bytes: metadata.len(),
            });
        }
        files.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lagm-game-media-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create");
        dir
    }

    #[test]
    fn collects_exe_and_ignores_noise() {
        let dir = temp_dir("exe");
        fs::write(dir.join("Game.exe"), b"x").expect("game");
        fs::write(dir.join("unins000.exe"), b"x").expect("unins");
        fs::write(dir.join("vcredist_x64.exe"), b"x").expect("vcredist");
        fs::write(dir.join("readme.txt"), b"x").expect("txt");

        let filter = ExeFilter::new().expect("filter");
        let files = filter.collect(&dir).expect("collect");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "Game.exe");

        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn exe_extension_is_case_insensitive() {
        let dir = temp_dir("case");
        fs::write(dir.join("GAME.EXE"), b"x").expect("write");
        let filter = ExeFilter::new().expect("filter");
        assert_eq!(filter.collect(&dir).expect("collect").len(), 1);
        fs::remove_dir_all(dir).ok();
    }
}
