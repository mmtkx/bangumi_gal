use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{CoreError, Result};
use crate::model::LibraryItem;
use crate::repo::{map_item, SqliteRepository, ITEM_COLUMNS};
use crate::time::now_iso8601;

/// 分类总数上限（含所有层级）。
///
/// 上限只用来挡住误操作（脚本刷出的几千个分类会把界面拖垮），
/// 不再是「一般用户用不到这么多」的产品约束，因此给到 10000。
pub const MAX_CATEGORIES: i64 = 10_000;

/// 分类树最大层级，根分类算第 1 层。
pub const MAX_CATEGORY_DEPTH: i64 = 5;

/// 分类名称长度上限，防止树节点被长名字撑破。
const MAX_NAME_CHARS: usize = 24;

/// 一条用户自定义分类。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Category {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
    /// 封面来源：`title`（黑底名称）/ `image`（本地图片副本）/ `carousel`（轮播子树条目封面）。
    pub cover_kind: String,
    /// 仅 `cover_kind = image` 时有值，指向应用数据目录里的副本。
    pub cover_path: Option<String>,
}

/// 封面来源的合法取值。
pub const COVER_TITLE: &str = "title";
pub const COVER_IMAGE: &str = "image";
pub const COVER_CAROUSEL: &str = "carousel";

/// 规范化封面来源，未知值按 `title` 处理（老数据默认值）。
pub fn parse_cover_kind(raw: &str) -> &'static str {
    match raw {
        "image" => COVER_IMAGE,
        "carousel" => COVER_CAROUSEL,
        _ => COVER_TITLE,
    }
}

const CATEGORY_COLUMNS: &str =
    "id, name, parent_id, sort_order, created_at, cover_kind, cover_path";

fn map_category(row: &rusqlite::Row<'_>) -> rusqlite::Result<Category> {
    Ok(Category {
        id: row.get(0)?,
        name: row.get(1)?,
        parent_id: row.get(2)?,
        sort_order: row.get(3)?,
        created_at: row.get(4)?,
        cover_kind: row.get(5)?,
        cover_path: row.get(6)?,
    })
}

/// 条目身份键：目录 + 分组键。
///
/// 与 scanner 的 `scope_key` 语义相同，但这里固定「始终带分隔符」，
/// 好让 SQL 侧直接用 `folder_path || '|' || IFNULL(file_filter, '')` 比较。
/// 用身份键而不是条目 id 记归属：扫描会 prune 重建条目，id 会变、路径不会。
pub fn item_key(folder_path: &str, file_filter: Option<&str>) -> String {
    format!("{folder_path}|{}", file_filter.unwrap_or(""))
}

/// 分类仓储。
///
/// 分类是纯本地的组织层，不参与 Bangumi 同步；条目与分类多对多，
/// 因此删分类只删组织关系，绝不碰条目与磁盘文件。
pub trait CategoryRepository: Send + Sync {
    fn list_categories(&self) -> Result<Vec<Category>>;
    fn category_by_id(&self, id: &str) -> Result<Option<Category>>;

    /// 新建分类；`parent_id` 为 None 表示根分类。总量与层级上限在这里校验。
    fn create_category(&self, name: &str, parent_id: Option<&str>) -> Result<Category>;
    fn rename_category(&self, id: &str, name: &str) -> Result<Category>;

    /// 移动分类（改父级与顺序）。拒绝成环，并保证移动后整棵子树仍不超过层级上限。
    fn move_category(
        &self,
        id: &str,
        parent_id: Option<&str>,
        sort_order: Option<i64>,
    ) -> Result<Category>;

    /// 删除分类；子分类与归属关系随外键级联删除，条目本身不受影响。
    fn delete_category(&self, id: &str) -> Result<()>;

    /// 与相邻的同级分类交换顺序。`delta` 为 -1 上移、1 下移，到头时什么也不做。
    fn swap_order(&self, id: &str, delta: i64) -> Result<()>;

