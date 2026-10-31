//! 拖入导入：把拖进窗口的目录 / zip 压缩包识别成动画或游戏，并剪切到对应根目录。
//!
//! 只做「识别 + 搬运」两件事，扫描与入库仍由 scanner 完成：
//! 导入结束后由 Services 触发一次 scan，条目才会出现在库里。
//!
//! 安全边界：
//! - 目标目录一律由「配置的根目录 + 名称」拼出来，名称不允许带路径分隔符；
//! - 目标已存在时拒绝，绝不覆盖用户已有目录；
//! - zip 解压用 `enclosed_name` 拒绝 `..` 与绝对路径（zip-slip）。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use library_core::model::ItemKind;

/// 视频扩展名（用于判断「这是动画目录」）。
const VIDEO_EXTS: &[&str] = &[
    "mkv", "mp4", "avi", "rmvb", "rm", "ts", "m2ts", "flv", "wmv", "mov", "webm", "m4v", "mpg",
    "mpeg",
];

/// 递归判断目录像动画还是游戏；`depth` 限制层数，避免在超大目录里全盘遍历。
pub fn classify_dir(dir: &Path) -> Option<ItemKind> {
    let mut has_exe = false;
    let mut has_video = false;
    walk(dir, 0, 3, &mut has_exe, &mut has_video);
    if has_exe {
        Some(ItemKind::Game)
    } else if has_video {
        Some(ItemKind::Anime)
    } else {
        None
    }
}

fn walk(dir: &Path, depth: usize, max_depth: usize, has_exe: &mut bool, has_video: &mut bool) {
    if *has_exe && *has_video {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            if depth < max_depth {
                walk(&path, depth + 1, max_depth, has_exe, has_video);
            }
            continue;
        }
        let Some(ext) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        let ext = ext.to_ascii_lowercase();
        if ext == "exe" {
            *has_exe = true;
        } else if VIDEO_EXTS.contains(&ext.as_str()) {
            *has_video = true;
        }
    }
}

/// 一个待导入项的识别结果。
pub struct ImportPlan {
    /// 实际的「内容目录」：目录来源就是它本身，zip 来源是解压后的目录。
    pub content: PathBuf,
    pub kind: ItemKind,
    /// 放到根目录下时使用的目录名。
    pub name: String,
    /// 导入成功后需要清理的临时解压目录。
    pub cleanup: Option<PathBuf>,
}

/// 判断来源是压缩包还是目录，并给出导入计划。
///
/// zip 会解压到 `staging/<name>/`；调用方负责在成功后删除这个暂存目录。
pub fn plan(source: &Path, staging: &Path) -> Result<ImportPlan, String> {
    if !source.exists() {
        return Err(format!("路径不存在：{}", source.display()));
    }

    if source.is_dir() {
        let kind = classify_dir(source)
            .ok_or_else(|| format!("目录里没有可识别的视频或可执行文件：{}", source.display()))?;
        let name = source
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or_else(|| "目录名无法识别".to_owned())?
            .to_owned();
        return Ok(ImportPlan {
            content: source.to_path_buf(),
            kind,
            name,
            cleanup: None,
        });
    }

    let ext = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if ext != "zip" {
        let archive_like = ["7z", "rar", "tar", "gz", "bz2", "xz"].contains(&ext.as_str());
        return Err(if archive_like {
            format!("暂不支持 .{ext}，请解压后拖入文件夹（或压成 .zip）")
        } else {
            "只支持文件夹与 .zip 压缩包".to_owned()
        });
    }

    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("imported")
        .to_owned();
    let dest = staging.join(&stem);
    if dest.exists() {
        fs::remove_dir_all(&dest)
            .map_err(|err| format!("清理暂存目录失败（{}）：{err}", dest.display()))?;
    }
    fs::create_dir_all(&dest)
        .map_err(|err| format!("创建暂存目录失败（{}）：{err}", dest.display()))?;
    extract_zip(source, &dest)?;

    // 压缩包常常只有一个顶层目录：那种情况下内容就是这个目录，名称也用它。
    let mut top_dirs: Vec<PathBuf> = Vec::new();
    let mut top_files = 0usize;
    let entries = fs::read_dir(&dest).map_err(|err| format!("读取暂存目录失败：{err}"))?;
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            top_dirs.push(entry.path());
        } else {
            top_files += 1;
        }
    }
    if top_dirs.len() == 1 && top_files == 0 {
        let inner = top_dirs.remove(0);
        let name = inner
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(&stem)
            .to_owned();
        let kind = classify_dir(&inner)
            .ok_or_else(|| "压缩包内没有可识别的视频或可执行文件".to_owned())?;
        return Ok(ImportPlan {
            content: inner,
            kind,
            name,
            cleanup: Some(dest),
        });
    }

    let kind =
        classify_dir(&dest).ok_or_else(|| "压缩包内没有可识别的视频或可执行文件".to_owned())?;
    Ok(ImportPlan {
        content: dest.clone(),
        kind,
        name: stem,
        cleanup: Some(dest),
    })
}

