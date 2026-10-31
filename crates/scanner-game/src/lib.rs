//! 游戏扫描器：递归寻找「最近的含可执行文件目录」，把该目录识别为游戏条目
//! （用户已确认的粒度：以 exe 所在目录为条目，覆盖 `游戏名/bin/Game.exe` 这类结构）。
//!
//! 本 crate 只依赖 library-core，写入数据库经 `LibraryRepository` trait 完成。

mod media;
mod name;

pub use media::{ExeFilter, MAX_PROBE_FILES};
pub use name::NameCleaner;

use std::collections::HashSet;
use std::path::Path;

use library_core::events::EventPublisher;
use library_core::model::{ItemKind, MediaFile, MediaProbe, NewItem, ScanReport, UpsertOutcome};
use library_core::repo::LibraryRepository;
use library_core::{paths, AppEvent, Result};

/// 游戏条目扫描器。
pub struct GameScanner {
    cleaner: NameCleaner,
    filter: ExeFilter,
}

impl GameScanner {
    pub fn new() -> Result<Self> {
        Ok(Self {
            cleaner: NameCleaner::new()?,
            filter: ExeFilter::new()?,
        })
    }

    pub fn kind(&self) -> ItemKind {
        ItemKind::Game
    }

    /// 扫描游戏根目录并写入 `library_item`。
    pub fn scan(
        &self,
        root: &Path,
        repo: &dyn LibraryRepository,
        events: &dyn EventPublisher,
    ) -> Result<ScanReport> {
        paths::ensure_directory(root)?;
        let mut report = ScanReport::new(ItemKind::Game, root);
        // 记录本次产出的条目目录，扫描结束后据此收敛孤儿记录（含换根目录后留下的旧条目）。
        let mut kept: HashSet<String> = HashSet::new();
        let mut walker = walkdir::WalkDir::new(root)
            .min_depth(1)
            .follow_links(false)
            .into_iter();

        while let Some(entry) = walker.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(error = %err, "跳过无法访问的目录");
                    continue;
                }
            };

            if !entry.file_type().is_dir() {
                continue;
            }
            report.folders_seen += 1;

            if media::should_skip_dir(entry.path()) {
                walker.skip_current_dir();
                continue;
            }

            let exes = self.filter.collect(entry.path())?;
            if exes.is_empty() {
                continue;
            }

            // 命中 exe 即确定条目边界，不再向下：bin/、data/ 里的附带可执行文件不算独立游戏。
            walker.skip_current_dir();

            let folder_name = entry.file_name().to_string_lossy().to_string();
            // 条目目录可能是 `bin` 这类容器目录，标题回溯到第一个非容器祖先名。
            let title_source = if name::is_generic_dir(&folder_name) {
                nearest_non_generic_ancestor(entry.path(), root)
                    .unwrap_or_else(|| folder_name.clone())
            } else {
                folder_name
            };
            let new_item = NewItem {
                kind: ItemKind::Game,
                title: self.cleaner.clean(&title_source),
                folder_path: entry.path().to_string_lossy().to_string(),
                file_filter: None,
            };
            kept.insert(new_item.folder_path.clone());
            let outcome = repo.upsert_scanned(&new_item)?;
            match &outcome {
                UpsertOutcome::Created(item) => {
                    events.publish(AppEvent::LibraryItemCreated {
                        id: item.id.clone(),
                        kind: item.kind,
                        title: item.title.clone(),
                        folder_path: item.folder_path.clone(),
                    });
                }
                UpsertOutcome::Updated(item) => {
                    events.publish(AppEvent::LibraryItemUpdated {
                        id: item.id.clone(),
                        kind: item.kind,
                        title: item.title.clone(),
                        folder_path: item.folder_path.clone(),
                    });
                }
                UpsertOutcome::Unchanged(_) => {}
            }
            report.record(&outcome);
        }

        let removed = prune_stale(repo, events, &kept)?;
        tracing::info!(
            root = %root.display(),
            scanned = report.folders_seen,
            created = report.created,
            updated = report.updated,
            unchanged = report.unchanged,
            pruned = removed,
            "游戏扫描完成"
        );
        Ok(report)
    }

    /// 读取条目目录内可直接启动的可执行文件。
    ///
    /// 条目目录是「最近的含 exe 目录」，正常情况下直接子文件即有 exe；
    /// 若文件被移动到更深的子目录，再退回深度受限递归，避免详情页显示空列表。
    pub fn probe(&self, folder: &Path) -> Result<MediaProbe> {
        let mut exes = self.filter.collect(folder)?;
        if exes.is_empty() {
            exes = self.filter.collect_nested(folder, 3)?;
        }
        let files: Vec<MediaFile> = exes
            .into_iter()
            .take(MAX_PROBE_FILES)
            .map(|file| MediaFile {
                name: file.name,
                size_bytes: file.size_bytes,
                episode: None,
                group: None,
            })
            .collect();

        let hint = match files.as_slice() {
            [] => None,
            [single] => Some(single.name.clone()),
            multiple => Some(format!("{} 个可执行文件", multiple.len())),
        };

        Ok(MediaProbe {
            file_count: files.len(),
            episode_hint: hint,
            files,
        })
    }
}

