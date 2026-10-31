//! Bangumi OAuth 2.0：授权 URL、本地回调监听、授权码换 token 与刷新。
//!
//! 端点（见 bangumi/api 文档）：
//! - 授权：`GET https://bgm.tv/oauth/authorize`
//! - 换 token / 刷新：`POST https://bgm.tv/oauth/access_token`
//! - 回调地址必须与应用后台登记值完全一致；授权码 60 秒内有效。

use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::sync::Mutex;

use crate::error::{SyncError, SyncResult};
use crate::token::TokenSet;

pub const AUTHORIZE_URL: &str = "https://bgm.tv/oauth/authorize";
pub const TOKEN_URL: &str = "https://bgm.tv/oauth/access_token";
pub const USER_AGENT: &str = concat!(
    "lagm/lagm-desktop/",
    env!("CARGO_PKG_VERSION"),
    " (Windows)"
);

/// OAuth 应用配置，来自本地设置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthConfig {
    pub client_id: String,
    pub client_secret: String,
    /// 必须与 Bangumi 应用后台的「回调地址」完全一致，例如 `http://127.0.0.1:27432/callback`。
    pub redirect_uri: String,
}

/// 配置来源抽象。app-services 用本地设置实现；单元测试可用固定实现。
pub trait SyncConfigProvider: Send + Sync {
    fn oauth_config(&self) -> SyncResult<OAuthConfig>;

    /// 出站代理。默认「环境变量 → Windows 系统代理」自动探测。
    fn proxy(&self) -> Option<String> {
        crate::proxy::resolve_proxy()
    }
}

/// 固定配置，供测试与离线构造。
pub struct StaticConfigProvider(pub OAuthConfig);

impl SyncConfigProvider for StaticConfigProvider {
    fn oauth_config(&self) -> SyncResult<OAuthConfig> {
        Ok(self.0.clone())
    }
}

/// 构造授权页 URL。
pub fn build_authorize_url(config: &OAuthConfig, state: &str) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.append_pair("client_id", &config.client_id);
    serializer.append_pair("response_type", "code");
    serializer.append_pair("redirect_uri", &config.redirect_uri);
    serializer.append_pair("state", state);
    format!("{AUTHORIZE_URL}?{}", serializer.finish())
}

/// 从 `path?query` 中提取查询参数（回调地址是本地 HTTP，路径以 `/` 开头）。
fn query_param(raw_url: &str, key: &str) -> Option<String> {
    let full = format!("http://127.0.0.1{raw_url}");
    let parsed = url::Url::parse(&full).ok()?;
    parsed
        .query_pairs()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.into_owned())
}

/// 从用户粘贴的「完整回调地址」或裸 code 中解析授权码。
///
/// 支持：`http://127.0.0.1:27432/callback?code=xxx&state=yyy`、
/// `/callback?code=xxx`、以及直接粘贴的 `xxx`。
pub fn extract_code(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = url::Url::parse(trimmed) {
        if let Some((_, value)) = parsed.query_pairs().find(|(name, _)| name == "code") {
            return Some(value.into_owned());
        }
    }
    if let Some(index) = trimmed.find("code=") {
        let rest = &trimmed[index + 5..];
        let code = rest.split(['&', '#']).next().unwrap_or("").trim();
        if !code.is_empty() {
            return Some(code.to_owned());
        }
    }
    // 没有任何 URL 结构时视作裸 code。
    if !trimmed.contains(['/', ' ', '?', '#']) {
        return Some(trimmed.to_owned());
    }
    None
}

/// 授权排障日志：写 `%TEMP%\lagm-oauth.log`，best-effort，不记录 code 明文。
fn debug_log(message: &str) {
    use std::io::Write;
    let path = std::env::temp_dir().join("lagm-oauth.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(
            file,
            "{} {}",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            message
        );
    }
}

fn bind_server(redirect_uri: &str) -> SyncResult<tiny_http::Server> {
    let parsed = url::Url::parse(redirect_uri)
        .map_err(|err| SyncError::Internal(format!("回调地址非法：{redirect_uri}（{err}）")))?;
    let host = parsed.host_str().unwrap_or("127.0.0.1");
    let port = parsed
        .port()
        .ok_or_else(|| SyncError::Internal("回调地址必须显式指定端口".to_owned()))?;
    let addr = format!("{host}:{port}");
    tiny_http::Server::http(&addr).map_err(|err| {
        SyncError::Internal(format!("无法监听回调地址 {addr}（端口可能被占用）：{err}"))
    })
}

