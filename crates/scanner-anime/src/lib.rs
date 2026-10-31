//! 动画扫描器：把「番剧目录」识别为条目，并从文件夹名 / 文件名解析标题与集号
//! （ADR-004：正则规则引擎 + 手动绑定兜底）。
//!
//! 条目目录判定：
//! - 候选：根目录下任意「直接含视频文件」的目录；
//! - 归并：候选一律上提到根目录第一层子目录作为条目目录，因此同一番剧下的多个压制组、
//!   分季目录、附属目录（SPs / 特典 / NCOP 等）都属于同一个条目；
//! - 区分：`MediaFile.group` 记录文件所属的相对子目录，详情页据此展示来源差异。
//!
//! 已知前提：根目录下一层就是番剧目录。若根目录下还有「季度/年份」这类中间层，
//! 会把整层合并成一个条目，需要改为可配置扫描深度。
//!
//! 本 crate 只依赖 library-core，写入数据库经 `LibraryRepository` trait 完成。

mod media;
mod title;

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub use media::{episode_hint, is_container_dir, is_video_file, MAX_PROBE_FILES};
pub use title::TitleParser;

use library_core::events::EventPublisher;
use library_core::model::{ItemKind, MediaFile, MediaProbe, NewItem, ScanReport, UpsertOutcome};
use library_core::repo::LibraryRepository;
use library_core::{paths, AppEvent, Result};

/// 探测条目文件时的最大递归深度：足够覆盖 `番剧/发布组/Season/文件`。
const PROBE_MAX_DEPTH: usize = 4;

/// 动画条目扫描器。
pub struct AnimeScanner {
    parser: TitleParser,
}

impl AnimeScanner {
    pub fn new() -> Result<Self> {
        Ok(Self {
            parser: TitleParser::new()?,
        })
    }

    pub fn kind(&self) -> ItemKind {
        ItemKind::Anime
    }

