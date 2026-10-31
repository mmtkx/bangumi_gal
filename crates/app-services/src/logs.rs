//! 日志文件的位置、限额与列表（M5）。
//!
//! 写入器在 `apps/desktop/src-tauri/src/logging.rs`，两边共用这里的路径与限额常量，
//! 避免「界面显示的上限」与「实际滚动阈值」各写一份。

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::settings::APP_NAME;

/// 当前日志文件名（滚动后归档为 `lagm-YYYYMMDD-HHMMSS.log`）。
pub const LOG_FILE_NAME: &str = "lagm.log";
/// 单个文件的大小上限，超过即滚动。
pub const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// 保留的历史归档数量（不含当前文件）。
pub const LOG_KEEP_FILES: usize = 10;

/// 默认日志目录：`%LOCALAPPDATA%\LAGM\logs`，与 ADR-009 的缓存目录同一层。
pub fn default_log_dir() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) => PathBuf::from(base).join(APP_NAME).join("logs"),
        None => std::env::temp_dir().join(APP_NAME).join("logs"),
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogFileDto {
    pub name: String,
    pub size_bytes: u64,
    pub modified_at: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogInfoDto {
    pub dir: String,
    pub current_file: String,
    pub max_bytes: u64,
    pub keep_files: usize,
    pub files: Vec<LogFileDto>,
}

/// 读取日志目录清单；目录不存在时返回空列表而不是报错。
pub fn log_info(dir: &Path) -> LogInfoDto {
    LogInfoDto {
        dir: dir.to_string_lossy().to_string(),
        current_file: dir.join(LOG_FILE_NAME).to_string_lossy().to_string(),
        max_bytes: LOG_MAX_BYTES,
        keep_files: LOG_KEEP_FILES,
        files: list_log_files(dir),
    }
}

pub fn list_log_files(dir: &Path) -> Vec<LogFileDto> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<LogFileDto> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            if !name.starts_with("lagm") || !name.ends_with(".log") {
                return None;
            }
            let metadata = entry.metadata().ok()?;
            let modified = metadata
                .modified()
                .ok()
                .map(|time| {
                    chrono::DateTime::<chrono::Local>::from(time)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default();
            Some(LogFileDto {
                name,
                size_bytes: metadata.len(),
                modified_at: modified,
            })
        })
        .collect();
    // 当前文件排在前面，其余按修改时间从新到旧。
    files.sort_by(|left, right| {
        left.name
            .ne(LOG_FILE_NAME)
            .cmp(&right.name.ne(LOG_FILE_NAME))
            .then_with(|| right.modified_at.cmp(&left.modified_at))
    });
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_lagm_logs_newest_first() {
        let dir = std::env::temp_dir().join(format!("lagm-logs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        std::fs::write(dir.join(LOG_FILE_NAME), b"current").expect("current");
        std::fs::write(dir.join("lagm-20261030-120000.log"), b"archive").expect("archive");
        std::fs::write(dir.join("other.txt"), b"ignored").expect("other");

        let info = log_info(&dir);
        assert_eq!(info.files.len(), 2);
        assert_eq!(info.files[0].name, LOG_FILE_NAME);
        assert_eq!(info.files[1].name, "lagm-20261030-120000.log");
        assert!(info.current_file.ends_with(LOG_FILE_NAME));
        assert_eq!(info.max_bytes, LOG_MAX_BYTES);
        assert_eq!(info.keep_files, LOG_KEEP_FILES);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_dir_yields_empty_list() {
        let dir = std::env::temp_dir().join("lagm-logs-does-not-exist");
        let info = log_info(&dir);
        assert!(info.files.is_empty());
        assert!(info.dir.contains("lagm-logs-does-not-exist"));
    }
}
