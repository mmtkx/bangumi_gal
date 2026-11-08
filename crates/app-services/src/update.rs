//! 更新：检查版本 + 下载安装，两个能力分两条链路。
//!
//! **检查**优先用 Tauri 官方更新器：它读的是 release 里的 `latest.json`，不走 GitHub API、
//! 没有限流，而且拿到的就是可安装的候选。只有在 `latest.json` 不存在或网络异常时才退回
//! 「GitHub API / 发布页跳转」取 tag —— 那条路只能提示版本，装不了。
//!
//! **安装**只能走官方更新器：它要求更新包必须带签名且校验不可关闭。私钥只在打包时
//! 通过 `TAURI_SIGNING_PRIVATE_KEY` 提供，公钥写在 `tauri.conf.json`，仓库里没有任何密钥。

use std::time::Duration;

use tauri::{AppHandle, Emitter};
use tauri_plugin_updater::UpdaterExt;

use crate::dto::UpdateCheckDto;
use crate::error::{ApiResult, AppError};

const RELEASES_API: &str = "https://api.github.com/repos/mmtkx/bangumi_gal/releases/latest";

/// 访问这个地址会被重定向到 `<repo>/releases/tag/<tag>`。
const RELEASES_LATEST_PAGE: &str = "https://github.com/mmtkx/bangumi_gal/releases/latest";

/// 发布页地址：更新器不可用时给前端的兜底链接。
const RELEASES_PAGE: &str = "https://github.com/mmtkx/bangumi_gal/releases/latest";

/// 下载进度事件名（前端直接 `listen` 这个通道，不走主事件总线）。
const PROGRESS_EVENT: &str = "update:progress";

#[derive(Debug, serde::Deserialize)]
struct Release {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
}

#[derive(Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateProgress {
    downloaded: u64,
    total: Option<u64>,
}

/// 查询最新版本并与 `current_version` 比较。
pub async fn check(
    app: &AppHandle,
    current_version: &str,
    proxy: Option<&str>,
) -> ApiResult<UpdateCheckDto> {
    match check_via_updater(app, current_version).await {
        Ok(Some(info)) => Ok(info),
        Ok(None) => Ok(UpdateCheckDto {
            current_version: current_version.to_owned(),
            latest_version: current_version.to_owned(),
            has_update: false,
            release_url: RELEASES_PAGE.to_owned(),
            notes: None,
            published_at: None,
            auto_install: true,
        }),
        Err(reason) => {
            // 没有 latest.json（还没发过带清单的版本）或清单坏了，都会走到这里。
            tracing::debug!(reason = %reason, "官方更新器不可用，退回发布页检查");
            let mut info =
                match fetch_via_api(current_version, proxy).await {
                    Ok(info) => info,
                    Err(api_reason) => fetch_via_redirect(current_version, proxy).await.map_err(
                        |fallback_reason| {
                            AppError::Update(format!(
                                "{api_reason}（备用通道也失败：{fallback_reason}）"
                            ))
                        },
                    )?,
                };
            // 这条链路只认版本号，装不了。
            info.auto_install = false;
            Ok(info)
        }
    }
}

/// 下载并安装更新。Windows 上安装器启动后本进程会自行退出，由 NSIS 重启新版本。
pub async fn install(app: &AppHandle) -> ApiResult<()> {
    let updater = app
        .updater()
        .map_err(|err| AppError::Update(format!("更新器不可用：{err}")))?;
    let update = updater
        .check()
        .await
        .map_err(|err| AppError::Update(format!("检查更新失败：{err}")))?
        .ok_or_else(|| AppError::Update("没有可安装的更新".to_owned()))?;

    tracing::info!(version = %update.version, "开始下载更新包");
    let handle = app.clone();
    let mut downloaded: u64 = 0;
    update
        .download_and_install(
            move |chunk_length, total| {
                downloaded = downloaded.saturating_add(chunk_length as u64);
                let _ = handle.emit(PROGRESS_EVENT, UpdateProgress { downloaded, total });
            },
            || tracing::info!("更新包下载完成，交给安装器"),
        )
        .await
        .map_err(|err| AppError::Update(format!("下载或安装失败：{err}")))
}

/// 官方更新器：读 `latest.json`，得到的版本可以直接安装。
async fn check_via_updater(
    app: &AppHandle,
    current_version: &str,
) -> Result<Option<UpdateCheckDto>, String> {
    let updater = app
        .updater()
        .map_err(|err| format!("更新器不可用：{err}"))?;
    let update = updater
        .check()
        .await
        .map_err(|err| format!("读取更新清单失败：{err}"))?;
    let Some(update) = update else {
        return Ok(None);
    };

    Ok(Some(UpdateCheckDto {
        current_version: current_version.to_owned(),
        latest_version: update.version.clone(),
        has_update: true,
        release_url: format!("{RELEASES_PAGE}/tag/v{}", update.version),
        notes: update.body.clone(),
        published_at: update.date.map(|date| date.to_string()),
        auto_install: true,
    }))
}

/// 首选兜底通道：REST API，能拿到发布说明与时间。
async fn fetch_via_api(
    current_version: &str,
    proxy: Option<&str>,
) -> Result<UpdateCheckDto, String> {
    let client = build_client(current_version, proxy).map_err(|err| err.to_string())?;
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
        auto_install: false,
    })
}

/// 次选兜底通道：只跟随发布页的重定向取 tag，不需要 API 配额。
async fn fetch_via_redirect(
    current_version: &str,
    proxy: Option<&str>,
) -> Result<UpdateCheckDto, String> {
    let client = build_client(current_version, proxy).map_err(|err| err.to_string())?;
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
        auto_install: false,
    })
}

fn build_client(current_version: &str, proxy: Option<&str>) -> Result<reqwest::Client, AppError> {
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
