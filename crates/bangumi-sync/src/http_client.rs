//! 基于 reqwest 的 Bangumi v0 API 客户端。
//!
//! token 从 `TokenStore` 动态读取，临近过期自动用 refresh token 续期，
//! 因此同一个实例在「授权前 / 授权后」都可以复用，无需重建 SyncEngine。
//!
//! 单集进度按「看过 / 未看」布尔语义同步：云端只有这两种状态，
//! 本地秒数仅在推送时折算成布尔（> 0 即看过），拉取时用 1 秒占位表示「看过」。

use std::sync::{Arc, RwLock};
use std::time::Duration;

use serde::Deserialize;

use crate::client::{BangumiClient, BoxFut};
use crate::error::{SyncError, SyncResult};
use crate::oauth::{OAuthConfig, SyncConfigProvider, USER_AGENT};
use crate::token::{TokenSet, TokenStore};
use crate::types::{
    AccountProfile, CloudCollection, CloudEpisodeProgress, CollectionPayload, ProgressPayload,
    RelatedSubject, SubjectSearchHit,
};

const API_BASE: &str = "https://api.bgm.tv";
/// 剩余寿命低于该阈值即提前刷新，避免临界过期。
const REFRESH_MARGIN_SECONDS: i64 = 60;

/// 缓存的 HTTP 客户端及其代理配置。代理变更时重建。
struct CachedHttp {
    proxy: Option<String>,
    client: reqwest::Client,
    /// 当前 token 对应的用户名。
    ///
    /// Bangumi v0 的几个接口对「当前用户」的写法要求不一致：
    ///
    /// - `GET /users/{username}/collections/{id}`：只认真实 username（`-` 返回 404）；
    /// - `POST /users/-/collections/{id}`：只认 `-`（username 返回 404）；
    /// - `GET|PATCH /users/-/collections/{id}/episodes`：只认 `-`（username 返回 400）。
    ///
    /// 这里只缓存给「读取单个收藏」用。
    username: Option<String>,
}

/// 真实 Bangumi API 客户端。
pub struct BangumiHttpClient {
    http: RwLock<CachedHttp>,
    store: Arc<dyn TokenStore>,
    config: Arc<dyn SyncConfigProvider>,
    refresh_guard: tokio::sync::Mutex<()>,
}

impl BangumiHttpClient {
    pub fn new(
        store: Arc<dyn TokenStore>,
        config: Arc<dyn SyncConfigProvider>,
    ) -> SyncResult<Self> {
        let proxy = config.proxy();
        let client = build_http_client(proxy.as_deref())?;
        Ok(Self {
            http: RwLock::new(CachedHttp {
                proxy,
                client,
                username: None,
            }),
            store,
            config,
            refresh_guard: tokio::sync::Mutex::new(()),
        })
    }

    /// 取当前 HTTP 客户端；代理配置变化时自动重建（设置保存后无需重启）。
    fn http(&self) -> SyncResult<reqwest::Client> {
        let proxy = self.config.proxy();
        {
            let guard = self
                .http
                .read()
                .map_err(|_| SyncError::Internal("HTTP 客户端锁不可用".to_owned()))?;
            if guard.proxy == proxy {
                return Ok(guard.client.clone());
            }
        }
        let client = build_http_client(proxy.as_deref())?;
        let mut guard = self
            .http
            .write()
            .map_err(|_| SyncError::Internal("HTTP 客户端锁不可用".to_owned()))?;
        // 换代理等于换了出口，用户名缓存仍然有效（与 token 绑定，不随代理变化），保留。
        let username = guard.username.take();
        *guard = CachedHttp {
            proxy,
            client,
            username,
        };
        Ok(guard.client.clone())
    }

    /// 取可用 access token；临近过期则刷新并回写。
    pub async fn access_token(&self) -> SyncResult<String> {
        let token = self.store.load()?.ok_or(SyncError::NotAuthorized)?;
        if !token_expired(&token) {
            return Ok(token.access_token);
        }
        self.refresh_locked(true).await
    }

    /// 无条件续期一次（见 `BangumiClient::force_refresh`）。
    async fn force_refresh_once(&self) -> SyncResult<()> {
        if self.store.load()?.is_none() {
            return Ok(());
        }
        self.refresh_locked(false).await.map(|_| ())
    }

