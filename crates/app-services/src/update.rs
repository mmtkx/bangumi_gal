//! 检查更新：读 GitHub 上最新的发布版本号，**不下载、不安装任何东西**。
//!
//! 为什么不用 Tauri 官方 updater：它要求更新包必须签名且无法关闭校验，
//! 本项目暂无密钥体系。当前只做「有新版本 → 提示 + 打开下载页」，
//! 用户手动装安装包，避免为了自动安装引入私钥保管与供应链风险。
//!
//! 两条通道，按顺序尝试：
//! 1. GitHub REST API：信息最全（发布说明、时间），但未认证请求限 60 次/小时/IP，
//!    走共享代理出口时很容易被别人的流量吃掉配额；
//! 2. 发布页跳转：请求 `github.com/<repo>/releases/latest`，跟随 302 从最终地址里取 tag。
//!    不走 API、没有配额，只是拿不到发布说明。
//!
//! 有备用通道，限流就退化成「少显示一段发布说明」，而不是整个功能报错。

use std::time::Duration;

use crate::dto::UpdateCheckDto;
use crate::error::{ApiResult, AppError};

const RELEASES_API: &str = "https://api.github.com/repos/mmtkx/bangumi_gal/releases/latest";

/// 访问这个地址会被重定向到 `<repo>/releases/tag/<tag>`。
const RELEASES_LATEST_PAGE: &str = "https://github.com/mmtkx/bangumi_gal/releases/latest";

#[derive(Debug, serde::Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

/// 查询最新版本并与 `current_version` 比较。
pub async fn check(current_version: &str, proxy: Option<&str>) -> ApiResult<UpdateCheckDto> {
    let client = build_client(current_version, proxy)?;
    match fetch_via_api(&client, current_version).await {
        Ok(info) => Ok(info),
        Err(api_reason) => match fetch_via_redirect(&client, current_version).await {
            Ok(info) => {
                tracing::debug!(reason = %api_reason, "GitHub API 不可用，改用发布页跳转检查更新");
                Ok(info)
            }
            Err(fallback_reason) => Err(AppError::Update(format!(
                "{api_reason}（备用通道也失败：{fallback_reason}）"
            ))),
        },
    }
}

fn build_client(current_version: &str, proxy: Option<&str>) -> ApiResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        // 超时给得短：检查更新是锦上添花，不该让设置页转圈十几秒。
        .connect_timeout(Duration::from_secs(6))
        .timeout(Duration::from_secs(12))
        // GitHub API 强制要求 User-Agent。
        .user_agent(format!("LAGM/{current_version}"));
    if let Some(proxy) = proxy {
        let proxy = reqwest::Proxy::all(proxy)
            .map_err(|err| AppError::Update(format!("代理地址无效：{proxy}（{err}）")))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|err| AppError::Update(format!("创建 HTTP 客户端失败：{err}")))
}

/// 首选通道：REST API，能拿到发布说明与时间。失败时返回一句给用户看的原因。
async fn fetch_via_api(
    client: &reqwest::Client,
    current_version: &str,
) -> Result<UpdateCheckDto, String> {
    let response = client
        .get(RELEASES_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| format!("访问 GitHub API 失败：{err}"))?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("仓库还没有发布任何正式版本".to_owned());
    }
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        // 未认证请求每个 IP 每小时 60 次，共享代理出口很容易被别人的流量吃掉。
        return Err("GitHub API 限流".to_owned());
    }
    if !status.is_success() {
        return Err(format!("GitHub API 返回 HTTP {status}"));
    }

    let release: Release = response
        .json()
        .await
        .map_err(|err| format!("解析发布信息失败：{err}"))?;

    Ok(UpdateCheckDto {
        current_version: current_version.to_owned(),
        latest_version: release.tag_name.trim_start_matches('v').to_owned(),
        has_update: is_newer(&release.tag_name, current_version),
        release_url: release.html_url,
        notes: release.body.filter(|body| !body.trim().is_empty()),
        published_at: release.published_at,
    })
}

/// 备用通道：只跟随发布页的重定向取 tag，不需要 API 配额。
async fn fetch_via_redirect(
    client: &reqwest::Client,
    current_version: &str,
) -> Result<UpdateCheckDto, String> {
    let response = client
        .get(RELEASES_LATEST_PAGE)
        .send()
        .await
        .map_err(|err| format!("访问发布页失败：{err}"))?;
    if !response.status().is_success() {
        return Err(format!("发布页返回 HTTP {}", response.status()));
    }

    // reqwest 默认跟随重定向，最终地址形如 `.../releases/tag/v0.0.1`。
    let final_url = response.url().to_string();
    let tag = tag_from_release_url(&final_url)
        .ok_or_else(|| "发布页没有指向任何 tag（可能还没有正式版本）".to_owned())?;

    Ok(UpdateCheckDto {
        current_version: current_version.to_owned(),
        latest_version: tag.trim_start_matches('v').to_owned(),
        has_update: is_newer(&tag, current_version),
        release_url: final_url,
        // 这条路拿不到发布说明，界面上那一段自然不显示。
        notes: None,
        published_at: None,
    })
}

/// 从 `https://github.com/o/r/releases/tag/v1.2.3` 里取出 `v1.2.3`。
fn tag_from_release_url(url: &str) -> Option<String> {
    let rest = url.split("/releases/tag/").nth(1)?;
    let tag = rest
        .split(['?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_end_matches('/');
    if tag.is_empty() {
        None
    } else {
        Some(tag.to_owned())
    }
}

/// `latest` 是否比 `current` 新。两边的 `v` 前缀与预发布后缀都会被忽略。
///
/// 只按点分数字比较，够用且不会因为多带一个 `-beta` 就判成「不更新的更高版本」。
fn is_newer(latest: &str, current: &str) -> bool {
    let parse = |raw: &str| -> Vec<u64> {
        raw.trim()
            .trim_start_matches(['v', 'V'])
            .split('.')
            .map(|part| {
                // 取数字前缀：`1-beta` 视为 1，`2` 视为 2，非数字视为 0。
                let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
                digits.parse::<u64>().unwrap_or(0)
            })
            .collect()
    };

    let latest = parse(latest);
    let current = parse(current);
    let width = latest.len().max(current.len());
    for index in 0..width {
        let left = latest.get(index).copied().unwrap_or(0);
        let right = current.get(index).copied().unwrap_or(0);
        if left != right {
            return left > right;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_ignoring_prefix_and_suffix() {
        assert!(is_newer("v0.0.2", "0.0.1"));
        assert!(is_newer("0.1.0", "0.0.9"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.0.1", "0.0.1"));
        assert!(!is_newer("v0.0.1", "0.0.2"));
        // 预发布后缀按主版本比较，不能因为多带了字符就判成「更新」。
        assert!(!is_newer("0.0.1-beta", "0.0.1"));
        assert!(is_newer("0.0.2-beta", "0.0.1"));
    }

    #[test]
    fn extracts_tag_from_redirect_url() {
        assert_eq!(
            tag_from_release_url("https://github.com/mmtkx/bangumi_gal/releases/tag/v0.0.1"),
            Some("v0.0.1".to_owned())
        );
        assert_eq!(
            tag_from_release_url("https://github.com/o/r/releases/tag/v1.2.3/"),
            Some("v1.2.3".to_owned())
        );
        // 没有发布过版本时 GitHub 会重定向到 /releases，这里必须返回 None 而不是乱取字符串。
        assert_eq!(
            tag_from_release_url("https://github.com/mmtkx/bangumi_gal/releases"),
            None
        );
    }
}