    /// 设置分类卡封面。`path` 只在 `kind = image` 时使用，指向已复制到应用数据目录的副本。
    fn set_category_cover(&self, id: &str, kind: &str, path: Option<&str>) -> Result<Category>;

    /// 每个分类**直接**收录的条目 id（不递归）。轮播封面由调用方沿树累加。
    fn item_ids_by_category(&self) -> Result<HashMap<String, Vec<String>>>;

    /// 把若干条目挂到分类下，已存在的跳过。返回新增的归属条数。
    fn assign_items(&self, category_id: &str, item_keys: &[String]) -> Result<usize>;

    /// 从分类下移除若干条目。返回实际移除的条数。
    fn unassign_items(&self, category_id: &str, item_keys: &[String]) -> Result<usize>;

    /// 用给定集合整体替换某个条目的分类（详情页的勾选式编辑）。
    fn set_item_categories(&self, item_key: &str, category_ids: &[String]) -> Result<()>;

    fn category_ids_for_item(&self, item_key: &str) -> Result<Vec<String>>;

    /// 某个分类下的条目（按标题排序）。
    fn list_items_in_category(&self, category_id: &str) -> Result<Vec<LibraryItem>>;

    /// 每个分类直接挂了多少条目。
    fn counts_by_category(&self) -> Result<HashMap<String, i64>>;

    /// 清理指向已不存在条目的归属（换根目录、删目录之后会留下孤儿）。
    fn prune_links(&self) -> Result<usize>;
}

/// 总量校验抽成纯函数：测试不必真的插一万行。
fn check_capacity(total: i64) -> Result<()> {
    if total >= MAX_CATEGORIES {
        return Err(CoreError::InvalidCategory(format!(
            "分类最多 {MAX_CATEGORIES} 个，先删掉不用的再新建"
        )));
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CoreError::InvalidCategory("分类名称不能为空".to_owned()));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(CoreError::InvalidCategory(format!(
            "分类名称最多 {MAX_NAME_CHARS} 个字"
        )));
    }
    Ok(trimmed.to_owned())
}

/// 节点深度（根为 1）。节点不存在时返回 InvalidCategory。
fn depth_of(conn: &Connection, id: &str) -> Result<i64> {
    let mut current = Some(id.to_owned());
    let mut depth = 0_i64;
    while let Some(node) = current {
        if depth > MAX_CATEGORIES {
            return Err(CoreError::InvalidCategory("分类层级异常".to_owned()));
        }
        let row: Option<Option<String>> = conn
            .query_row(
                "SELECT parent_id FROM category WHERE id = ?1",
                [&node],
                |row| row.get(0),
            )
            .optional()?;
        let Some(parent) = row else {
            return Err(CoreError::InvalidCategory(format!("分类不存在：{id}")));
        };
        depth += 1;
        current = parent;
    }
    Ok(depth)
}

/// 以该节点为根的子树高度（节点数为 1 表示叶子）。
fn height_of(conn: &Connection, id: &str) -> Result<i64> {
    let height: i64 = conn.query_row(
        "WITH RECURSIVE sub(id, lvl) AS (
             SELECT id, 1 FROM category WHERE id = ?1
             UNION ALL
             SELECT c.id, sub.lvl + 1 FROM category c JOIN sub ON c.parent_id = sub.id
         )
         SELECT IFNULL(MAX(lvl), 0) FROM sub",
        [id],
        |row| row.get(0),
    )?;
    Ok(height)
}

/// `candidate` 是否落在 `id` 的子树里（用于拒绝把父节点移到自己孩子下面）。
fn is_descendant(conn: &Connection, id: &str, candidate: &str) -> Result<bool> {
    let found: i64 = conn.query_row(
        "WITH RECURSIVE sub(id) AS (
             SELECT id FROM category WHERE id = ?1
             UNION ALL
             SELECT c.id FROM category c JOIN sub ON c.parent_id = sub.id
         )
         SELECT EXISTS(SELECT 1 FROM sub WHERE id = ?2)",
        params![id, candidate],
        |row| row.get(0),
    )?;
    Ok(found == 1)
}

