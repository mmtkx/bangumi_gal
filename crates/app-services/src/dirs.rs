use std::path::PathBuf;

use library_core::CoreError;

use crate::dto::{DirEntryDto, DirListingDto, FileEntryDto};
use crate::error::{ApiResult, AppError};

/// 列出目录下的子目录，供设置页选择根目录。
///
/// 前端禁止直接访问文件系统（plan.md 3.2），因此目录浏览也经由后端命令完成。
pub fn list_subdirs(raw: Option<&str>) -> ApiResult<DirListingDto> {
    let target = raw
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(PathBuf::from);

    let Some(path) = target else {
        return Ok(list_drives());
    };

    library_core::paths::ensure_directory(&path)?;

    let read = std::fs::read_dir(&path).map_err(|err| {
        AppError::Core(CoreError::Io {
            path: path.clone(),
            reason: err.to_string(),
        })
    })?;

    let mut entries = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %path.display(), error = %err, "跳过无法读取的目录项");
                continue;
            }
        };
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        entries.push(DirEntryDto {
            name: entry.file_name().to_string_lossy().to_string(),
            path: entry.path().to_string_lossy().to_string(),
        });
    }
    entries.sort_by_key(|entry| entry.name.to_lowercase());

    Ok(DirListingDto {
        path: Some(path.to_string_lossy().to_string()),
        parent: path
            .parent()
            .map(|parent| parent.to_string_lossy().to_string()),
        entries,
    })
}

/// 列出目录下的 .exe 文件，供「选择可执行文件」弹窗使用。
pub fn list_exe_files(raw: &str) -> ApiResult<Vec<FileEntryDto>> {
    let path = PathBuf::from(raw.trim());
    library_core::paths::ensure_directory(&path)?;

    let read = std::fs::read_dir(&path).map_err(|err| {
        AppError::Core(CoreError::Io {
            path: path.clone(),
            reason: err.to_string(),
        })
    })?;

    let mut files = Vec::new();
    for entry in read {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %path.display(), error = %err, "跳过无法读取的目录项");
                continue;
            }
        };
        let file_path = entry.path();
        let is_exe = file_path
            .extension()
            .and_then(|ext| ext.to_str())
            .map(|ext| ext.eq_ignore_ascii_case("exe"))
            .unwrap_or(false);
        if !is_exe {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_file() {
            continue;
        }
        files.push(FileEntryDto {
            name: entry.file_name().to_string_lossy().to_string(),
            path: file_path.to_string_lossy().to_string(),
            size_bytes: metadata.len(),
        });
    }
    files.sort_by_key(|entry| entry.name.to_lowercase());
    Ok(files)
}

fn list_drives() -> DirListingDto {
    let mut entries = Vec::new();
    for letter in b'A'..=b'Z' {
        let root = format!("{}:\\", letter as char);
        if std::path::Path::new(&root).is_dir() {
            entries.push(DirEntryDto {
                name: root.clone(),
                path: root,
            });
        }
    }
    DirListingDto {
        path: None,
        parent: None,
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_subdirectories_sorted() {
        let base = std::env::temp_dir().join(format!(
            "lagm-dirs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        std::fs::create_dir_all(base.join("zeta")).expect("zeta");
        std::fs::create_dir_all(base.join("Alpha")).expect("alpha");
        std::fs::write(base.join("file.txt"), b"x").expect("file");

        let listing = list_subdirs(Some(&base.to_string_lossy())).expect("listing");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "zeta"]);
        assert!(listing.parent.is_some());

        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn missing_path_reports_error() {
        let missing = std::env::temp_dir().join("lagm-dirs-missing-does-not-exist");
        assert!(list_subdirs(Some(&missing.to_string_lossy())).is_err());
    }
}
