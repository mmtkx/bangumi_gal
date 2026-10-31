use std::path::Path;

use library_core::{CoreError, Result};

/// plan.md 未限定视频容器，覆盖常见格式即可；不认识的文件一律忽略。
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "rmvb", "rm", "mov", "wmv", "flv", "ts", "m2ts", "webm", "m4v", "mpg",
    "mpeg", "ogv",
];

/// 单次探测最多返回的文件数，避免超大合集把 IPC 负载撑爆。
pub const MAX_PROBE_FILES: usize = 500;

const SKIP_DIR_NAMES: &[&str] = &["$recycle.bin", "system volume information", "node_modules"];

/// 附属内容目录：里面的视频属于所属番剧，不应单独成为条目。
///
/// 名称比较前会去掉结尾的数字与分隔符，所以 `SPs`、`SP 1`、`SP-01` 视为同一类。
const CONTAINER_DIR_NAMES: &[&str] = &[
    "sp",
    "sps",
    "special",
    "specials",
    "extra",
    "extras",
    "bonus",
    "ova",
    "oad",
    "ncop",
    "nced",
    "op",
    "ed",
    "pv",
    "cm",
    "menu",
    "scan",
    "scans",
    "cd",
    "cds",
    "sub",
    "subs",
    "subtitle",
    "subtitles",
    "font",
    "fonts",
    "映像特典",
    "特典",
    "字幕",
    "扫图",
    "音乐",
    "ost",
    "原声",
];

/// 相对条目目录的文件（`relative` 用 `/` 分隔，跨平台展示一致）。
pub struct RelativeFile {
    pub relative: String,
    pub size_bytes: u64,
}

pub fn is_video_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            VIDEO_EXTENSIONS
                .iter()
                .any(|candidate| ext.eq_ignore_ascii_case(candidate))
        })
        .unwrap_or(false)
}

fn normalize_dir_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .trim_end_matches(|c: char| {
            c.is_ascii_digit() || c == ' ' || c == '.' || c == '_' || c == '-'
        })
        .to_string()
}

/// 归一化成分组键：小写、只保留字母数字与假名/汉字，其余一律丢弃。
/// 用于把 `Kaguya-sama wa Kokurasetai` 与 `Kaguya_sama wa Kokurasetai` 视作同一组，
/// 也用于判断某个文件是否属于某个松散分组。
pub fn normalize_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        }
    }
    out
}

/// 列出目录直属的视频文件名主干（不递归）。
///
/// 根目录松散文件分组用：子目录里的内容已经由目录型条目覆盖，这里只看一层。
pub fn direct_video_stems(dir: &Path) -> Result<Vec<String>> {
    let entries = std::fs::read_dir(dir).map_err(|err| CoreError::Io {
        path: dir.to_path_buf(),
        reason: err.to_string(),
    })?;

    let mut stems = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || !is_video_file(&path) {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            stems.push(stem.to_string());
        }
    }
    Ok(stems)
}

/// 是否是附属内容目录（SPs / 特典 / NCOP 等）。
pub fn is_container_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let normalized = normalize_dir_name(name);
    CONTAINER_DIR_NAMES
        .iter()
        .any(|candidate| *candidate == normalized)
}

/// 跳过隐藏目录与系统目录，避免扫描回收站、卷信息目录。
pub fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with('.')
        || SKIP_DIR_NAMES
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// 目录是否直接包含视频文件（只看一层）。
pub fn has_direct_videos(dir: &Path) -> Result<bool> {
    let entries = std::fs::read_dir(dir).map_err(|err| CoreError::Io {
        path: dir.to_path_buf(),
        reason: err.to_string(),
    })?;

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "跳过无法读取的目录项");
                continue;
            }
        };
        if is_video_file(&entry.path()) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 递归收集条目目录下的视频（含 SPs 等附属目录），返回相对条目目录的路径。
///
/// 上提后的条目目录可能没有直接子视频（`番剧/发布组目录/01.mkv`），因此探测必须递归。
pub fn collect_videos_recursive(dir: &Path, max_depth: usize) -> Result<Vec<RelativeFile>> {
    if !dir.is_dir() {
        return Err(CoreError::NotADirectory(dir.to_path_buf()));
    }

    let mut files = Vec::new();
    let walker = walkdir::WalkDir::new(dir)
        .min_depth(1)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| entry.depth() == 0 || !should_skip_dir(entry.path()));

    for entry in walker {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                tracing::warn!(dir = %dir.display(), error = %err, "跳过无法访问的目录");
                continue;
            }
        };
        if !entry.file_type().is_file() || !is_video_file(entry.path()) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        let Ok(relative) = entry.path().strip_prefix(dir) else {
            continue;
        };
        let relative = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join("/");
        files.push(RelativeFile {
            relative,
            size_bytes: metadata.len(),
        });
    }

    // 直接位于条目目录的文件排在前、子目录（SPs 等）排在后，且同层按名称排序。
    files.sort_by(|left, right| {
        let left_depth = left.relative.matches('/').count();
        let right_depth = right.relative.matches('/').count();
        left_depth
            .cmp(&right_depth)
            .then_with(|| left.relative.cmp(&right.relative))
    });
    Ok(files)
}

/// 集号摘要，例如 `E01–E12`。非连续时退化为集数。
pub fn episode_hint(episodes: &[Option<u32>]) -> Option<String> {
    let mut numbers: Vec<u32> = episodes.iter().flatten().copied().collect();
    if numbers.is_empty() {
        return None;
    }
    numbers.sort_unstable();
    numbers.dedup();

    if numbers.len() == 1 {
        return Some(format!("E{:02}", numbers[0]));
    }
    let contiguous = numbers.windows(2).all(|pair| pair[1] == pair[0] + 1);
    if contiguous {
        let last = numbers[numbers.len() - 1];
        Some(format!("E{:02}–E{:02}", numbers[0], last))
    } else {
        Some(format!("{} 集", numbers.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_hint_formats() {
        assert_eq!(episode_hint(&[None, None]), None);
        assert_eq!(episode_hint(&[Some(3)]).as_deref(), Some("E03"));
        assert_eq!(
            episode_hint(&[Some(1), Some(2), Some(3)]).as_deref(),
            Some("E01–E03")
        );
        assert_eq!(episode_hint(&[Some(1), Some(5)]).as_deref(), Some("2 集"));
    }

    #[test]
    fn extension_matching_is_case_insensitive() {
        assert!(is_video_file(Path::new("a.MKV")));
        assert!(!is_video_file(Path::new("a.jpg")));
    }

    #[test]
    fn system_dirs_are_skipped() {
        assert!(should_skip_dir(Path::new("D:/anime/$RECYCLE.BIN")));
        assert!(should_skip_dir(Path::new("D:/anime/.hidden")));
        assert!(!should_skip_dir(Path::new("D:/anime/Show")));
    }

    #[test]
    fn container_dirs_are_recognized() {
        for name in ["SPs", "SP 1", "SP-01", "NCOP", "特典", "Subs", "Extras"] {
            assert!(
                is_container_dir(Path::new(&format!("D:/anime/Show/{name}"))),
                "{name}"
            );
        }
        assert!(!is_container_dir(Path::new("D:/anime/Show")));
        assert!(!is_container_dir(Path::new("D:/anime/Show/Season 1")));
        assert!(!is_container_dir(Path::new("D:/anime/Show/Release")));
    }
}