fn sibling_name_taken(
    conn: &Connection,
    parent_id: Option<&str>,
    name: &str,
    exclude: Option<&str>,
) -> Result<bool> {
    let taken: i64 = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM category
             WHERE IFNULL(parent_id, '') = ?1 AND name = ?2 AND IFNULL(id, '') <> ?3
         )",
        params![parent_id.unwrap_or(""), name, exclude.unwrap_or("")],
        |row| row.get(0),
    )?;
    Ok(taken == 1)
}

fn next_sort_order(conn: &Connection, parent_id: Option<&str>) -> Result<i64> {
    let next: i64 = conn.query_row(
        "SELECT IFNULL(MAX(sort_order), -1) + 1 FROM category WHERE IFNULL(parent_id, '') = ?1",
        [parent_id.unwrap_or("")],
        |row| row.get(0),
    )?;
    Ok(next)
}

fn require_category(conn: &Connection, id: &str) -> Result<Category> {
    conn.query_row(
        &format!("SELECT {CATEGORY_COLUMNS} FROM category WHERE id = ?1"),
        [id],
        map_category,
    )
    .optional()?
    .ok_or_else(|| CoreError::InvalidCategory(format!("分类不存在：{id}")))
}

impl CategoryRepository for SqliteRepository {
    fn list_categories(&self) -> Result<Vec<Category>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {CATEGORY_COLUMNS} FROM category ORDER BY sort_order, name"
            ))?;
            let rows = stmt.query_map([], map_category)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    fn category_by_id(&self, id: &str) -> Result<Option<Category>> {
        self.db.with_conn(|conn| {
            conn.query_row(
                &format!("SELECT {CATEGORY_COLUMNS} FROM category WHERE id = ?1"),
                [id],
                map_category,
            )
            .optional()
            .map_err(CoreError::from)
        })
    }

    fn create_category(&self, name: &str, parent_id: Option<&str>) -> Result<Category> {
        let name = validate_name(name)?;
        self.db.with_conn(|conn| {
            let total: i64 =
                conn.query_row("SELECT COUNT(*) FROM category", [], |row| row.get(0))?;
            check_capacity(total)?;
            if let Some(parent) = parent_id {
                let depth = depth_of(conn, parent)?;
                if depth + 1 > MAX_CATEGORY_DEPTH {
                    return Err(CoreError::InvalidCategory(format!(
                        "分类最多嵌套 {MAX_CATEGORY_DEPTH} 层"
                    )));
                }
            }
            if sibling_name_taken(conn, parent_id, &name, None)? {
                return Err(CoreError::InvalidCategory(format!(
                    "同一层级下已经有「{name}」"
                )));
            }
            let id = uuid::Uuid::new_v4().to_string();
            let created = now_iso8601();
            let sort_order = next_sort_order(conn, parent_id)?;
            conn.execute(
                "INSERT INTO category (id, name, parent_id, sort_order, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, name, parent_id, sort_order, created],
            )?;
            Ok(Category {
                id,
                name,
                parent_id: parent_id.map(str::to_owned),
                sort_order,
                created_at: created,
                cover_kind: COVER_TITLE.to_owned(),
                cover_path: None,
            })
        })
    }

    fn rename_category(&self, id: &str, name: &str) -> Result<Category> {
        let name = validate_name(name)?;
        self.db.with_conn(|conn| {
            let current = require_category(conn, id)?;
            if sibling_name_taken(conn, current.parent_id.as_deref(), &name, Some(id))? {
                return Err(CoreError::InvalidCategory(format!(
                    "同一层级下已经有「{name}」"
                )));
            }
            conn.execute(
                "UPDATE category SET name = ?2 WHERE id = ?1",
                params![id, name],
            )?;
            require_category(conn, id)
        })
    }

    fn move_category(
        &self,
        id: &str,
        parent_id: Option<&str>,
        sort_order: Option<i64>,
    ) -> Result<Category> {
        self.db.with_conn(|conn| {
            let current = require_category(conn, id)?;
            if parent_id == Some(id) {
                return Err(CoreError::InvalidCategory(
                    "不能把分类移到它自己下面".to_owned(),
                ));
            }
            if let Some(parent) = parent_id {
                if is_descendant(conn, id, parent)? {
                    return Err(CoreError::InvalidCategory(
                        "不能把分类移到它自己的子分类下".to_owned(),
                    ));
                }
                let parent_depth = depth_of(conn, parent)?;
                let height = height_of(conn, id)?;
                if parent_depth + height > MAX_CATEGORY_DEPTH {
                    return Err(CoreError::InvalidCategory(format!(
                        "移动后层级会超过 {MAX_CATEGORY_DEPTH} 层"
                    )));
                }
            }
            if sibling_name_taken(conn, parent_id, &current.name, Some(id))? {
                return Err(CoreError::InvalidCategory(format!(
                    "目标层级下已经有「{}」",
                    current.name
                )));
            }
            let order = match sort_order {
                Some(value) => value,
                // 未指定时排到新兄弟的最后，避免和已有节点抢同一个位置。
                None => next_sort_order(conn, parent_id)?,
            };
            conn.execute(
                "UPDATE category SET parent_id = ?2, sort_order = ?3 WHERE id = ?1",
                params![id, parent_id, order],
            )?;
            require_category(conn, id)
        })
    }

    fn delete_category(&self, id: &str) -> Result<()> {
        self.db.with_conn(|conn| {
            let affected = conn.execute("DELETE FROM category WHERE id = ?1", [id])?;
            if affected == 0 {
                return Err(CoreError::InvalidCategory(format!("分类不存在：{id}")));
            }
            Ok(())
        })
    }

    fn swap_order(&self, id: &str, delta: i64) -> Result<()> {
        if delta == 0 {
            return Ok(());
        }
        self.db.with_conn(|conn| {
            let current = require_category(conn, id)?;
            let parent = current.parent_id.as_deref().unwrap_or("");
            // 取排序方向上的最近一个同级节点。sort_order 理论上唯一（新建时取 max+1），
            // 但仍用 id 兜底比较，避免手工写入造成的并列让「上移」不动。
            let neighbor: Option<(String, i64)> = if delta < 0 {
                conn.query_row(
                    "SELECT id, sort_order FROM category
                     WHERE IFNULL(parent_id, '') = ?1 AND id <> ?2
                       AND (sort_order < ?3 OR (sort_order = ?3 AND id < ?2))
                     ORDER BY sort_order DESC, id DESC LIMIT 1",
                    params![parent, id, current.sort_order],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
            } else {
                conn.query_row(
                    "SELECT id, sort_order FROM category
                     WHERE IFNULL(parent_id, '') = ?1 AND id <> ?2
                       AND (sort_order > ?3 OR (sort_order = ?3 AND id > ?2))
                     ORDER BY sort_order ASC, id ASC LIMIT 1",
                    params![parent, id, current.sort_order],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
            };
            let Some((neighbor_id, neighbor_order)) = neighbor else {
                return Ok(());
            };
            let tx = conn.transaction()?;
            tx.execute(
                "UPDATE category SET sort_order = ?2 WHERE id = ?1",
                params![id, neighbor_order],
            )?;
            tx.execute(
                "UPDATE category SET sort_order = ?2 WHERE id = ?1",
                params![neighbor_id, current.sort_order],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    fn set_category_cover(&self, id: &str, kind: &str, path: Option<&str>) -> Result<Category> {
        let kind = parse_cover_kind(kind);
        if kind == COVER_IMAGE && path.is_none_or(|value| value.trim().is_empty()) {
            return Err(CoreError::InvalidCategory(
                "选择一张图片后才能使用自定义封面".to_owned(),
            ));
        }
        let stored = if kind == COVER_IMAGE { path } else { None };
        self.db.with_conn(|conn| {
            require_category(conn, id)?;
            conn.execute(
                "UPDATE category SET cover_kind = ?2, cover_path = ?3 WHERE id = ?1",
                params![id, kind, stored],
            )?;
            require_category(conn, id)
        })
    }

    fn item_ids_by_category(&self) -> Result<HashMap<String, Vec<String>>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT ic.category_id, li.id
                 FROM item_category ic
                 JOIN library_item li
                   ON (li.folder_path || '|' || IFNULL(li.file_filter, '')) = ic.item_key
                 ORDER BY ic.category_id, li.title",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out: HashMap<String, Vec<String>> = HashMap::new();
            for row in rows {
                let (category_id, item_id) = row?;
                out.entry(category_id).or_default().push(item_id);
            }
            Ok(out)
        })
    }

    fn assign_items(&self, category_id: &str, item_keys: &[String]) -> Result<usize> {
        self.db.with_conn(|conn| {
            require_category(conn, category_id)?;
            let now = now_iso8601();
            let mut stmt = conn.prepare(
                "INSERT OR IGNORE INTO item_category (category_id, item_key, assigned_at)
                 VALUES (?1, ?2, ?3)",
            )?;
            let mut added = 0;
            for key in item_keys {
                added += stmt.execute(params![category_id, key, now])?;
            }
            Ok(added)
        })
    }

    fn unassign_items(&self, category_id: &str, item_keys: &[String]) -> Result<usize> {
        self.db.with_conn(|conn| {
            let mut stmt =
                conn.prepare("DELETE FROM item_category WHERE category_id = ?1 AND item_key = ?2")?;
            let mut removed = 0;
            for key in item_keys {
                removed += stmt.execute(params![category_id, key])?;
            }
            Ok(removed)
        })
    }

    fn set_item_categories(&self, item_key: &str, category_ids: &[String]) -> Result<()> {
        self.db.with_conn(|conn| {
            let tx = conn.transaction()?;
            tx.execute("DELETE FROM item_category WHERE item_key = ?1", [item_key])?;
            let now = now_iso8601();
            {
                let mut stmt = tx.prepare(
                    "INSERT OR IGNORE INTO item_category (category_id, item_key, assigned_at)
                     VALUES (?1, ?2, ?3)",
                )?;
                for id in category_ids {
                    // 先确认分类存在：外键报错信息对用户没有意义。
                    require_category(&tx, id)?;
                    stmt.execute(params![id, item_key, now])?;
                }
            }
            tx.commit()?;
            Ok(())
        })
    }

    fn category_ids_for_item(&self, item_key: &str) -> Result<Vec<String>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(
                "SELECT category_id FROM item_category WHERE item_key = ?1 ORDER BY category_id",
            )?;
            let rows = stmt.query_map([item_key], |row| row.get::<_, String>(0))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    fn list_items_in_category(&self, category_id: &str) -> Result<Vec<LibraryItem>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn.prepare(&format!(
                "SELECT {ITEM_COLUMNS} FROM library_item
                 WHERE (folder_path || '|' || IFNULL(file_filter, '')) IN
                       (SELECT item_key FROM item_category WHERE category_id = ?1)
                 ORDER BY title"
            ))?;
            let rows = stmt.query_map([category_id], map_item)?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    fn counts_by_category(&self) -> Result<HashMap<String, i64>> {
        self.db.with_conn(|conn| {
            let mut stmt = conn
                .prepare("SELECT category_id, COUNT(*) FROM item_category GROUP BY category_id")?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            let mut out = HashMap::new();
            for row in rows {
                let (id, count) = row?;
                out.insert(id, count);
            }
            Ok(out)
        })
    }

    fn prune_links(&self) -> Result<usize> {
        self.db.with_conn(|conn| {
            let removed = conn.execute(
                "DELETE FROM item_category WHERE item_key NOT IN (
                     SELECT folder_path || '|' || IFNULL(file_filter, '') FROM library_item
                 )",
                [],
            )?;
            Ok(removed)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::model::{ItemKind, NewItem, UpsertOutcome};
    use crate::repo::LibraryRepository;
    use std::sync::Arc;

    fn repo() -> SqliteRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteRepository::new(Arc::new(db))
    }

    fn add_item(repo: &SqliteRepository, folder: &str, filter: Option<&str>) {
        let outcome = repo
            .upsert_scanned(&NewItem {
                kind: ItemKind::Anime,
                title: "Show".to_owned(),
                folder_path: folder.to_owned(),
                file_filter: filter.map(str::to_owned),
            })
            .expect("upsert");
        assert!(matches!(
            outcome,
            UpsertOutcome::Created(_) | UpsertOutcome::Updated(_) | UpsertOutcome::Unchanged(_)
        ));
    }

    #[test]
    fn category_count_and_depth_limits_are_enforced() {
        let repo = repo();
        // 前 5 层可以一路建下去，第 6 层必须被拒。
        let mut parent: Option<String> = None;
        for level in 1..=MAX_CATEGORY_DEPTH {
            let created = repo
                .create_category(&format!("L{level}"), parent.as_deref())
                .expect("create");
            parent = Some(created.id);
        }
        assert!(
            repo.create_category("L6", parent.as_deref()).is_err(),
            "第 6 层应被拒绝"
        );

        // 总量上限只测边界：真的插一万行没有意义，也拖慢测试。
        assert!(check_capacity(MAX_CATEGORIES - 1).is_ok());
        assert!(check_capacity(MAX_CATEGORIES).is_err());
    }

    #[test]
    fn links_follow_item_identity_and_survive_rescan() {
        let repo = repo();
        let category = repo.create_category("10月新番", None).expect("create");
        let folder = "D:/anime/[VCB] K-ON!/[VCB] K-ON! [Ma10p]";
        let filter = Some("dirs:[VCB] K-ON! [Ma10p]");
        add_item(&repo, folder, filter);

        let key = item_key(folder, filter);
        repo.assign_items(&category.id, std::slice::from_ref(&key))
            .expect("assign");
        assert_eq!(
            repo.list_items_in_category(&category.id)
                .expect("items")
                .len(),
            1
        );

        // 重扫（同路径同分组键）会复用同一行，归属仍然有效。
        add_item(&repo, folder, filter);
        assert_eq!(
            repo.list_items_in_category(&category.id)
                .expect("items")
                .len(),
            1
        );

        // 条目消失后归属变成孤儿，prune 之后应被清掉。
        repo.delete_all_items(None).expect("clear");
        assert_eq!(repo.prune_links().expect("prune"), 1);
        assert_eq!(repo.counts_by_category().expect("counts").len(), 0);
    }

    #[test]
    fn siblings_can_be_reordered() {
        let repo = repo();
        repo.create_category("a", None).expect("a");
        let b = repo.create_category("b", None).expect("b");

        repo.swap_order(&b.id, -1).expect("up");
        let names: Vec<String> = repo
            .list_categories()
            .expect("list")
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(names, vec!["b".to_owned(), "a".to_owned()]);

        // 已经是最上面，再上移应当什么都不做而不是报错。
        repo.swap_order(&b.id, -1).expect("noop");
        let names: Vec<String> = repo
            .list_categories()
            .expect("list")
            .into_iter()
            .map(|row| row.name)
            .collect();
        assert_eq!(names, vec!["b".to_owned(), "a".to_owned()]);
    }

    #[test]
    fn moving_into_own_subtree_is_rejected() {
        let repo = repo();
        let root = repo.create_category("root", None).expect("root");
        let child = repo
            .create_category("child", Some(&root.id))
            .expect("child");
        assert!(repo.move_category(&root.id, Some(&child.id), None).is_err());
        assert!(repo.move_category(&root.id, Some(&root.id), None).is_err());
    }
}