    /// 扫描动画根目录并写入 `library_item`。
    pub fn scan(
        &self,
        root: &Path,
        repo: &dyn LibraryRepository,
        events: &dyn EventPublisher,
    ) -> Result<ScanReport> {
        paths::ensure_directory(root)?;
        let mut report = ScanReport::new(ItemKind::Anime, root);

        // 第一遍：收集目录树与「直接含视频」的目录。
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut has_direct_videos: HashSet<PathBuf> = HashSet::new();
        let mut walker = walkdir::WalkDir::new(root)
            .min_depth(1)
            .follow_links(false)
            .into_iter();

        while let Some(entry) = walker.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    // 单个目录不可读不应中断整次扫描，跳过并继续。
                    tracing::warn!(error = %err, "跳过无法访问的目录");
                    continue;
                }
            };
            if !entry.file_type().is_dir() {
                continue;
            }
            if media::should_skip_dir(entry.path()) {
                walker.skip_current_dir();
                continue;
            }

            let path = entry.path().to_path_buf();
            if media::has_direct_videos(&path)? {
                has_direct_videos.insert(path.clone());
            }
            dirs.push(path);
        }
        report.folders_seen = dirs.len();

        // 第二遍：候选目录一律上提到根目录下的第一层，作为条目目录。
        //
        // 同一部番下的多个压制组、分季目录、附属目录都归到该番剧一个条目里，
        // 具体来源差异由详情页按 `group`（相对子目录）区分。
        let mut item_roots: BTreeMap<String, PathBuf> = BTreeMap::new();
        for dir in &dirs {
            if media::is_container_dir(dir) || !has_direct_videos.contains(dir) {
                continue;
            }
            let mut current = dir.clone();
            while let Some(parent) = current.parent().map(Path::to_path_buf) {
                // 条目目录必须落在根目录之下；根目录本身不作为条目。
                if parent == root || !parent.starts_with(root) {
                    break;
                }
                current = parent;
            }
            item_roots.insert(current.to_string_lossy().to_string(), current);
        }

        // 第三遍：根目录直属视频按「系列键」分组。
        //
        // 下载后把某部番的 01/02/03 直接摊在根目录是很常见的整理习惯；
        // 这时没有子目录可当条目，只能从文件名反推系列标题来分组。
        // 条目身份是 (目录, 分组键)，因此同一目录可以承载多个条目。
        let mut planned: Vec<(PathBuf, Option<String>, String)> = Vec::new();
        for item_root in item_roots.values() {
            let folder_name = item_root
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            planned.push((
                item_root.clone(),
                None,
                self.parser.parse_title(&folder_name),
            ));
        }
        for (key, title) in self.loose_groups(root)? {
            planned.push((root.to_path_buf(), Some(key), title));
        }

        let mut kept: HashSet<String> = HashSet::new();
        for (folder, filter, title) in planned {
            let new_item = NewItem {
                kind: ItemKind::Anime,
                title,
                folder_path: folder.to_string_lossy().to_string(),
                file_filter: filter.clone(),
            };
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
            kept.insert(scope_key(&folder, filter.as_deref()));
        }

        let removed = self.prune_stale(root, repo, events, &kept)?;
        tracing::info!(
            root = %root.display(),
            scanned = report.folders_seen,
            created = report.created,
            updated = report.updated,
            unchanged = report.unchanged,
            pruned = removed,
            "动画扫描完成"
        );
        Ok(report)
    }

    /// 清理本次扫描未产出的旧条目（只删记录，不动磁盘文件）。
    ///
    /// 目录结构变化、判定规则变化、以及**更换资源根目录**都会让旧记录变成孤儿：
    /// 后者的旧条目根本不在新根目录下，若只按「是否在新根下」过滤就会永远留着。
    /// 因此这里按「本次扫描产出的条目集合」反推，凡是没产出的动画条目一律收敛。
    fn prune_stale(
        &self,
        _root: &Path,
        repo: &dyn LibraryRepository,
        events: &dyn EventPublisher,
        kept: &HashSet<String>,
    ) -> Result<usize> {
        let mut removed = 0;

        for item in repo.list_items(Some(ItemKind::Anime))? {
            if kept.contains(&scope_key(
                Path::new(&item.folder_path),
                item.file_filter.as_deref(),
            )) {
                continue;
            }
            if let Err(err) = repo.delete_item(&item.id) {
                tracing::warn!(error = %err, item = %item.id, "清理失效条目失败");
                continue;
            }
            events.publish(AppEvent::LibraryItemDeleted {
                id: item.id.clone(),
                kind: ItemKind::Anime,
                title: item.title.clone(),
            });
            removed += 1;
        }
        Ok(removed)
    }

    /// 根目录直属视频按「系列键」分组，返回 (键, 展示标题)。
    ///
    /// 只读一层：子目录内容已由目录型条目覆盖。键来自文件名解析出的系列标题，
    /// 因此同一部番的 01/02/03 会归到同一组，不同番剧不会混在一起。
    fn loose_groups(&self, root: &Path) -> Result<Vec<(String, String)>> {
        let mut groups: BTreeMap<String, String> = BTreeMap::new();
        for stem in media::direct_video_stems(root)? {
            let title = self.parser.parse_series_title(&stem);
            let key = media::normalize_key(&title);
            if key.is_empty() {
                continue;
            }
            groups.entry(key).or_insert(title);
        }
        Ok(groups.into_iter().collect())
    }

    /// 文件是否属于某个松散分组。
    fn file_in_group(&self, relative: &str, filter: &str) -> bool {
        let bare = relative.rsplit('/').next().unwrap_or(relative);
        let stem = bare.rsplit_once('.').map(|(head, _)| head).unwrap_or(bare);
        media::normalize_key(&self.parser.parse_series_title(stem)) == filter
    }

    /// 目录内出现次数最多的几个「系列标题」，按次数降序。
    ///
    /// 用途：用户自己加了一层「壳目录」（名字随手起，例如「辉夜姬」），
    /// 真正能和 Bangumi 对上的是里面的文件名。目录名匹配不上时再拿这些标题试。
    /// 松散条目（file_filter 非空）里文件名就是分组依据，结果会与条目名重复，
    /// 由调用方去重。
    pub fn dominant_series_titles(
        &self,
        folder: &Path,
        filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<String>> {
        let probe = self.probe(folder, filter)?;
        let mut counts: BTreeMap<String, (String, usize)> = BTreeMap::new();
        for file in &probe.files {
            let bare = file.name.rsplit('/').next().unwrap_or(&file.name);
            let stem = bare.rsplit_once('.').map(|(head, _)| head).unwrap_or(bare);
            let title = self.parser.parse_series_title(stem);
            let key = media::normalize_key(&title);
            if key.is_empty() {
                continue;
            }
            let slot = counts.entry(key).or_insert((title, 0));
            slot.1 += 1;
        }

        let mut ranked: Vec<(String, usize)> = counts.into_values().collect();
        // 次数相同时按标题排序，保证结果稳定（否则 HashMap 顺序会让测试抖动）。
        ranked.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
        Ok(ranked
            .into_iter()
            .take(limit.max(1))
            .map(|(title, _)| title)
            .collect())
    }

    /// 读取条目目录的媒体信息（递归，含多个压制组 / 分季 / 附属目录）。
    ///
    /// `filter` 是该条目的分组键：根目录松散文件会共用一个目录，
    /// 只有键一致的文件才属于这个条目；`None` 表示整个目录都属于它。
    pub fn probe(&self, folder: &Path, filter: Option<&str>) -> Result<MediaProbe> {
        let mut collected = media::collect_videos_recursive(folder, PROBE_MAX_DEPTH)?;
        if let Some(filter) = filter {
            let total = collected.len();
            collected.retain(|file| self.file_in_group(&file.relative, filter));
            if collected.is_empty() && total > 0 {
                // 解析规则变化可能让旧的分组键失效：宁可先展示全部文件，也不让条目空掉。
                tracing::warn!(
                    folder = %folder.display(),
                    filter,
                    files = total,
                    "分组键未匹配到任何文件，退回展示目录内全部视频"
                );
                collected = media::collect_videos_recursive(folder, PROBE_MAX_DEPTH)?;
            }
        }

        let mut files: Vec<MediaFile> = collected
            .into_iter()
            .take(MAX_PROBE_FILES)
            .map(|file| {
                let bare_name = file
                    .relative
                    .rsplit('/')
                    .next()
                    .unwrap_or(file.relative.as_str())
                    .to_string();
                let group = file
                    .relative
                    .rfind('/')
                    .map(|index| file.relative[..index].to_string());
                MediaFile {
                    episode: self.parser.parse_episode(&bare_name),
                    name: file.relative,
                    size_bytes: file.size_bytes,
                    group,
                }
            })
            .collect();

        // 直属文件排前，其次按来源目录分组，组内按集号与文件名；详情页据此分组展示。
        normalize_groups(&mut files);
        files.sort_by(|left, right| {
            left.group
                .is_some()
                .cmp(&right.group.is_some())
                .then_with(|| left.group.cmp(&right.group))
                .then_with(|| left.episode.cmp(&right.episode))
                .then_with(|| left.name.cmp(&right.name))
        });

        // 集号摘要只统计解析出来的集号，避免顺延编号把 SP 算成正片。
        let parsed: Vec<Option<u32>> = files.iter().map(|file| file.episode).collect();

        // 解析不到集号的文件（SP / NCOP / 剧场版等）排在已解析最大集号之后顺延编号，
        // 避免与正片集号撞车导致进度互相覆盖。
        fill_missing_episodes(&mut files);

        Ok(MediaProbe {
            file_count: files.len(),
            episode_hint: episode_hint(&parsed),
            files,
        })
    }
}