/// 从条目目录向上回溯，取第一个不是通用容器名的祖先目录名（不越过扫描根目录）。
fn nearest_non_generic_ancestor(folder: &Path, root: &Path) -> Option<String> {
    let mut current = folder.parent();
    while let Some(dir) = current {
        if !dir.starts_with(root) || dir == root {
            break;
        }
        if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
            if !name::is_generic_dir(name) {
                return Some(name.to_owned());
            }
        }
        current = dir.parent();
    }
    None
}

/// 清理本次扫描未产出的游戏条目（只删记录，不动磁盘文件）。
///
/// 更换资源根目录后，旧根下的条目不在新根的产出集合里，必须一并收敛，
/// 否则列表里会同时出现新旧两个目录的内容。
fn prune_stale(
    repo: &dyn LibraryRepository,
    events: &dyn EventPublisher,
    kept: &HashSet<String>,
) -> Result<usize> {
    let mut removed = 0;
    for item in repo.list_items(Some(ItemKind::Game))? {
        if kept.contains(&item.folder_path) {
            continue;
        }
        if let Err(err) = repo.delete_item(&item.id) {
            tracing::warn!(error = %err, item = %item.id, "清理失效游戏条目失败");
            continue;
        }
        events.publish(AppEvent::LibraryItemDeleted {
            id: item.id.clone(),
            kind: ItemKind::Game,
            title: item.title.clone(),
        });
        removed += 1;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_core::db::Db;
    use library_core::events::NoopPublisher;
    use library_core::repo::SqliteRepository;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lagm-game-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create root");
        dir
    }

    fn repo() -> SqliteRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteRepository::new(Arc::new(db))
    }

    fn touch(dir: &Path, name: &str) {
        fs::create_dir_all(dir).expect("mkdir");
        fs::write(dir.join(name), b"x").expect("write");
    }

    #[test]
    fn finds_exe_in_nested_folder_as_item_root() {
        let root = temp_root("nested");
        touch(&root.join("Hollow Knight").join("bin"), "hollow_knight.exe");

        let scanner = GameScanner::new().expect("scanner");
        let repo = repo();
        let report = scanner.scan(&root, &repo, &NoopPublisher).expect("scan");

        assert_eq!(report.created, 1);
        let items = repo.list_items(Some(ItemKind::Game)).expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Hollow Knight");
        assert!(items[0].folder_path.ends_with("bin"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn folder_without_exe_is_not_an_item() {
        let root = temp_root("noexe");
        touch(&root.join("saves"), "save.dat");
        touch(&root.join("noise"), "unins000.exe");

        let scanner = GameScanner::new().expect("scanner");
        let repo = repo();
        let report = scanner.scan(&root, &repo, &NoopPublisher).expect("scan");
        assert_eq!(report.created, 0);
        assert_eq!(repo.list_items(None).expect("list").len(), 0);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn does_not_descend_below_item_root() {
        let root = temp_root("boundary");
        let game = root.join("Game");
        touch(&game, "Game.exe");
        touch(&game.join("tools"), "tool.exe");

        let scanner = GameScanner::new().expect("scanner");
        let repo = repo();
        let report = scanner.scan(&root, &repo, &NoopPublisher).expect("scan");
        assert_eq!(report.created, 1, "子目录内的 exe 不应再成一个条目");

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probe_lists_executables() {
        let root = temp_root("probe");
        touch(&root, "Game.exe");
        touch(&root, "Launcher.exe");
        touch(&root, "readme.txt");

        let scanner = GameScanner::new().expect("scanner");
        let probe = scanner.probe(&root).expect("probe");
        assert_eq!(probe.file_count, 2);
        assert_eq!(probe.episode_hint.as_deref(), Some("2 个可执行文件"));

        fs::remove_dir_all(root).ok();
    }
}