    /// 在锁内刷新并回写。
    ///
    /// `only_if_expired` 为真时先复查：并发场景下别的任务可能已经刷过，
    /// 避免为同一份 token 重复请求。服务端判定令牌失效时清空存储并返回
    /// `NotAuthorized`，让界面直接显示「未授权 / 需重新授权」而不是继续假装已登录。
    async fn refresh_locked(&self, only_if_expired: bool) -> SyncResult<String> {
        let _guard = self.refresh_guard.lock().await;
        let token = self.store.load()?.ok_or(SyncError::NotAuthorized)?;
        if only_if_expired && !token_expired(&token) {
            return Ok(token.access_token);
        }
        let refresh_token = token
            .refresh_token
            .clone()
            .ok_or(SyncError::NotAuthorized)?;
        let config: OAuthConfig = self.config.oauth_config()?;
        let http = self.http()?;
        let refreshed = match crate::oauth::refresh(&http, &config, &refresh_token).await {
            Ok(value) => value,
            Err(SyncError::NotAuthorized) => {
                self.store.clear()?;
                return Err(SyncError::NotAuthorized);
            }
            Err(err) => return Err(err),
        };
        let access = refreshed.access_token.clone();
        self.store.save(refreshed)?;
        // 换 token 可能意味着换了账号，用户名缓存必须失效。
        if let Ok(mut guard) = self.http.write() {
            guard.username = None;
        }
        Ok(access)
    }

    /// 当前 token 对应的用户名；首次调用走 `/v0/me`，之后走缓存。
    ///
    /// 刷新 token 后要清空缓存（可能换了账号），见 `access_token`。
    pub async fn username(&self) -> SyncResult<String> {
        if let Ok(guard) = self.http.read() {
            if let Some(name) = guard.username.clone() {
                return Ok(name);
            }
        }
        let profile = self.fetch_me().await?;
        let name = if profile.username.trim().is_empty() {
            profile.id.to_string()
        } else {
            profile.username.clone()
        };
        let mut guard = self
            .http
            .write()
            .map_err(|_| SyncError::Internal("HTTP 客户端锁不可用".to_owned()))?;
        guard.username = Some(name.clone());
        Ok(name)
    }

    fn api_url(&self, path: &str) -> String {
        format!("{API_BASE}{path}")
    }
}

/// 把 `collections/{id}/episodes` 的响应解析成统一的进度行。
///
///
/// 兼容两种形状：`[{episode:{...},type:2}]` 与直接平铺的 `[{id,sort,type:2}]`。
fn parse_episode_progress(subject_id: i64, raw: &str) -> SyncResult<Vec<CloudEpisodeProgress>> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|err| {
        tracing::debug!(body = %raw, "解析单集进度失败正文");
        SyncError::Api(format!("解析单集进度失败：{err}"))
    })?;
    // 形态不止一种：裸数组、{data:[...]}、以及未来可能的分页包裹。
    let rows: Vec<serde_json::Value> = match value {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => map
            .get("data")
            .or_else(|| map.get("episodes"))
            .or_else(|| map.get("items"))
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let episode_obj = row.get("episode").unwrap_or(&row);
        let Some(episode) = episode_number(episode_obj) else {
            continue;
        };
        let kind = row
            .get("type")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        out.push(CloudEpisodeProgress {
            subject_id,
            episode,
            // 云端只有布尔状态：看过记 1 秒占位，未看记 0；本地秒数不会被它覆盖。
            watched_seconds: u64::from(kind == 2),
            updated_at: row
                .get("updated_at")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        });
    }
    Ok(out)
}

/// 从剧集对象里取集号：优先 `sort`（浮点排序号），退化为 `ep`（可能是 "01" 字符串）。
fn episode_number(value: &serde_json::Value) -> Option<u32> {
    let from_number = |raw: Option<&serde_json::Value>| -> Option<u32> {
        raw.and_then(|item| {
            item.as_u64()
                .or_else(|| item.as_f64().map(|value| value as u64))
                .or_else(|| {
                    item.as_str()
                        .and_then(|text| text.parse::<f64>().ok())
                        .map(|v| v as u64)
                })
        })
        .filter(|value| *value > 0)
        .map(|value| value as u32)
    };
    from_number(value.get("sort"))
        .or_else(|| from_number(value.get("ep")))
        .or_else(|| from_number(value.get("sort").or_else(|| value.get("ep"))))
}

