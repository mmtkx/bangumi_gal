use chrono::{SecondsFormat, Utc};

/// 统一时间戳格式：UTC RFC3339，秒精度，例如 `2026-10-29T09:44:18Z`。
///
/// 全库只使用这一种格式，字符串比较即时间比较，避免 SQLite 侧再做时间解析。
pub fn now_iso8601() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}
