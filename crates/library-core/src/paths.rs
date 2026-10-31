use std::path::{Path, PathBuf};

use crate::error::{CoreError, Result};

/// 校验路径存在且为目录，返回调用方传入的原样路径（保留用户可读形式）。
pub fn ensure_directory(path: &Path) -> Result<PathBuf> {
    let meta = std::fs::metadata(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => CoreError::NotADirectory(path.to_path_buf()),
        _ => CoreError::PathUnavailable {
            path: path.to_path_buf(),
            reason: err.to_string(),
        },
    })?;
    if !meta.is_dir() {
        return Err(CoreError::NotADirectory(path.to_path_buf()));
    }
    Ok(path.to_path_buf())
}

/// 规范化到可比较的形式：解析符号链接/短名 + 去掉 Windows verbatim 前缀 + 统一小写。
pub fn normalized(path: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(path).map_err(|err| CoreError::PathUnavailable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    })?;
    let text = canonical.to_string_lossy();
    // canonicalize 在 Windows 上返回 \\?\B:\...；比较用路径不需要该前缀。
    let stripped = text.strip_prefix(r"\\?\").unwrap_or(&text);
    Ok(PathBuf::from(stripped.to_lowercase()))
}

/// 两个根目录不得相同、不得互为父子（plan.md 1.2）。
///
/// 用规范化后的路径比较，避免 `D:\Anime\` 与 `d:\anime` 这类同目录异写法绕过校验。
pub fn ensure_disjoint_roots(anime: &Path, game: &Path) -> Result<()> {
    let anime_norm = normalized(anime)?;
    let game_norm = normalized(game)?;

    if anime_norm == game_norm {
        return Err(CoreError::RootsIdentical(anime.to_path_buf()));
    }
    if anime_norm.starts_with(&game_norm) || game_norm.starts_with(&anime_norm) {
        return Err(CoreError::RootsNested {
            anime: anime.to_path_buf(),
            game: game.to_path_buf(),
        });
    }
    Ok(())
}

/// 判断 `candidate` 是否位于 `root` 之下（用于扫描时过滤越界条目）。
pub fn is_within(root: &Path, candidate: &Path) -> bool {
    match (normalized(root), normalized(candidate)) {
        (Ok(root), Ok(candidate)) => candidate.starts_with(&root),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lagm-paths-{}-{}", name, uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn identical_roots_are_rejected() {
        let dir = temp_dir("identical");
        let err = ensure_disjoint_roots(&dir, &dir).expect_err("应当拒绝");
        assert!(matches!(err, CoreError::RootsIdentical(_)));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn nested_roots_are_rejected_both_directions() {
        let parent = temp_dir("nested");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).expect("child");

        assert!(matches!(
            ensure_disjoint_roots(&parent, &child).expect_err("父/子"),
            CoreError::RootsNested { .. }
        ));
        assert!(matches!(
            ensure_disjoint_roots(&child, &parent).expect_err("子/父"),
            CoreError::RootsNested { .. }
        ));

        std::fs::remove_dir_all(parent).ok();
    }

    #[test]
    fn sibling_roots_are_accepted() {
        let base = temp_dir("sibling");
        let anime = base.join("anime");
        let game = base.join("game");
        std::fs::create_dir_all(&anime).expect("anime");
        std::fs::create_dir_all(&game).expect("game");
        ensure_disjoint_roots(&anime, &game).expect("同级目录应当通过");
        std::fs::remove_dir_all(base).ok();
    }

    #[test]
    fn missing_root_reports_path_unavailable() {
        let missing = std::env::temp_dir().join(format!("lagm-missing-{}", uuid::Uuid::new_v4()));
        let err = ensure_directory(&missing).expect_err("应当失败");
        assert!(matches!(err, CoreError::NotADirectory(_)));
    }
}
