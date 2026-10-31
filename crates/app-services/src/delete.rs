//! 资源删除的文件层实现（plan.md 6.5 / ADR-005）。
//!
//! 这里只做「哪些路径可以删」与「怎么删」两件事：
//! - 路径必须落在配置的动画 / 游戏根目录之内，且不能是根目录本身；
//! - 默认走系统回收站（trash），永久删除只在调用方已确认的情况下使用。

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    /// 移入系统回收站（默认，可恢复）。
    Trash,
    /// 不可恢复的物理删除。
    Permanent,
}

impl DeleteMode {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trash" => Some(Self::Trash),
            "permanent" => Some(Self::Permanent),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Trash => "trash",
            Self::Permanent => "permanent",
        }
    }
}

/// 校验删除目标位于根目录之下（不含根目录本身）。
///
/// 用 canonicalize 后的路径比较，避免 `..`、符号链接与大小写差异绕过；
/// 目标不存在时无法 canonicalize，由调用方按「文件已不在」处理。
pub fn ensure_inside(folder: &Path, root: &Path) -> Result<PathBuf, String> {
    let folder_abs = folder
        .canonicalize()
        .map_err(|err| format!("条目目录不可访问（{}）：{err}", folder.display()))?;
    let root_abs = root
        .canonicalize()
        .map_err(|_| format!("根目录不可访问：{}", root.display()))?;
    if folder_abs == root_abs {
        return Err("条目目录等于根目录，已拒绝删除".to_owned());
    }
    if !folder_abs.starts_with(&root_abs) {
        return Err(format!(
            "条目目录不在根目录内，已拒绝删除：{}",
            folder_abs.display()
        ));
    }
    Ok(folder_abs)
}

/// 目录删除后端。
///
/// 抽成 trait 是为了让自动化测试不接触真实回收站：`cargo test` 每跑一次都会往
/// `$Recycle.Bin` 丢一份临时目录（回收站是复制语义，写盘量翻倍），跑几十次就会
/// 在回收站里堆出成千上万个条目。测试注入 [`NoRecycleBin`] 这类等价实现。
pub trait FolderRemover: Send + Sync {
    fn trash(&self, folder: &Path) -> Result<(), String>;
    fn permanent(&self, folder: &Path) -> Result<(), String>;
}

/// 生产实现：回收站走 `trash` crate，永久删除走 `remove_dir_all`。
pub struct SystemRemover;

impl FolderRemover for SystemRemover {
    fn trash(&self, folder: &Path) -> Result<(), String> {
        trash::delete(folder)
            .map_err(|err| format!("移入回收站失败（{}）：{err}", folder.display()))
    }

    fn permanent(&self, folder: &Path) -> Result<(), String> {
        std::fs::remove_dir_all(folder)
            .map_err(|err| format!("永久删除失败（{}）：{err}", folder.display()))
    }
}

/// 按模式删除目录。目录不存在视为已完成（只清记录即可）。
pub fn remove_folder(
    folder: &Path,
    mode: DeleteMode,
    remover: &dyn FolderRemover,
) -> Result<(), String> {
    if !folder.exists() {
        return Ok(());
    }
    match mode {
        DeleteMode::Trash => remover.trash(folder),
        DeleteMode::Permanent => remover.permanent(folder),
    }
}

/// 测试用删除后端：**不触碰系统回收站**，直接物理删除并记录调用次数。
///
/// 禁止在测试里调用 `trash::delete`：回收站是复制语义，会把临时目录整份写进
/// `$Recycle.Bin`，测试跑几十次就能堆出成千上万个条目。
#[cfg(test)]
#[derive(Default)]
pub(crate) struct NoRecycleBin {
    trash_calls: std::sync::atomic::AtomicUsize,
    permanent_calls: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl NoRecycleBin {
    pub(crate) fn trash_calls(&self) -> usize {
        self.trash_calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn permanent_calls(&self) -> usize {
        self.permanent_calls
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
impl FolderRemover for NoRecycleBin {
    fn trash(&self, folder: &Path) -> Result<(), String> {
        self.trash_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        remove_tree_for_test(folder)
    }

    fn permanent(&self, folder: &Path) -> Result<(), String> {
        self.permanent_calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        remove_tree_for_test(folder)
    }
}

#[cfg(test)]
fn remove_tree_for_test(folder: &Path) -> Result<(), String> {
    std::fs::remove_dir_all(folder)
        .map_err(|err| format!("删除失败（{}）：{err}", folder.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lagm-delete-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    #[test]
    fn parses_modes() {
        assert_eq!(DeleteMode::parse("trash"), Some(DeleteMode::Trash));
        assert_eq!(DeleteMode::parse("PERMANENT"), Some(DeleteMode::Permanent));
        assert_eq!(DeleteMode::parse("rm-rf"), None);
    }

    #[test]
    fn rejects_paths_outside_root_and_root_itself() {
        let base = temp_dir("boundary");
        let root = base.join("anime");
        let inside = root.join("show");
        let outside = base.join("elsewhere");
        fs::create_dir_all(&inside).expect("inside");
        fs::create_dir_all(&outside).expect("outside");

        assert!(ensure_inside(&inside, &root).is_ok());
        assert!(ensure_inside(&root, &root).is_err());
        assert!(ensure_inside(&outside, &root).is_err());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn missing_folder_is_treated_as_done() {
        let base = temp_dir("missing");
        let gone = base.join("not-here");
        remove_folder(&gone, DeleteMode::Permanent, &NoRecycleBin::default())
            .expect("已不存在应视为完成");
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn permanent_removes_directory_tree() {
        let base = temp_dir("permanent");
        let folder = base.join("show");
        fs::create_dir_all(folder.join("GroupA")).expect("nested");
        fs::write(folder.join("GroupA").join("01.mkv"), b"x").expect("file");

        let remover = NoRecycleBin::default();
        remove_folder(&folder, DeleteMode::Permanent, &remover).expect("remove");
        assert!(!folder.exists());
        assert_eq!(remover.permanent_calls(), 1);
        assert_eq!(remover.trash_calls(), 0);

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn trash_mode_routes_to_remover_without_touching_recycle_bin() {
        let base = temp_dir("trash");
        let folder = base.join("show");
        fs::create_dir_all(&folder).expect("dir");
        fs::write(folder.join("01.mkv"), b"x").expect("file");

        // 只断言「模式路由正确」，真实回收站行为由手工验收覆盖。
        let remover = NoRecycleBin::default();
        remove_folder(&folder, DeleteMode::Trash, &remover).expect("trash 路由");
        assert!(!folder.exists());
        assert_eq!(remover.trash_calls(), 1);
        assert_eq!(remover.permanent_calls(), 0);

        fs::remove_dir_all(&base).ok();
    }
}