/// 解析 `/v0/me`：头像有 large / medium / small 三档，按顺序取第一个可用的。
fn parse_account(raw: &str) -> SyncResult<AccountProfile> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|err| {
        tracing::debug!(body = %raw, "解析账号资料失败正文");
        SyncError::Api(format!("解析账号资料失败：{err}"))
    })?;
    let id = value
        .get("id")
        .and_then(serde_json::Value::as_i64)
        .ok_or_else(|| SyncError::Api("账号资料缺少 id".to_owned()))?;
    let pick = |key: &str| {
        value
            .get("avatar")
            .and_then(|avatar| avatar.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    Ok(AccountProfile {
        id,
        username: value
            .get("username")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        nickname: value
            .get("nickname")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        avatar_url: pick("large")
            .or_else(|| pick("medium"))
            .or_else(|| pick("small")),
    })
}

/// 搜索响应解析：既接受 `{data:[...]}` 也接受裸数组。
fn parse_search_response(raw: &str) -> SyncResult<Vec<SubjectSearchHit>> {
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| SyncError::Api(format!("解析搜索响应失败：{err}")))?;
    if value.is_array() {
        return serde_json::from_value(value)
            .map_err(|err| SyncError::Api(format!("解析搜索响应失败：{err}")));
    }
    let parsed: SearchResponse = serde_json::from_value(value)
        .map_err(|err| SyncError::Api(format!("解析搜索响应失败：{err}")))?;
    Ok(parsed.data)
}

/// 在 `/v0/episodes` 响应里按集号找 episode id。
fn find_episode_id(raw: &str, episode: u32) -> Option<i64> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let list = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .or_else(|| value.as_array())?;
    list.iter().find_map(|item| {
        if episode_number(item) == Some(episode) {
            item.get("id").and_then(serde_json::Value::as_i64)
        } else {
            None
        }
    })
}

/// 构建 HTTP 客户端；`proxy` 为空时跟随系统代理。
pub fn build_http_client(proxy: Option<&str>) -> SyncResult<reqwest::Client> {
    // 超时要短：bgm.tv 经代理偶尔不可达，一条挂 20 秒会让整批操作看起来卡死。
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(15));
    builder = match proxy {
        Some(raw) => {
            let parsed = reqwest::Proxy::all(raw)
                .map_err(|err| SyncError::Internal(format!("代理地址无效：{raw}（{err}）")))?;
            builder.proxy(parsed)
        }
        // 不显式配置就跟随系统代理（reqwest 的 system-proxy 特性读 Windows 设置与环境变量）。
        // 这里曾经强制 no_proxy，导致「浏览器能打开 bgm.tv、应用却连接超时」。
        None => builder,
    };
    builder
        .build()
        .map_err(|err| SyncError::Internal(format!("构建 HTTP 客户端失败：{err}")))
}

/// 用一次性 HTTP 客户端完成授权码交换（授权流程无需复用长连接）。
pub async fn exchange_code_with_client(
    config: &OAuthConfig,
    code: &str,
    proxy: Option<&str>,
) -> SyncResult<TokenSet> {
    let http = build_http_client(proxy)?;
    crate::oauth::exchange_code(&http, config, code).await
}

fn token_expired(token: &TokenSet) -> bool {
    let Some(raw) = &token.expires_at else {
        return false;
    };
    match chrono::DateTime::parse_from_rfc3339(raw) {
        Ok(at) => {
            let utc = at.with_timezone(&chrono::Utc);
            utc <= chrono::Utc::now() + chrono::Duration::seconds(REFRESH_MARGIN_SECONDS)
        }
        // 无法解析就当作未过期，让请求自己暴露问题。
        Err(_) => false,
    }
}

#[derive(Debug, Deserialize)]
struct CollectionRow {
    #[serde(rename = "type", default)]
    collection_type: Option<i64>,
    #[serde(default)]
    rate: Option<i64>,
    #[serde(default)]
    updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Vec<SubjectSearchHit>,
}