/// 解压 zip 到 `dest`；拒绝越界路径与符号链接项。
fn extract_zip(archive_path: &Path, dest: &Path) -> Result<(), String> {
    let file = fs::File::open(archive_path)
        .map_err(|err| format!("打开压缩包失败（{}）：{err}", archive_path.display()))?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|err| format!("读取压缩包失败（可能不是标准 zip）：{err}"))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("读取压缩包第 {index} 项失败：{err}"))?;
        let Some(relative) = entry.enclosed_name() else {
            return Err("压缩包内存在越界路径，已中止".to_owned());
        };
        let target = dest.join(&relative);

        if entry.is_dir() {
            fs::create_dir_all(&target)
                .map_err(|err| format!("创建目录失败（{}）：{err}", target.display()))?;
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("创建目录失败（{}）：{err}", parent.display()))?;
        }
        let mut writer = fs::File::create(&target)
            .map_err(|err| format!("写入文件失败（{}）：{err}", target.display()))?;
        io::copy(&mut entry, &mut writer)
            .map_err(|err| format!("解压文件失败（{}）：{err}", target.display()))?;
    }
    Ok(())
}

/// 把内容搬到 `root/name`。同盘用 rename，跨盘回退为复制后删除。
pub fn move_into(content: &Path, root: &Path, name: &str) -> Result<PathBuf, String> {
    if name.contains(['/', '\\']) || name == "." || name == ".." {
        return Err(format!("目录名不合法：{name}"));
    }
    let root_abs = strip_verbatim(
        root.canonicalize()
            .map_err(|err| format!("根目录不可访问（{}）：{err}", root.display()))?,
    );
    let target = root_abs.join(name);
    if target.exists() {
        return Err(format!("目标已存在，已跳过：{}", target.display()));
    }
    // 源在目标内部会导致自我嵌套（例如把整个根目录拖进来）。
    if let Ok(source_abs) = content.canonicalize() {
        if source_abs.starts_with(&target) || target.starts_with(&source_abs) {
            return Err("来源与目标目录重叠，已拒绝导入".to_owned());
        }
    }

    if fs::rename(content, &target).is_ok() {
        return Ok(target);
    }
    // 跨盘或占用导致 rename 失败：退化为复制 + 删除。
    copy_dir(content, &target)?;
    if content.is_dir() {
        fs::remove_dir_all(content)
            .map_err(|err| format!("清理来源目录失败（{}）：{err}", content.display()))?;
    } else {
        fs::remove_file(content)
            .map_err(|err| format!("清理来源文件失败（{}）：{err}", content.display()))?;
    }
    Ok(target)
}

/// 去掉 canonicalize 产生的 verbatim 前缀（Windows 的 `\\?\`）。
///
/// 带前缀的路径会原样写进数据库，之后「打开目录」「启动播放器」会拿到
/// `\\?\D://...` 这种形式，explorer 与部分播放器不认；UNC 路径保持原样。
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if text.starts_with("\\\\?\\UNC\\") {
        return path;
    }
    if let Some(rest) = text.strip_prefix("\\\\?\\") {
        return PathBuf::from(rest);
    }
    path
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), String> {
    fs::create_dir_all(to).map_err(|err| format!("创建目录失败（{}）：{err}", to.display()))?;
    let entries =
        fs::read_dir(from).map_err(|err| format!("读取目录失败（{}）：{err}", from.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let target = to.join(entry.file_name());
        if path.is_dir() {
            copy_dir(&path, &target)?;
        } else {
            fs::copy(&path, &target).map_err(|err| {
                format!(
                    "复制文件失败（{} → {}）：{err}",
                    path.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lagm-import-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp");
        dir
    }

    #[test]
    fn classifies_video_and_exe_directories() {
        let base = temp_dir("classify");
        let anime = base.join("show");
        fs::create_dir_all(anime.join("Season 1")).expect("anime");
        fs::write(anime.join("Season 1").join("01.mkv"), b"x").expect("video");

        let game = base.join("game");
        fs::create_dir_all(game.join("bin")).expect("game");
        fs::write(game.join("bin").join("game.exe"), b"x").expect("exe");

        let empty = base.join("empty");
        fs::create_dir_all(&empty).expect("empty");

        assert_eq!(classify_dir(&anime), Some(ItemKind::Anime));
        assert_eq!(classify_dir(&game), Some(ItemKind::Game));
        assert_eq!(classify_dir(&empty), None);

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn move_into_rejects_existing_target_and_bad_names() {
        let base = temp_dir("move");
        let root = base.join("anime");
        fs::create_dir_all(&root).expect("root");
        let source = base.join("incoming");
        fs::create_dir_all(&source).expect("source");
        fs::write(source.join("01.mkv"), b"x").expect("file");

        // 正常搬运。
        let target = move_into(&source, &root, "incoming").expect("move");
        assert!(target.join("01.mkv").is_file());
        assert!(!source.exists());

        // 同名已存在：拒绝且不覆盖。
        let again = base.join("incoming");
        fs::create_dir_all(&again).expect("again");
        fs::write(again.join("keep.txt"), b"keep").expect("keep");
        assert!(move_into(&again, &root, "incoming").is_err());
        // 原有目标目录必须原样保留，不能被同名来源覆盖。
        assert!(target.join("01.mkv").is_file());
        assert!(!target.join("keep.txt").exists());

        // 带路径分隔符的名字直接拒绝。
        assert!(move_into(&again, &root, "a/b").is_err());

        fs::remove_dir_all(&base).ok();
    }
}
