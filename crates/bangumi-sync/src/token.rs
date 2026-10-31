use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::{SyncError, SyncResult};

/// OAuth 令牌集合。plan.md 1.3：只存 token，不存账号密码。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenSet {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// RFC3339 UTC，过期时间；None 表示未知。
    pub expires_at: Option<String>,
}

/// 令牌存储抽象。
///
/// 生产实现走 Windows 凭据管理器（keyring），骨架阶段先用内存实现，
/// 保证同步引擎可测试且不触碰系统凭据。接入 OAuth 时新增 `KeyringTokenStore` 即可。
pub trait TokenStore: Send + Sync {
    fn load(&self) -> SyncResult<Option<TokenSet>>;
    fn save(&self, token: TokenSet) -> SyncResult<()>;
    fn clear(&self) -> SyncResult<()>;
}

/// 内存令牌存储，供离线骨架与单元测试使用。
#[derive(Default)]
pub struct InMemoryTokenStore {
    inner: Mutex<Option<TokenSet>>,
}

impl InMemoryTokenStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_token(token: TokenSet) -> Self {
        Self {
            inner: Mutex::new(Some(token)),
        }
    }
}

impl TokenStore for InMemoryTokenStore {
    fn load(&self) -> SyncResult<Option<TokenSet>> {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .map_err(|_| SyncError::Internal("token 存储锁不可用".to_owned()))
    }

    fn save(&self, token: TokenSet) -> SyncResult<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| SyncError::Internal("token 存储锁不可用".to_owned()))?;
        *guard = Some(token);
        Ok(())
    }

    fn clear(&self) -> SyncResult<()> {
        let mut guard = self
            .inner
            .lock()
            .map_err(|_| SyncError::Internal("token 存储锁不可用".to_owned()))?;
        *guard = None;
        Ok(())
    }
}

/// 生产用令牌存储：Windows 凭据管理器（keyring）。
///
/// service/user 固定，token 以 JSON 串存入一条凭据；换机不迁移（plan.md 1.3 只存 token）。
pub struct KeyringTokenStore {
    service: String,
    user: String,
}

impl KeyringTokenStore {
    pub fn new(service: impl Into<String>, user: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            user: user.into(),
        }
    }

    pub fn lagm() -> Self {
        Self::new("LAGM", "bangumi-oauth")
    }

    fn entry(&self) -> SyncResult<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.user)
            .map_err(|err| SyncError::Internal(format!("凭据管理器不可用：{err}")))
    }
}

impl TokenStore for KeyringTokenStore {
    fn load(&self) -> SyncResult<Option<TokenSet>> {
        let entry = self.entry()?;
        match entry.get_password() {
            Ok(raw) => serde_json::from_str(&raw)
                .map(Some)
                .map_err(|err| SyncError::Internal(format!("已存 token 解析失败：{err}"))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(SyncError::Internal(format!("读取 token 失败：{err}"))),
        }
    }

    fn save(&self, token: TokenSet) -> SyncResult<()> {
        let entry = self.entry()?;
        let raw = serde_json::to_string(&token)
            .map_err(|err| SyncError::Internal(format!("序列化 token 失败：{err}")))?;
        entry
            .set_password(&raw)
            .map_err(|err| SyncError::Internal(format!("保存 token 失败：{err}")))
    }

    fn clear(&self) -> SyncResult<()> {
        let entry = self.entry()?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(SyncError::Internal(format!("清除 token 失败：{err}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> TokenSet {
        TokenSet {
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at: Some("2026-12-01T00:00:00Z".into()),
        }
    }

    #[test]
    fn save_load_clear() {
        let store = InMemoryTokenStore::new();
        assert!(store.load().expect("load").is_none());
        store.save(sample()).expect("save");
        assert_eq!(store.load().expect("load"), Some(sample()));
        store.clear().expect("clear");
        assert!(store.load().expect("load").is_none());
    }
}