fn respond_html(request: tiny_http::Request, status: u16, body: &str) {
    let header =
        tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"text/html; charset=utf-8"[..])
            .expect("静态 header 合法");
    let response = tiny_http::Response::from_string(body)
        .with_status_code(tiny_http::StatusCode(status))
        .with_header(header);
    let _ = request.respond(response);
}

/// 在已绑定的 server 上等待回调，返回授权码。
///
/// 只接受带 `code` 或 `error` 的请求，其余（favicon 等）回应 404 后继续等待。
fn wait_on_server(
    server: &tiny_http::Server,
    expected_state: &str,
    timeout: Duration,
) -> SyncResult<String> {
    let deadline = Instant::now() + timeout;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match server.recv_timeout(remaining) {
            Ok(Some(request)) => {
                let raw_url = request.url().to_owned();
                let path = raw_url.split('?').next().unwrap_or_default().to_owned();
                debug_log(&format!(
                    "incoming path={path} has_code={} has_error={}",
                    query_param(&raw_url, "code").is_some(),
                    query_param(&raw_url, "error").is_some()
                ));
                if let Some(error) = query_param(&raw_url, "error") {
                    respond_html(request, 400, "<h1>授权失败</h1><p>请在 LAGM 中重试。</p>");
                    return Err(SyncError::Api(format!("授权被拒绝：{error}")));
                }
                let Some(code) = query_param(&raw_url, "code") else {
                    respond_html(request, 404, "<h1>Not Found</h1>");
                    continue;
                };
                let state = query_param(&raw_url, "state").unwrap_or_default();
                if !expected_state.is_empty() && state != expected_state {
                    respond_html(request, 400, "<h1>state 校验失败</h1>");
                    return Err(SyncError::Api("回调 state 与请求不一致，已中止".to_owned()));
                }
                respond_html(
                    request,
                    200,
                    "<!doctype html><meta charset=\"utf-8\"><title>LAGM 授权完成</title>\
                     <h1>授权完成</h1><p>可以关闭此页面并返回 LAGM。</p>",
                );
                return Ok(code);
            }
            Ok(None) => break,
            Err(err) => return Err(SyncError::Internal(format!("接收回调失败：{err}"))),
        }
    }
    Err(SyncError::Api(
        "等待授权回调超时（请重试并在浏览器中完成授权）".to_owned(),
    ))
}

/// 绑定回调地址并等待授权码。
pub fn wait_for_callback(
    redirect_uri: &str,
    expected_state: &str,
    timeout: Duration,
) -> SyncResult<String> {
    debug_log(&format!("start: redirect_uri={redirect_uri}"));
    let server = match bind_server(redirect_uri) {
        Ok(server) => server,
        Err(err) => {
            debug_log(&format!("bind failed: {err}"));
            return Err(err);
        }
    };
    debug_log("listener bound, waiting for callback");
    let result = wait_on_server(&server, expected_state, timeout);
    match &result {
        Ok(code) => debug_log(&format!("done: received code (len={})", code.len())),
        Err(err) => debug_log(&format!("done with error: {err}")),
    }
    result
}

/// 用授权码换取 token。
pub async fn exchange_code(
    http: &reqwest::Client,
    config: &OAuthConfig,
    code: &str,
) -> SyncResult<TokenSet> {
    let params = [
        ("grant_type", "authorization_code"),
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("code", code),
        ("redirect_uri", config.redirect_uri.as_str()),
    ];
    post_token(http, &params).await
}

/// 用 refresh token 换新的 token 对。
pub async fn refresh(
    http: &reqwest::Client,
    config: &OAuthConfig,
    refresh_token: &str,
) -> SyncResult<TokenSet> {
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("refresh_token", refresh_token),
        ("redirect_uri", config.redirect_uri.as_str()),
    ];
    post_token(http, &params).await
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<i64>,
}

/// 表单编码抽成同步函数：`Serializer` 不是 `Send`，不能出现在 async 块里跨 await。
fn encode_form(params: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in params {
        serializer.append_pair(key, value);
    }
    serializer.finish()
}

async fn post_token(http: &reqwest::Client, params: &[(&str, &str)]) -> SyncResult<TokenSet> {
    let body = encode_form(params);

    let response = http
        .post(TOKEN_URL)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .body(body)
        .send()
        .await
        .map_err(|err| {
            SyncError::Api(format!(
                "请求 token 失败：{}",
                crate::http_client::describe_error(&err)
            ))
        })?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| SyncError::Api(format!("读取 token 响应失败：{err}")))?;
    if !status.is_success() {
        // invalid_grant 表示授权码或刷新令牌已被服务端判定无效：这不是网络问题，
        // 重试没有意义，只能让用户重新授权，因此映射成 NotAuthorized。
        if text.contains("invalid_grant") {
            return Err(SyncError::NotAuthorized);
        }
        return Err(SyncError::Api(format!(
            "token 接口返回 HTTP {status}：{text}"
        )));
    }
    let parsed: TokenResponse = serde_json::from_str(&text)
        .map_err(|err| SyncError::Api(format!("解析 token 响应失败：{err}（{text}）")))?;
    Ok(TokenSet {
        access_token: parsed.access_token,
        refresh_token: parsed.refresh_token,
        expires_at: expires_at(parsed.expires_in),
    })
}

