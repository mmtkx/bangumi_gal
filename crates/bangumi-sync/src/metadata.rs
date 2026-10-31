use std::path::{Path, PathBuf};

use library_core::MetadataIndexRepository;

use crate::client::BangumiClient;
use crate::error::{SyncError, SyncResult};

/// 元数据缓存：按条目分目录写入可配置根目录（ADR-009）。
///
/// 目录结构：`<root>/<subject_id>/subject.json`。写入先落临时文件再重命名，
/// 避免进程中断留下半个 JSON。索引写入 `metadata_index` 表。
pub struct MetadataCache {
    root: PathBuf,
}

impl MetadataCache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 条目缓存目录。
    pub fn subject_dir(&self, subject_id: i64) -> PathBuf {
        self.root.join(subject_id.to_string())
    }

    /// 条目元数据文件路径。
    pub fn subject_file(&self, subject_id: i64) -> PathBuf {
        self.subject_dir(subject_id).join("subject.json")
    }

    /// 封面缓存文件（原始字节，不转码；扩展名统一为 `.img`）。
    pub fn cover_file(&self, subject_id: i64) -> PathBuf {
        self.subject_dir(subject_id).join("cover.img")
    }

    /// 确保封面已缓存：已存在直接返回；否则按元数据里的图片地址下载一次。
    ///
    /// 元数据尚未缓存、或响应里没有可用图片时返回 `Ok(None)`，由调用方决定是否提示。
    pub async fn ensure_cover(
        &self,
        client: &dyn BangumiClient,
        subject_id: i64,
    ) -> SyncResult<Option<PathBuf>> {
        let file = self.cover_file(subject_id);
        if file.is_file()
            && std::fs::metadata(&file)
                .map(|m| m.len() > 0)
                .unwrap_or(false)
        {
            return Ok(Some(file));
        }
        let meta_path = self.subject_file(subject_id);
        let Ok(raw) = std::fs::read(&meta_path) else {
            return Ok(None);
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&raw) else {
            return Ok(None);
        };
        let Some(url) = cover_url(&value) else {
            return Ok(None);
        };
        let bytes = client.fetch_bytes(url).await?;
        if bytes.is_empty() {
            return Ok(None);
        }
        let dir = self.subject_dir(subject_id);
        std::fs::create_dir_all(&dir).map_err(|err| {
            SyncError::Internal(format!("创建封面目录失败：{}（{err}）", dir.display()))
        })?;
        std::fs::write(&file, &bytes).map_err(|err| {
            SyncError::Internal(format!("写入封面失败：{}（{err}）", file.display()))
        })?;
        tracing::info!(subject = subject_id, bytes = bytes.len(), "封面已缓存");
        Ok(Some(file))
    }

    /// 抓取并写入元数据，更新索引，返回文件路径。
    pub async fn refresh(
        &self,
        client: &dyn BangumiClient,
        index: &dyn MetadataIndexRepository,
        subject_id: i64,
    ) -> SyncResult<PathBuf> {
        let metadata = client.fetch_subject_metadata(subject_id).await?;
        let dir = self.subject_dir(subject_id);
        std::fs::create_dir_all(&dir).map_err(|err| {
            SyncError::Internal(format!("创建缓存目录失败：{}（{err}）", dir.display()))
        })?;

        let file = dir.join("subject.json");
        let temp = dir.join("subject.json.tmp");
        let body = serde_json::to_vec_pretty(&metadata)
            .map_err(|err| SyncError::Payload(err.to_string()))?;
        std::fs::write(&temp, body).map_err(|err| {
            SyncError::Internal(format!("写入缓存失败：{}（{err}）", temp.display()))
        })?;
        std::fs::rename(&temp, &file).map_err(|err| {
            SyncError::Internal(format!("提交缓存失败：{}（{err}）", file.display()))
        })?;

        let path = file.to_string_lossy().to_string();
        index.upsert(subject_id, &path)?;
        tracing::info!(subject = subject_id, path = %path, "元数据已缓存");
        Ok(file)
    }
}

/// 从 subject 元数据里挑一个可用的封面地址（large → common → medium → small → grid）。
///
/// Bangumi 的图片地址可能是协议相对形式（`//lain.bgm.tv/...`），补成 https 才能下载。
pub fn cover_url(value: &serde_json::Value) -> Option<String> {
    let images = value.get("images")?;
    for key in ["large", "common", "medium", "small", "grid"] {
        let Some(raw) = images.get(key).and_then(serde_json::Value::as_str) else {
            continue;
        };
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        return Some(if let Some(rest) = trimmed.strip_prefix("//") {
            format!("https://{rest}")
        } else {
            trimmed.to_owned()
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_core::{Db, SqliteMetadataIndexRepository};
    use std::sync::Arc;

    use crate::client::OfflineClient;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lagm-meta-{}-{}",
            name,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        ));
        dir
    }

    #[test]
    fn cover_url_prefers_large_and_fixes_protocol_relative() {
        let value = serde_json::json!({
            "images": { "large": "//lain.bgm.tv/pic/cover/l/1.jpg", "small": "https://x/s.jpg" }
        });
        assert_eq!(
            cover_url(&value).as_deref(),
            Some("https://lain.bgm.tv/pic/cover/l/1.jpg")
        );
        assert_eq!(cover_url(&serde_json::json!({})), None);
    }

    fn index() -> SqliteMetadataIndexRepository {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        SqliteMetadataIndexRepository::new(Arc::new(db))
    }

    #[tokio::test]
    async fn refresh_writes_per_subject_file_and_index() {
        let root = temp_root("basic");
        let cache = MetadataCache::new(&root);
        let index = index();
        let client = OfflineClient::new(true).with_subject_metadata(
            400602,
            serde_json::json!({ "id": 400602, "name": "テスト" }),
        );

        let path = cache
            .refresh(&client, &index, 400602)
            .await
            .expect("refresh");
        assert!(path.is_file());
        assert_eq!(path, cache.subject_file(400602));
        assert!(cache.subject_dir(400602).is_dir());

        let row = index.get(400602).expect("get").expect("indexed");
        assert_eq!(row.cached_path, path.to_string_lossy());

        let body = std::fs::read_to_string(&path).expect("read");
        assert!(body.contains("テスト"));

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn refresh_is_idempotent_and_overwrites() {
        let root = temp_root("overwrite");
        let cache = MetadataCache::new(&root);
        let index = index();
        let client = OfflineClient::new(true)
            .with_subject_metadata(1, serde_json::json!({ "id": 1, "name": "first" }));
        cache.refresh(&client, &index, 1).await.expect("first");

        let client2 = OfflineClient::new(true)
            .with_subject_metadata(1, serde_json::json!({ "id": 1, "name": "second" }));
        cache.refresh(&client2, &index, 1).await.expect("second");

        let body = std::fs::read_to_string(cache.subject_file(1)).expect("read");
        assert!(body.contains("second"));
        assert!(!body.contains("first"));
        assert_eq!(index.count().expect("count"), 1);

        std::fs::remove_dir_all(root).ok();
    }
}
