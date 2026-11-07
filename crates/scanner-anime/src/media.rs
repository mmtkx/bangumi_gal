use std::cmp::Ordering;
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
    "image",
    "images",
    "picture",
    "pictures",
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

/// 中文目录名用「包含」匹配：`特典映像`、`映像特典`、`特典盘` 都能命中「特典」。
///
/// 只放足够特异的词，避免把作品名误判成附属目录（例如整词 `映像` 就会误伤「映像研」）。
const CONTAINER_NAME_CONTAINS: &[&str] = &[
    "特典",
    "图集",
    "插画",
    "插图",
    "扫图",
    "设定集",
    "原画集",
    "字幕",
    "音乐",
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
    path.file_name()
        .and_then(|name| name.to_str())
        .map(is_container_name)
        .unwrap_or(false)
}

fn is_container_name(name: &str) -> bool {
    let normalized = normalize_dir_name(name);
    if CONTAINER_DIR_NAMES
        .iter()
        .any(|candidate| *candidate == normalized)
    {
        return true;
    }
    // 中文目录名很少恰好等于某个固定词（`特典映像`、`映像特典`、`图集` 都可能），
    // 用「包含」判断；拉丁名仍走精确匹配，避免 `Images` 之外的意外命中。
    CONTAINER_NAME_CONTAINS
        .iter()
        .any(|marker| normalized.contains(marker))
}

/// 相对路径是否位于附属内容目录之下（`SPs/01.mkv`、`特典/NCOP 2.mkv`）。
///
/// 只看目录层，最后一层是文件名。附属目录里的文件名常带自己的编号
/// （`SP 01`、`NCOP 02`），与正片集号撞车后会把进度写坏，因此这些文件不采信解析结果。
pub fn is_special_path(relative: &str) -> bool {
    let mut parts: Vec<&str> = relative.split('/').collect();
    let Some(file_name) = parts.pop() else {
        return false;
    };
    if parts.iter().any(|part| is_container_name(part)) {
        return true;
    }
    // 文件名里的方括号标记同样说明是附属内容：`[10][OVA][1080P].mkv` 里的 `[OVA]`
    // 不该被当成第 10 集去占正片的集号。只看方括号内容，避免误伤标题里带 ova 字样的正片。
    bracket_contents(file_name)
        .iter()
        .any(|inner| is_container_name(inner))
}

/// 取出文件名里所有方括号/中文方括号的内容。
fn bracket_contents(name: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = name;
    while let Some(start) = rest.find(['[', '【']) {
        let after = &rest[start + 1..];
        let Some(end) = after.find([']', '】']) else {
            break;
        };
        out.push(&after[..end]);
        rest = &after[end + 1..];
    }
    out
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

/// 自然排序：数字段按数值比较，文本段按不区分大小写比较。
///
/// 解析不出集号的文件（菜单、NCOP、SP 等）要靠文件名决定先后，
/// 字典序会把 `SP 10` 排在 `SP 2` 前面，与用户预期相反。
pub fn natural_cmp(left: &str, right: &str) -> Ordering {
    let left_tokens = tokenize(left);
    let right_tokens = tokenize(right);
    for (left_token, right_token) in left_tokens.iter().zip(right_tokens.iter()) {
        let order = match (left_token, right_token) {
            (Token::Num(left_num), Token::Num(right_num)) => compare_digits(left_num, right_num),
            _ => text_cmp(left_token.text(), right_token.text()),
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    left_tokens
        .len()
        .cmp(&right_tokens.len())
        .then_with(|| left.cmp(right))
}

enum Token<'a> {
    Num(&'a str),
    Text(&'a str),
}

impl Token<'_> {
    fn text(&self) -> &str {
        match self {
            Token::Num(text) | Token::Text(text) => text,
        }
    }
}

/// 只按 ASCII 数字切段：多字节字符的每个字节都 >= 0x80，不会被当成数字，
/// 因此切点一定落在字符边界上，可以直接按字节切片。
fn tokenize(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut tokens = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let digits = bytes[start].is_ascii_digit();
        let mut end = start + 1;
        while end < bytes.len() && bytes[end].is_ascii_digit() == digits {
            end += 1;
        }
        let slice = &text[start..end];
        tokens.push(if digits {
            Token::Num(slice)
        } else {
            Token::Text(slice)
        });
        start = end;
    }
    tokens
}

/// 按数值比较两段数字，先去掉前导零再比长度，避免大数溢出。
fn compare_digits(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn text_cmp(left: &str, right: &str) -> Ordering {
    let mut left_chars = left.chars().flat_map(char::to_lowercase);
    let mut right_chars = right.chars().flat_map(char::to_lowercase);
    loop {
        match (left_chars.next(), right_chars.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(left_char), Some(right_char)) => match left_char.cmp(&right_char) {
                Ordering::Equal => {}
                other => return other,
            },
        }
    }
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
    fn natural_order_compares_digit_runs_by_value() {
        let mut names = ["SP 10.mkv", "SP 2.mkv", "SP 1.mkv"];
        names.sort_by(|left, right| natural_cmp(left, right));
        assert_eq!(names, ["SP 1.mkv", "SP 2.mkv", "SP 10.mkv"]);

        assert_eq!(natural_cmp("Show A", "Show a"), Ordering::Less);
        assert_eq!(natural_cmp("Show 007", "Show 7"), Ordering::Less);
        assert_eq!(natural_cmp("特典 1", "特典 2"), Ordering::Less);
    }

    #[test]
    fn special_paths_are_detected_by_directory() {
        for path in ["SPs/01.mkv", "特典/NCOP 2.mkv", "Season 1/Extras/a.mkv"] {
            assert!(is_special_path(path), "{path}");
        }
        for path in ["01.mkv", "Season 1/01.mkv", "SP.mkv"] {
            assert!(!is_special_path(path), "{path}");
        }
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