/// 条目身份：目录 + 分组键。
///
/// 根目录松散条目共用同一个目录，必须带上分组键才能区分；目录型条目没有键，
/// 直接用目录路径，保持与旧数据一致。
fn scope_key(folder: &Path, filter: Option<&str>) -> String {
    match filter {
        Some(value) => format!("{}|{}", folder.to_string_lossy(), value),
        None => folder.to_string_lossy().to_string(),
    }
}

/// 去掉所有来源目录的公共前缀，让「来源」显示为有意义的相对分支。
///
/// 例：`[VCB] Show` 与 `[VCB] Show/SPs` 去公共前缀后显示为「主目录」与「SPs」；
/// 两个不同压制组则保持各自的目录名。
fn normalize_groups(files: &mut [MediaFile]) {
    let mut dirs: Vec<String> = files.iter().filter_map(|file| file.group.clone()).collect();
    dirs.sort();
    dirs.dedup();
    if dirs.len() < 2 {
        return;
    }

    let mut prefix: Vec<String> = dirs[0].split('/').map(str::to_owned).collect();
    for dir in &dirs[1..] {
        let parts: Vec<String> = dir.split('/').map(str::to_owned).collect();
        let common = prefix
            .iter()
            .zip(parts.iter())
            .take_while(|(left, right)| left == right)
            .count();
        prefix.truncate(common);
        if prefix.is_empty() {
            return;
        }
    }

    let prefix_len = prefix.join("/").len();
    for file in files.iter_mut() {
        if let Some(group) = &file.group {
            let rest = group[prefix_len..].trim_start_matches('/').to_string();
            file.group = if rest.is_empty() { None } else { Some(rest) };
        }
    }
}