/// 展开 reqwest 错误的完整原因链，便于定位 DNS / TLS / 代理 / 超时。
pub fn describe_error(err: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut detail = err.to_string();
    let mut source: Option<&(dyn std::error::Error + 'static)> = err.source();
    while let Some(cause) = source {
        detail.push_str(" <- ");
        detail.push_str(&cause.to_string());
        source = cause.source();
    }
    if err.is_timeout() {
        detail.push_str(" [超时]");
    }
    if err.is_connect() {
        detail.push_str(" [连接失败]");
    }
    detail
}

/// 把响应正文压成一行短提示：错误信息是给人看的，整段 JSON 只会淹没界面。
fn brief(raw: &str) -> String {
    const LIMIT: usize = 160;
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= LIMIT {
        return collapsed;
    }
    let mut out: String = collapsed.chars().take(LIMIT).collect();
    out.push('…');
    out
}

fn transport(err: reqwest::Error) -> SyncError {
    SyncError::Api(format!("网络请求失败：{}", describe_error(&err)))
}

async fn ensure_success(response: reqwest::Response, action: &str) -> SyncResult<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let text = response.text().await.unwrap_or_default();
    tracing::debug!(action, status = %status, body = %text, "Bangumi 请求失败正文");
    Err(SyncError::Api(format!(
        "{action}失败：HTTP {status} {}",
        brief(&text)
    )))
}

impl BangumiClient for BangumiHttpClient {
    fn is_authorized(&self) -> bool {
        self.store
            .load()
            .map(|token| token.is_some())
            .unwrap_or(false)
    }

