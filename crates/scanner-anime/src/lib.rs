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
        self.probe_with_hint(folder, filter, None)
    }

    /// 带「第一季真实集数」提示的探测。
    ///
    /// `first_season_eps` 来自绑定条目在 Bangumi 上的总集数，只用于修正
    /// 「前季磁盘缺集 → 后季偏移不足」这一种偏差；拿不到就不要传。
    pub fn probe_with_hint(
        &self,
        folder: &Path,
        filter: Option<&str>,
        first_season_eps: Option<u32>,
    ) -> Result<MediaProbe> {
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

        let entries: Vec<(MediaFile, Option<u32>)> = collected
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
                let special = media::is_special_path(&file.relative);
                // 附属内容自成一块编号，季号对它没有意义。
                let season = if special {
                    None
                } else {
                    self.parser.parse_season_in_path(&file.relative)
                };
                let media_file = MediaFile {
                    // 附属目录的文件名常自带编号（`SP 01`、`NCOP 02`），采信会与正片撞号，
                    // 交给下面的顺延编号统一处理。
                    episode: if special {
                        None
                    } else {
                        self.parser.parse_episode(&bare_name)
                    },
                    name: file.relative,
                    size_bytes: file.size_bytes,
                    group,
                    special,
                };
                (media_file, season)
            })
            .collect();
        let (mut files, seasons): (Vec<MediaFile>, Vec<Option<u32>>) = entries.into_iter().unzip();

        // 多季合并成一个条目时，先把各季集号挪到互不重叠的区间，否则 S01E05 与 S02E05 会写同一行进度。
        apply_season_offsets(&mut files, &seasons, first_season_eps);

        // 正片整块在前（按来源分组、组内按集号），附属内容整块在后（按来源分组、组内按文件名自然序）。
        // 顺序即顺延编号的顺序，因此附属文件永远拿不到正片的集号。
        normalize_groups(&mut files);
        files.sort_by(|left, right| {
            left.special
                .cmp(&right.special)
                .then_with(|| left.group.is_some().cmp(&right.group.is_some()))
                .then_with(|| left.group.cmp(&right.group))
                .then_with(|| left.episode.is_none().cmp(&right.episode.is_none()))
                .then_with(|| left.episode.cmp(&right.episode))
                .then_with(|| media::natural_cmp(&left.name, &right.name))
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

/// 多季合并到一个条目时，按季把集号挪到互不重叠的区间。
///
/// 偏移量取「前面各季已出现的最大集号之和」：分季目录里的集号通常是完整的 1..N，
/// 所以第一季 12 集时第二季从 13 起。判据只有文件名/目录名里的季标记，
/// 拿不到季信息时（`seasons` 全为 None 或只有一季）不做任何改动。
///
/// 为什么不能只靠「同号去重」：`S01E05` 与 `S02E05` 谁是第 5 集没有正确答案，
/// 去重只能按文件名排序猜；而分季的集号本身是可靠的，按季偏移才不会被猜错。
/// 允许用 Bangumi 集数覆盖磁盘最大集号的最大差值（约一季）。
/// 超过这个差值就认为拿到的是系列级集数，不能当单季基准。
const MISSING_EPISODE_TOLERANCE: u32 = 12;

fn apply_season_offsets(
    files: &mut [MediaFile],
    seasons: &[Option<u32>],
    first_season_eps: Option<u32>,
) {
    let mut max_by_season: BTreeMap<u32, u32> = BTreeMap::new();
    for (file, season) in files.iter().zip(seasons.iter()) {
        if file.special {
            continue;
        }
        let Some(episode) = file.episode else {
            continue;
        };
        // 没有季标记的文件归入第 1 季：`Show/01.mkv` 与 `Show/Season 2/01.mkv` 这种混排很常见。
        let slot = max_by_season.entry(season.unwrap_or(1)).or_insert(0);
        *slot = (*slot).max(episode);
    }
    if max_by_season.len() < 2 {
        return;
    }

    // 半修：磁盘上的第一季可能缺集，导致后面各季整体偏移不足（S02E01 会被编成第 11 集
    // 而不是第 13 集，云端进度就会写到 S01E11 上）。绑定条目的总集数更接近真实值。
    //
    // 但只信任「比磁盘略多」的情形：如果这个数字比磁盘多出一整季以上，多半是系列级条目
    // （eps 覆盖全部季），拿它当单季基准会把第二季推得更离谱。
    if let (Some(hint), Some((&first_season, &disk_max))) =
        (first_season_eps, max_by_season.iter().next())
    {
        let gap = hint.saturating_sub(disk_max);
        if gap > 0 && gap <= MISSING_EPISODE_TOLERANCE {
            tracing::debug!(first_season, disk_max, hint, "第一季集数按 Bangumi 修正");
            max_by_season.insert(first_season, hint);
        } else if gap > MISSING_EPISODE_TOLERANCE {
            tracing::debug!(
                first_season,
                disk_max,
                hint,
                "Bangumi 集数明显大于磁盘，按系列级条目处理，忽略"
            );
        }
    }

    let mut offsets: BTreeMap<u32, u32> = BTreeMap::new();
    let mut offset = 0_u32;
    for (season, max_episode) in &max_by_season {
        offsets.insert(*season, offset);
        offset = offset.saturating_add(*max_episode);
    }

    for (file, season) in files.iter_mut().zip(seasons.iter()) {
        if file.special {
            continue;
        }
        let Some(episode) = file.episode else {
            continue;
        };
        let Some(offset) = offsets.get(&season.unwrap_or(1)) else {
            continue;
        };
        file.episode = Some(episode.saturating_add(*offset));
    }
}

/// 给没有集号的文件顺延编号，保证一次探测内集号唯一。
///
/// 调用前文件已按「正片在前、附属内容在后」排好，因此正片里解析不出集号的
/// 紧接正片最大集号，附属内容再往后排，两块编号不会互相侵占。
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
    fn probe_places_unparsed_files_after_episodes() {
        let root = temp_root("probe-unparsed");
        let show = root.join("Show");
        touch(&show, "Show - 02.mkv");
        touch(&show, "Show - 01.mkv");
        touch(&show, "Show - Preview.mkv");
        touch(&show, "Show - Menu.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        let probe = scanner.probe(&show, None).expect("probe");
        let names: Vec<&str> = probe.files.iter().map(|file| file.name.as_str()).collect();
        // 无集号的文件必须排在正片之后，且彼此按文件名自然序；集号顺延到最大集号之后。
        assert_eq!(
            names,
            [
                "Show - 01.mkv",
                "Show - 02.mkv",
                "Show - Menu.mkv",
                "Show - Preview.mkv"
            ]
        );
        let episodes: Vec<Option<u32>> = probe.files.iter().map(|file| file.episode).collect();
        assert_eq!(episodes, [Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(probe.episode_hint.as_deref(), Some("E01–E02"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probe_numbers_specials_after_main_episodes() {
        let root = temp_root("probe-specials");
        let show = root.join("Show");
        touch(&show.join("Season 1"), "Show S01E01.mkv");
        touch(&show.join("Season 1"), "Show S01E02.mkv");
        touch(&show.join("Extras"), "Show 01.mkv");
        touch(&show.join("Extras"), "Show 02.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        let probe = scanner.probe(&show, None).expect("probe");

        // Extras 字典序在 Season 1 之前，但附属内容必须整块排在正片之后。
        let names: Vec<&str> = probe.files.iter().map(|file| file.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Season 1/Show S01E01.mkv",
                "Season 1/Show S01E02.mkv",
                "Extras/Show 01.mkv",
                "Extras/Show 02.mkv"
            ]
        );
        // Extras 里的 01/02 若采信文件名就会与正片 1/2 撞号，这里必须顺延成 3/4。
        let episodes: Vec<Option<u32>> = probe.files.iter().map(|file| file.episode).collect();
        assert_eq!(episodes, [Some(1), Some(2), Some(3), Some(4)]);
        let specials: Vec<bool> = probe.files.iter().map(|file| file.special).collect();
        assert_eq!(specials, [false, false, true, true]);
        assert_eq!(probe.episode_hint.as_deref(), Some("E01–E02"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probe_offsets_episodes_across_seasons() {
        let root = temp_root("probe-seasons");
        let show = root.join("Show");
        touch(&show.join("Season 1"), "Show S01E01.mkv");
        touch(&show.join("Season 1"), "Show S01E02.mkv");
        touch(&show.join("Season 2"), "Show S02E01.mkv");
        touch(&show.join("Season 2"), "Show S02E02.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        let probe = scanner.probe(&show, None).expect("probe");

        // 两季合并成一个条目时，第二季必须从第 3 集起，否则 S01E05 与 S02E05 会共用一行进度。
        let episodes: Vec<Option<u32>> = probe.files.iter().map(|file| file.episode).collect();
        assert_eq!(episodes, [Some(1), Some(2), Some(3), Some(4)]);
        assert_eq!(probe.episode_hint.as_deref(), Some("E01–E04"));

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probe_uses_bangumi_episode_count_when_first_season_is_incomplete() {
        let root = temp_root("probe-seasons-gap");
        let show = root.join("Show");
        // 第一季磁盘上只有 10 集，真实是 12 集。
        for episode in 1..=10 {
            touch(
                &show.join("Season 1"),
                &format!("Show S01E{episode:02}.mkv"),
            );
        }
        touch(&show.join("Season 2"), "Show S02E01.mkv");
        touch(&show.join("Season 2"), "Show S02E02.mkv");

        let scanner = AnimeScanner::new().expect("scanner");

        // 没有提示时只能按磁盘推断：第二季从第 11 集起（偏移不足）。
        let plain = scanner.probe(&show, None).expect("probe");
        let tail: Vec<u32> = plain
            .files
            .iter()
            .filter_map(|file| file.episode)
            .skip(10)
            .collect();
        assert_eq!(tail, [11, 12]);

        // 给出 Bangumi 的总集数 12 后，第二季应落到第 13 集起。
        let fixed = scanner
            .probe_with_hint(&show, None, Some(12))
            .expect("probe with hint");
        let tail: Vec<u32> = fixed
            .files
            .iter()
            .filter_map(|file| file.episode)
            .skip(10)
            .collect();
        assert_eq!(tail, [13, 14]);

        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn probe_ignores_series_level_episode_count() {
        let root = temp_root("probe-seasons-series-eps");
        let show = root.join("Show");
        for episode in 1..=10 {
            touch(
                &show.join("Season 1"),
                &format!("Show S01E{episode:02}.mkv"),
            );
        }
        touch(&show.join("Season 2"), "Show S02E01.mkv");

        let scanner = AnimeScanner::new().expect("scanner");
        // 26 比磁盘多出 16 集，明显是覆盖全系列的集数：不能拿来当第一季基准，
        // 否则第二季会被推到第 27 集。
        let probe = scanner
            .probe_with_hint(&show, None, Some(26))
            .expect("probe");
        let last = probe
            .files
            .iter()
            .filter_map(|file| file.episode)
            .next_back();
        assert_eq!(last, Some(11));

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