/// 给没有解析出集号的文件顺延编号，保证一次探测内集号唯一。
fn fill_missing_episodes(files: &mut [MediaFile]) {
    let max_parsed = files
        .iter()
        .filter_map(|file| file.episode)
        .max()
        .unwrap_or(0);
    let mut next = max_parsed.saturating_add(1);
    for file in files.iter_mut() {
        if file.episode.is_none() {
            file.episode = Some(next);
            next = next.saturating_add(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_core::db::Db;
    use library_core::events::NoopPublisher;
    use library_core::repo::SqliteRepository;
    use std::fs;
    use std::sync::Arc;

    fn temp_root(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("lagm-anime-{}-{}", name, uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("create root");
        dir
    }

    fn repo() -> SqliteRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteRepository::new(Arc::new(db))
    }

    fn noop() -> NoopPublisher {
        NoopPublisher
    }

    fn touch(dir: &Path, name: &str) {
        fs::create_dir_all(dir).expect("mkdir");
        fs::write(dir.join(name), b"x").expect("write");
    }

    #[test]
    fn rescan_new_root_drops_items_from_old_root() {
        let first = temp_root("root-a");
        let show_a = first.join("[Group] Show A");
        touch(&show_a, "Show A - 01.mkv");

        let second = temp_root("root-b");
        let show_b = second.join("[Group] Show B");
        touch(&show_b, "Show B - 01.mkv");

        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        scanner.scan(&first, &repo, &noop()).expect("scan a");
        assert_eq!(repo.list_items(None).expect("list").len(), 1);

        scanner.scan(&second, &repo, &noop()).expect("scan b");
        let items = repo.list_items(None).expect("list");
        assert_eq!(items.len(), 1, "换根目录后旧根下的条目应被清理");
        assert!(
            items[0].folder_path.contains("root-b"),
            "只应保留新根目录下的条目，实际 {}",
            items[0].folder_path
        );

        fs::remove_dir_all(first).ok();
        fs::remove_dir_all(second).ok();
    }

    #[test]
    fn dominant_series_titles_reads_file_names() {
        // 「壳目录」场景：目录名是用户随手起的，标题只存在于文件名里。
        let root = temp_root("shell");
        let show = root.join("辉夜姬");
        touch(&show, "Kaguya-sama wa Kokurasetai - 01.mkv");
        touch(&show, "Kaguya-sama wa Kokurasetai - 02.mkv");
        touch(&show, "[Group] Kaguya-sama wa Kokurasetai - 03.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        let titles = scanner
            .dominant_series_titles(&show, None, 2)
            .expect("titles");
        assert!(
            titles.iter().any(|t| t.contains("Kaguya-sama")),
            "应从文件名提取系列标题，实际 {titles:?}"
        );

        // 空目录不能返回伪标题。
        let empty = root.join("empty");
        fs::create_dir_all(&empty).expect("mkdir");
        assert!(scanner
            .dominant_series_titles(&empty, None, 2)
            .expect("titles")
            .is_empty());

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn loose_videos_in_root_are_grouped_by_series() {
        let root = temp_root("loose");
        touch(&root, "Kaguya-sama wa Kokurasetai - 01.mkv");
        touch(&root, "Kaguya-sama wa Kokurasetai - 02.mkv");
        touch(&root, "Bocchi the Rock - 01.mkv");

        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        scanner.scan(&root, &repo, &noop()).expect("scan");

        let items = repo.list_items(None).expect("list");
        assert_eq!(items.len(), 2, "应按文件名分成两个条目");

        let kaguya = items
            .iter()
            .find(|item| item.title.contains("Kaguya"))
            .expect("辉夜条目");
        // 条目指向根目录本身，靠分组键区分归属。
        assert_eq!(
            Path::new(&kaguya.folder_path),
            root.as_path(),
            "根目录松散条目的 folder_path 应是根目录"
        );
        assert!(kaguya.file_filter.is_some());

        let probe = scanner
            .probe(&root, kaguya.file_filter.as_deref())
            .expect("probe");
        assert_eq!(probe.files.len(), 2, "只应包含本组的两个文件");
        assert!(probe
            .files
            .iter()
            .all(|file| file.name.contains("Kaguya-sama")));

        // 另一个分组互不干扰。
        let bocchi = items
            .iter()
            .find(|item| item.title.contains("Bocchi"))
            .expect("波奇条目");
        let probe = scanner
            .probe(&root, bocchi.file_filter.as_deref())
            .expect("probe");
        assert_eq!(probe.files.len(), 1);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_creates_items_only_for_folders_with_videos() {
        let root = temp_root("scan");
        let show = root.join("[Nekomoe] Sousou no Frieren");
        touch(&show, "[Nekomoe] Sousou no Frieren - 01 [1080p].mkv");
        touch(&show, "[Nekomoe] Sousou no Frieren - 02 [1080p].mkv");
        // 只有字幕的目录不应成为条目
        let subs = root.join("subs");
        touch(&subs, "readme.txt");

        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        let report = scanner.scan(&root, &repo, &noop()).expect("scan");

        assert_eq!(report.created, 1);
        let items = repo.list_items(Some(ItemKind::Anime)).expect("list");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Sousou no Frieren");
        assert_eq!(items[0].folder_path, show.to_string_lossy());

        fs::remove_dir_all(root).ok();
    }

    /// 真实库里的形态：番剧目录下只有一个发布组目录，条目应上提到番剧层。
    #[test]
    fn single_release_folder_is_lifted_to_show_folder() {
        let root = temp_root("lift");
        let show = root.join("夏日重现");
        let release =
            show.join("[Nekomoe kissaten&VCB-Studio] Summer Time Rendering [Ma10p_1080p]");
        touch(
            &release,
            "[Nekomoe] Summer Time Rendering - 01 [Ma10p_1080p].mkv",
        );

        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        let report = scanner.scan(&root, &repo, &noop()).expect("scan");

        assert_eq!(report.created, 1, "应只生成一个条目");
        let items = repo.list_items(Some(ItemKind::Anime)).expect("list");
        assert_eq!(items[0].title, "夏日重现");
        assert_eq!(items[0].folder_path, show.to_string_lossy());

        fs::remove_dir_all(root).ok();
    }

    /// SPs 这类附属目录不单独成条，视频归入番剧条目。
    #[test]
    fn container_dirs_do_not_become_items() {
        let root = temp_root("container");
        let show = root.join("夏日重现");
        let release = show.join("Release");
        touch(&release, "Show - 01.mkv");
        touch(&show.join("SPs"), "Show - SP01.mkv");
        touch(&show.join("NCOP"), "NCOP.mkv");

        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        scanner.scan(&root, &repo, &noop()).expect("scan");

        let items = repo.list_items(Some(ItemKind::Anime)).expect("list");
        assert_eq!(items.len(), 1, "SPs / NCOP 不应各自成条");
        assert_eq!(items[0].folder_path, show.to_string_lossy());

        let probe = scanner.probe(&show, None).expect("probe");
        assert_eq!(probe.file_count, 3, "递归应包含附属目录里的视频");
        assert!(probe.files.iter().any(|file| file.name.contains("SPs/")));
        // 集号必须唯一，否则进度会互相覆盖。
        let episodes: Vec<u32> = probe.files.iter().filter_map(|file| file.episode).collect();
        assert_eq!(episodes.len(), 3);
        let mut unique = episodes.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 3, "集号不应重复：{episodes:?}");

        fs::remove_dir_all(root).ok();
    }

    /// 多季并列时也归到同一个条目，差异由来源目录体现。
    #[test]
    fn seasons_are_merged_into_one_item() {
        let root = temp_root("seasons");
        let show = root.join("Show");
        touch(&show.join("Season 1"), "Show S01E01.mkv");
        touch(&show.join("Season 2"), "Show S02E01.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        let repo = repo();
        let report = scanner.scan(&root, &repo, &noop()).expect("scan");
        assert_eq!(report.created, 1);

        let probe = scanner.probe(&show, None).expect("probe");
        assert_eq!(probe.file_count, 2);
        let mut groups: Vec<String> = probe
            .files
            .iter()
            .filter_map(|file| file.group.clone())
            .collect();
        groups.sort();
        assert_eq!(groups, vec!["Season 1".to_owned(), "Season 2".to_owned()]);

        fs::remove_dir_all(root).ok();
    }

    /// 同一番剧下两个压制组：合并成一个条目，靠 group 与文件名区分。
    #[test]
    fn multiple_release_groups_merge_into_one_item() {
        let root = temp_root("groups");
        let show = root.join("夏日重现");
        touch(
            &show.join("[VCB-Studio] Show"),
            "[VCB-Studio] Show - 01.mkv",
        );
        touch(&show.join("[Nekomoe] Show"), "[Nekomoe] Show - 01.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        let repo = repo();
        let report = scanner.scan(&root, &repo, &noop()).expect("scan");
        assert_eq!(report.created, 1, "两个压制组应属于同一个番剧条目");

        let probe = scanner.probe(&show, None).expect("probe");
        assert_eq!(probe.file_count, 2);
        assert_eq!(probe.files.len(), 2);
        // 同一集在不同组下各有一个文件，集号允许相同，靠 group 区分。
        assert_eq!(probe.files[0].episode, Some(1));
        assert_eq!(probe.files[1].episode, Some(1));
        let groups: Vec<String> = probe
            .files
            .iter()
            .filter_map(|file| file.group.clone())
            .collect();
        assert_eq!(groups.len(), 2);
        assert_ne!(groups[0], groups[1]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn rescan_is_stable_and_prunes_stale_items() {
        let root = temp_root("rescan");
        let show = root.join("[Group] Old Title");
        touch(&show, "01.mkv");

        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        assert_eq!(
            scanner.scan(&root, &repo, &noop()).expect("first").created,
            1
        );
        assert_eq!(
            scanner
                .scan(&root, &repo, &noop())
                .expect("second")
                .unchanged,
            1
        );

        let renamed = root.join("[Group] New Title");
        fs::rename(&show, &renamed).expect("rename");
        let report = scanner.scan(&root, &repo, &noop()).expect("third");
        assert_eq!(report.created, 1, "路径变化视为新条目");
        assert_eq!(
            repo.count_items(Some(ItemKind::Anime)).expect("count"),
            1,
            "旧路径的条目应被清理，否则列表出现重复"
        );

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probe_reports_episodes() {
        let root = temp_root("probe");
        let show = root.join("Show");
        touch(&show, "Show - 01.mkv");
        touch(&show, "Show - 02.mkv");
        touch(&show, "cover.jpg");

        let scanner = AnimeScanner::new().expect("scanner");
        let probe = scanner.probe(&show, None).expect("probe");
        assert_eq!(probe.file_count, 2);
        assert_eq!(probe.episode_hint.as_deref(), Some("E01–E02"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_missing_root_fails_cleanly() {
        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        let missing = std::env::temp_dir().join(format!("lagm-missing-{}", uuid::Uuid::new_v4()));
        assert!(scanner.scan(&missing, &repo, &noop()).is_err());
    }

    #[test]
    fn upsert_outcome_created_matches_db_state() {
        let root = temp_root("outcome");
        touch(&root.join("A"), "01.mkv");
        let repo = repo();
        let scanner = AnimeScanner::new().expect("scanner");
        scanner.scan(&root, &repo, &noop()).expect("scan");
        let item = repo.list_items(None).expect("list").remove(0);
        let outcome = repo
            .upsert_scanned(&NewItem {
                kind: ItemKind::Anime,
                title: item.title.clone(),
                folder_path: item.folder_path.clone(),
                file_filter: item.file_filter.clone(),
            })
            .expect("upsert");
        assert!(matches!(outcome, UpsertOutcome::Unchanged(_)));
        fs::remove_dir_all(root).ok();
    }
}
