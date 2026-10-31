//! 出站代理解析。
//!
//! 国内网络访问 bgm.tv 常需代理，而桌面端与浏览器不同：reqwest 不会读取 Windows
//! 的「系统代理」设置。这里按「环境变量 → Windows 系统代理 → 无」的顺序解析，
//! 也允许用户在设置里显式指定（优先级最高，由调用方传入）。

/// 是否显式代理：用于日志与提示。
pub fn describe(proxy: Option<&str>) -> &'static str {
    if proxy.is_some() {
        "已配置代理"
    } else {
        "直连"
    }
}

/// 解析代理地址：环境变量优先，其次 Windows 系统代理。
pub fn resolve_proxy() -> Option<String> {
    if let Some(value) = env_proxy() {
        return Some(value);
    }
    #[cfg(target_os = "windows")]
    {
        windows_system_proxy()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

fn env_proxy() -> Option<String> {
    for key in ["HTTPS_PROXY", "https_proxy", "ALL_PROXY", "all_proxy"] {
        if let Ok(raw) = std::env::var(key) {
            let value = raw.trim();
            if !value.is_empty() {
                return Some(normalize(value));
            }
        }
    }
    None
}

/// 补全 scheme：`127.0.0.1:7890` → `http://127.0.0.1:7890`。
fn normalize(raw: &str) -> String {
    if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    }
}

/// 从 `ProxyServer` 取值里挑出 https 那一项。
///
/// 该值可能是 `127.0.0.1:7890`，也可能是 `http=127.0.0.1:7890;https=127.0.0.1:7891`。
fn pick_proxy_server(raw: &str) -> String {
    let raw = raw.trim();
    if !raw.contains('=') {
        return raw.to_owned();
    }
    let entries: Vec<(&str, &str)> = raw
        .split(';')
        .filter_map(|part| part.split_once('='))
        .collect();
    entries
        .iter()
        .find(|(scheme, _)| scheme.eq_ignore_ascii_case("https"))
        .or_else(|| entries.first())
        .map(|(_, value)| value.trim().to_owned())
        .unwrap_or_else(|| raw.to_owned())
}

#[cfg(target_os = "windows")]
fn windows_system_proxy() -> Option<String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(
            r"Software\Microsoft\Windows\CurrentVersion\Internet Settings",
            KEY_READ,
        )
        .ok()?;
    let enabled: u32 = key.get_value("ProxyEnable").unwrap_or(0);
    if enabled == 0 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let server = pick_proxy_server(&server);
    if server.is_empty() {
        return None;
    }
    Some(normalize(&server))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_adds_default_scheme() {
        assert_eq!(normalize("127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(
            normalize("socks5://127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
    }

    #[test]
    fn pick_prefers_https_entry() {
        assert_eq!(pick_proxy_server("127.0.0.1:7890"), "127.0.0.1:7890");
        assert_eq!(
            pick_proxy_server("http=127.0.0.1:7890;https=127.0.0.1:7891"),
            "127.0.0.1:7891"
        );
        // 没有 https 项时退回第一项。
        assert_eq!(
            pick_proxy_server("http=127.0.0.1:7890;ftp=127.0.0.1:7892"),
            "127.0.0.1:7890"
        );
    }

    #[test]
    fn describe_reflects_state() {
        assert_eq!(describe(None), "直连");
        assert_eq!(describe(Some("http://x")), "已配置代理");
    }
}