    fn force_refresh(&self) -> BoxFut<'_, SyncResult<()>> {
        Box::pin(async move { self.force_refresh_once().await })
    }

    fn fetch_collection(&self, subject_id: i64) -> BoxFut<'_, SyncResult<CloudCollection>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;
            // 注意：收藏单条只认真实 username；换成 `-` 会稳定 404（被误判成「未收藏」）。
            let username = self.username().await?;
            let url = self.api_url(&format!("/v0/users/{username}/collections/{subject_id}"));
            let response = http
                .get(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(transport)?;

            if response.status() == reqwest::StatusCode::NOT_FOUND {
                // 未收藏：返回空快照，由引擎决定是否导入/推送。
                // 这条日志在「网页上明明已收藏」时是首要排查线索（通常是授权了另一个账号）。
                tracing::debug!(
                    subject = subject_id,
                    "Bangumi 返回 404：当前授权账号未收藏该条目"
                );
                return Ok(CloudCollection {
                    subject_id,
                    collection_status: None,
                    rating: None,
                    updated_at: None,
                });
            }

            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "拉取收藏失败正文");
                return Err(SyncError::Api(format!(
                    "拉取收藏失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            let row: CollectionRow = serde_json::from_str(&text).map_err(|err| {
                tracing::debug!(body = %text, "解析收藏响应失败正文");
                SyncError::Api(format!("解析收藏响应失败：{err}"))
            })?;
            Ok(CloudCollection {
                subject_id,
                collection_status: row.collection_type,
                // Bangumi 用 0 表示未评分。
                rating: row.rate.filter(|value| *value > 0),
                updated_at: row.updated_at,
            })
        })
    }

    fn push_collection(&self, payload: CollectionPayload) -> BoxFut<'_, SyncResult<()>> {
        Box::pin(async move {
            // Bangumi 收藏接口以 type 必填；未设置收藏状态时跳过（仅评分无法单独写入）。
            let Some(collection_type) = payload.collection_status else {
                return Ok(());
            };
            let token = self.access_token().await?;
            let http = self.http()?;
            // 与读取相反：写入收藏只认 `-`（换成真实 username 会 404）。
            let url = self.api_url(&format!("/v0/users/-/collections/{}", payload.subject_id));
            let body = serde_json::json!({
                "type": collection_type,
                "rate": payload.rating.unwrap_or(0),
            });
            let response = http
                .post(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .json(&body)
                .send()
                .await
                .map_err(transport)?;
            ensure_success(response, "推送收藏状态").await
        })
    }

    fn fetch_episode_progress(
        &self,
        subject_id: i64,
    ) -> BoxFut<'_, SyncResult<Vec<CloudEpisodeProgress>>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;
            // 注意：这里必须用 `-`（当前用户）。换成真实 username 会被服务端判 400，
            // 而收藏接口 `/collections/{id}` 恰好相反——只认真实 username，`-` 会 404。
            let url = self.api_url(&format!("/v0/users/-/collections/{subject_id}/episodes"));
            let response = http
                .get(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(&[("limit", "100"), ("offset", "0")])
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "拉取单集进度失败正文");
                return Err(SyncError::Api(format!(
                    "拉取单集进度失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            parse_episode_progress(subject_id, &text)
        })
    }

    fn push_episode_progress(&self, payload: ProgressPayload) -> BoxFut<'_, SyncResult<()>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;

            // 先按集号换出 Bangumi 的 episode id：写进度只能用 id，不能只用集号。
            let list_url = self.api_url("/v0/episodes");
            let response = http
                .get(&list_url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(&[
                    ("subject_id", payload.subject_id.to_string()),
                    ("type", "0".to_owned()),
                    ("limit", "200".to_owned()),
                    ("offset", "0".to_owned()),
                ])
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "获取剧集列表失败正文");
                return Err(SyncError::Api(format!(
                    "获取剧集列表失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            let episode_id = find_episode_id(&text, payload.episode).ok_or_else(|| {
                SyncError::Api(format!(
                    "Bangumi 条目 {} 里找不到第 {} 集",
                    payload.subject_id, payload.episode
                ))
            })?;

            // 与拉取一致：写进度用 `-`（当前用户）。
            let url = self.api_url(&format!(
                "/v0/users/-/collections/{}/episodes/{episode_id}",
                payload.subject_id
            ));
            // type：0 未看、2 看过。本地秒数 > 0 即视为看过。
            let kind = if payload.watched_seconds > 0 { 2 } else { 0 };
            let response = http
                .patch(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .json(&serde_json::json!({ "type": kind }))
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            if !status.is_success() {
                let text = response.text().await.unwrap_or_default();
                tracing::debug!(body = %text, "推送单集进度失败正文");
                return Err(SyncError::Api(format!(
                    "推送第 {} 集进度失败：HTTP {status} {}",
                    payload.episode,
                    brief(&text)
                )));
            }
            Ok(())
        })
    }

    fn fetch_me(&self) -> BoxFut<'_, SyncResult<AccountProfile>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;
            let url = self.api_url("/v0/me");
            let response = http
                .get(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "读取账号资料失败正文");
                return Err(SyncError::Api(format!(
                    "读取账号资料失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            parse_account(&text)
        })
    }

    fn fetch_related_subjects(
        &self,
        subject_id: i64,
    ) -> BoxFut<'_, SyncResult<Vec<RelatedSubject>>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;
            let url = self.api_url(&format!("/v0/subjects/{subject_id}/subjects"));
            let response = http
                .get(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "读取关联条目失败正文");
                return Err(SyncError::Api(format!(
                    "读取关联条目失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            let rows: Vec<serde_json::Value> = serde_json::from_str(&text).map_err(|err| {
                tracing::debug!(body = %text, "解析关联条目失败正文");
                SyncError::Api(format!("解析关联条目失败：{err}"))
            })?;
            Ok(rows
                .into_iter()
                .filter_map(|row| {
                    Some(RelatedSubject {
                        id: row.get("id")?.as_i64()?,
                        name: row
                            .get("name")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        name_cn: row
                            .get("name_cn")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        relation: row
                            .get("relation")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        subject_type: row
                            .get("type")
                            .and_then(serde_json::Value::as_i64)
                            .unwrap_or_default(),
                    })
                })
                .collect())
        })
    }

    fn fetch_bytes(&self, url: String) -> BoxFut<'_, SyncResult<Vec<u8>>> {
        Box::pin(async move {
            // 图片走公开 CDN，不带 Authorization；超时比 JSON 请求更短。
            let http = self.http()?;
            let response = http
                .get(&url)
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            if !status.is_success() {
                return Err(SyncError::Api(format!("下载图片失败：HTTP {status}")));
            }
            let bytes = response.bytes().await.map_err(transport)?;
            // 封面正常在 100 KB 级别，超过 4 MB 视为异常响应，避免把内存写爆。
            if bytes.len() > 4 * 1024 * 1024 {
                return Err(SyncError::Api(format!(
                    "图片过大（{} 字节），已放弃缓存",
                    bytes.len()
                )));
            }
            Ok(bytes.to_vec())
        })
    }

    fn fetch_subject_metadata(&self, subject_id: i64) -> BoxFut<'_, SyncResult<serde_json::Value>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;
            let url = self.api_url(&format!("/v0/subjects/{subject_id}"));
            let response = http
                .get(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "拉取元数据失败正文");
                return Err(SyncError::Api(format!(
                    "拉取元数据失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            serde_json::from_str(&text).map_err(|err| {
                tracing::debug!(body = %text, "解析元数据失败正文");
                SyncError::Api(format!("解析元数据失败：{err}"))
            })
        })
    }

    fn search_subjects(
        &self,
        keyword: String,
        subject_type: i64,
    ) -> BoxFut<'_, SyncResult<Vec<SubjectSearchHit>>> {
        Box::pin(async move {
            let token = self.access_token().await?;
            let http = self.http()?;
            let url = self.api_url("/v0/search/subjects");
            let body = serde_json::json!({
                "keyword": keyword,
                "sort": "match",
                // nsfw=true 才能搜到成人向条目（galgame 大多在此列）。
                "filter": { "type": [subject_type], "nsfw": true }
            });
            let response = http
                .post(&url)
                .bearer_auth(&token)
                .header(reqwest::header::ACCEPT, "application/json")
                .query(&[("limit", "8"), ("offset", "0")])
                .json(&body)
                .send()
                .await
                .map_err(transport)?;
            let status = response.status();
            let text = response.text().await.map_err(transport)?;
            if !status.is_success() {
                tracing::debug!(body = %text, "搜索条目失败正文");
                return Err(SyncError::Api(format!(
                    "搜索条目失败：HTTP {status} {}",
                    brief(&text)
                )));
            }
            match parse_search_response(&text) {
                Ok(hits) => Ok(hits),
                Err(err) => {
                    tracing::debug!(body = %text, "解析搜索响应失败正文");
                    Err(err)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(expires_at: Option<&str>) -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at: expires_at.map(str::to_owned),
        }
    }

    #[test]
    fn token_without_expiry_is_not_expired() {
        assert!(!token_expired(&token(None)));
    }

    #[test]
    fn token_expiring_soon_is_expired() {
        let soon = (chrono::Utc::now() + chrono::Duration::seconds(30))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(token_expired(&token(Some(&soon))));
    }

    #[test]
    fn token_with_long_life_is_not_expired() {
        let later = (chrono::Utc::now() + chrono::Duration::hours(2))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        assert!(!token_expired(&token(Some(&later))));
    }

    #[test]
    fn parses_wrapped_episode_progress() {
        let raw = r#"[
            {"episode":{"id":101,"sort":1.0,"ep":1.0},"type":2,"updated_at":"2026-10-30T08:00:00Z"},
            {"episode":{"id":102,"sort":2.0,"ep":2.0},"type":0}
        ]"#;
        let rows = parse_episode_progress(42, raw).expect("parse");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].episode, 1);
        assert_eq!(rows[0].watched_seconds, 1, "type=2 表示看过");
        assert_eq!(rows[0].updated_at.as_deref(), Some("2026-10-30T08:00:00Z"));
        assert_eq!(rows[1].episode, 2);
        assert_eq!(rows[1].watched_seconds, 0, "type=0 表示未看");
        assert!(rows.iter().all(|row| row.subject_id == 42));
    }

    #[test]
    fn parses_flat_episode_progress_with_string_ep() {
        let raw = r#"[{"id":7,"sort":3.5,"ep":"03","type":2}]"#;
        let rows = parse_episode_progress(9, raw).expect("parse");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].episode, 3, "3.5 取下整");
        assert_eq!(rows[0].watched_seconds, 1);
    }

    #[test]
    fn skips_rows_without_episode_number() {
        let raw = r#"[{"episode":{"id":1},"type":2}]"#;
        assert!(parse_episode_progress(1, raw).expect("parse").is_empty());
    }

    #[test]
    fn parses_account_profile_with_avatar_fallback() {
        let raw = r#"{"id":123456,"username":"lagm_user","nickname":"测试用户","avatar":{"large":"https://lain.bgm.tv/pic/user/l/1.jpg","medium":"","small":""}}"#;
        let account = parse_account(raw).expect("parse");
        assert_eq!(account.id, 123456);
        assert_eq!(account.username, "lagm_user");
        assert_eq!(account.nickname, "测试用户");
        assert_eq!(
            account.avatar_url.as_deref(),
            Some("https://lain.bgm.tv/pic/user/l/1.jpg")
        );
    }

    #[test]
    fn account_without_avatar_or_nickname_is_ok() {
        let raw = r#"{"id":1,"username":"u","avatar":{}}"#;
        let account = parse_account(raw).expect("parse");
        assert_eq!(account.nickname, "");
        assert_eq!(account.avatar_url, None);
    }

    #[test]
    fn finds_episode_id_from_wrapped_and_flat_lists() {
        let wrapped = r#"{"data":[{"id":11,"sort":1.0},{"id":12,"sort":2.0}],"total":2}"#;
        assert_eq!(find_episode_id(wrapped, 2), Some(12));
        let flat = r#"[{"id":21,"ep":3}]"#;
        assert_eq!(find_episode_id(flat, 3), Some(21));
        assert_eq!(find_episode_id(flat, 9), None);
    }
}