fn expires_at(expires_in: Option<i64>) -> Option<String> {
    let seconds = expires_in?;
    Some(
        (chrono::Utc::now() + chrono::Duration::seconds(seconds))
            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    )
}

/// 用系统默认浏览器打开授权页。仅 Windows（plan.md 1.3）。
pub fn open_browser(url: &str) -> SyncResult<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
            .map(|_| ())
            .map_err(|err| SyncError::Internal(format!("打开浏览器失败：{err}")))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = url;
        Err(SyncError::Internal("当前仅支持 Windows".to_owned()))
    }
}

/// 避免并发刷新：调用方持有该锁再检查/刷新。
pub type RefreshLock = Mutex<()>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn config() -> OAuthConfig {
        OAuthConfig {
            client_id: "bgmtest".into(),
            client_secret: "secret".into(),
            redirect_uri: "http://127.0.0.1:27432/callback".into(),
        }
    }

    #[test]
    fn authorize_url_encodes_redirect_uri_and_state() {
        let url = build_authorize_url(&config(), "state-123");
        assert!(url.starts_with("https://bgm.tv/oauth/authorize?"));
        assert!(url.contains("client_id=bgmtest"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A27432%2Fcallback"));
        assert!(url.contains("state=state-123"));
    }

    #[test]
    fn query_param_extracts_code() {
        let code = query_param("/callback?code=abc123&state=xyz", "code");
        assert_eq!(code.as_deref(), Some("abc123"));
        assert_eq!(
            query_param("/callback?code=abc&state=xyz", "state").as_deref(),
            Some("xyz")
        );
        assert!(query_param("/favicon.ico", "code").is_none());
    }

    #[test]
    fn expires_at_is_in_the_future() {
        let value = expires_at(Some(604800)).expect("some");
        assert!(value.ends_with('Z'));
        assert!(expires_at(None).is_none());
    }

    #[test]
    fn extract_code_handles_url_path_and_raw() {
        assert_eq!(
            extract_code("http://127.0.0.1:27432/callback?code=abc&state=x").as_deref(),
            Some("abc")
        );
        assert_eq!(
            extract_code("/callback?code=abc123").as_deref(),
            Some("abc123")
        );
        assert_eq!(extract_code("raw-code-1").as_deref(), Some("raw-code-1"));
        assert_eq!(extract_code("   "), None);
        assert_eq!(
            extract_code("http://127.0.0.1:27432/callback?error=denied"),
            None
        );
    }

    #[test]
    fn callback_listener_receives_code() {
        // 固定高位端口；若被占用则跳过（避免环境相关失败）。
        let addr = "127.0.0.1:27499";
        let Ok(server) = tiny_http::Server::http(addr) else {
            return;
        };
        let handle =
            std::thread::spawn(move || wait_on_server(&server, "st", Duration::from_secs(5)));

        // 等监听就绪后发一个最小 HTTP 请求。
        let mut stream = None;
        for _ in 0..30 {
            if let Ok(s) = TcpStream::connect(addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut stream = stream.expect("应能连接回调端口");
        stream
            .write_all(b"GET /callback?code=CODE1&state=st HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).ok();

        assert_eq!(handle.join().expect("join").expect("code"), "CODE1");
        assert!(response.contains("200") || response.contains("授权完成"));
    }

    #[test]
    fn callback_listener_rejects_wrong_state() {
        let addr = "127.0.0.1:27500";
        let Ok(server) = tiny_http::Server::http(addr) else {
            return;
        };
        let handle =
            std::thread::spawn(move || wait_on_server(&server, "expected", Duration::from_secs(5)));
        let mut stream = None;
        for _ in 0..30 {
            if let Ok(s) = TcpStream::connect(addr) {
                stream = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut stream = stream.expect("连接");
        stream
            .write_all(b"GET /callback?code=C&state=wrong HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).ok();
        assert_eq!(
            handle.join().expect("join").expect_err("应拒绝").code(),
            "sync_api_error"
        );
    }
}
