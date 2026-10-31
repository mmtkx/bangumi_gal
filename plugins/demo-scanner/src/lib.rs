//! 示例能力插件：从动画文件名解析标题、季、集号。
//!
//! 这是给插件作者的最小可运行样例，刻意不依赖任何 crate：
//! - 只使用 `lagm` 模块的 `log` 导入（无需任何权限）；
//! - 输入输出都是 JSON 文本，通过线性内存传递，避免绑定生成器。
//!
//! 构建：
//! ```bash
//! rustup target add wasm32-unknown-unknown
//! cargo build --release --target wasm32-unknown-unknown
//! cp target/wasm32-unknown-unknown/release/demo_scanner.wasm main.wasm
//! ```

use std::slice;

// 宿主提供的日志导入（level：0 debug、1 info、2 warn、3 error）。
#[link(wasm_import_module = "lagm")]
extern "C" {
    fn log(level: i32, ptr: *const u8, len: usize);
}

/// 宿主写输入前调用，返回可写缓冲区指针。
///
/// 每次调用都会泄漏一小块内存：宿主约定一次调用只分配一次，
/// 插件实例也随调用结束而销毁，因此不需要回收逻辑。
#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buffer: Vec<u8> = Vec::with_capacity(len.max(1));
    let ptr = buffer.as_mut_ptr();
    std::mem::forget(buffer);
    ptr
}

/// 唯一入口：读取输入 JSON，返回 `(ptr << 32) | len` 指向输出 JSON。
#[no_mangle]
pub extern "C" fn invoke(ptr: *const u8, len: usize) -> u64 {
    let input = if ptr.is_null() || len == 0 {
        String::new()
    } else {
        // 宿主保证 ptr/len 落在插件线性内存内。
        String::from_utf8_lossy(unsafe { slice::from_raw_parts(ptr, len) }).into_owned()
    };

    let file = json_string_field(&input, "file").unwrap_or_default();
    emit_log(1, &format!("解析文件名：{file}"));

    let parsed = parse_file_name(&file);
    if parsed.episode == 0 {
        emit_log(2, "没有识别到集号，已回退为整名标题");
    }

    let output = format!(
        "{{\"title\":\"{}\",\"season\":{},\"episode\":{},\"confidence\":{},\"source\":\"demo-scanner\"}}",
        escape_json(&parsed.title),
        parsed.season,
        parsed.episode,
        if parsed.episode > 0 { "0.8" } else { "0.3" },
    );
    write_output(&output)
}

fn emit_log(level: i32, message: &str) {
    unsafe { log(level, message.as_ptr(), message.len()) };
}

fn write_output(text: &str) -> u64 {
    let mut buffer = text.as_bytes().to_vec();
    let len = buffer.len() as u64;
    let ptr = buffer.as_mut_ptr() as u64;
    std::mem::forget(buffer);
    (ptr << 32) | len
}

struct Parsed {
    title: String,
    season: u32,
    episode: u32,
}

/// 解析规则与核心扫描器保持同一思路，但刻意做得更简单：
/// 去扩展名 → 去括号噪声 → 取最后一段「- 集号」→ 其余作标题。
fn parse_file_name(file: &str) -> Parsed {
    let base = file
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(file)
        .to_owned();
    let stem = match base.rfind('.') {
        Some(idx) => base[..idx].to_owned(),
        None => base,
    };

    let cleaned = strip_brackets(&stem);
    let season = detect_season(&cleaned);

    let (mut title, tail) = match cleaned.rfind(" - ") {
        Some(idx) => (
            cleaned[..idx].to_owned(),
            cleaned[idx + 3..].to_owned(),
        ),
        None => (cleaned.clone(), String::new()),
    };
    let mut episode = first_number(&tail);

    if episode == 0 {
        // 没有「- 集号」时，退化为「最后一段数字即集号」。
        if let Some((head, number)) = split_last_number(&cleaned) {
            title = head;
            episode = number;
        }
    }

    let title = trim_noise(&title);
    Parsed {
        title: if title.is_empty() {
            trim_noise(&stem)
        } else {
            title
        },
        season,
        episode,
    }
}

/// 丢掉 `[...]`、`{...}`、`(...)` 及其全角写法里的内容（发布组、分辨率等）。
fn strip_brackets(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut depth = 0usize;
    for ch in input.chars() {
        match ch {
            '[' | '{' | '(' | '【' | '（' => depth += 1,
            ']' | '}' | ')' | '】' | '）' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

/// 识别 `Season 2` / `S2` / `第二季` 这类季数标记，默认第 1 季。
fn detect_season(input: &str) -> u32 {
    let lower = input.to_lowercase();
    if let Some(idx) = lower.find("season") {
        let digits = first_number(&lower[idx + "season".len()..]);
        if digits > 0 {
            return digits;
        }
    }
    // 只认独立成词的「s2」：前面必须是分隔符，避免把 Show 的 s 当成标记。
    let bytes = lower.as_bytes();
    for idx in 0..bytes.len() {
        if bytes[idx] != b's' {
            continue;
        }
        let before_ok = idx == 0
            || matches!(
                bytes[idx - 1],
                b' ' | b'-' | b'_' | b'.' | b'[' | b'(' | b'+'
            );
        if !before_ok {
            continue;
        }
        let digits = first_number(&lower[idx + 1..]);
        if digits > 0 && digits < 100 {
            return digits;
        }
    }
    1
}

fn first_number(input: &str) -> u32 {
    let digits: String = input
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}

fn split_last_number(input: &str) -> Option<(String, u32)> {
    let mut end = None;
    let mut start = None;
    for (idx, ch) in input.char_indices().rev() {
        if ch.is_ascii_digit() {
            end = Some(idx + ch.len_utf8());
            start = Some(idx);
        } else if end.is_some() {
            break;
        }
    }
    let end = end?;
    let start = start?;
    let number = input[start..end].parse().ok()?;
    Some((input[..start].to_owned(), number))
}

fn trim_noise(input: &str) -> String {
    input
        .trim_matches(|ch: char| {
            ch.is_whitespace() || matches!(ch, '-' | '_' | '.' | '~' | '：' | ':')
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// 极简 JSON 字符串取值：够用即可，不做完整解析。
fn json_string_field(raw: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut rest = &raw[raw.find(&needle)? + needle.len()..];
    rest = &rest[rest.find(':')? + 1..];
    rest = rest.trim_start();
    rest = rest.strip_prefix('"')?;
    let mut escaped = false;
    let mut out = String::new();
    for ch in rest.chars() {
        if escaped {
            out.push(match ch {
                'n' => '\n',
                't' => '\t',
                other => other,
            });
            escaped = false;
            continue;
        }
        match ch {
            '\\' => escaped = true,
            '"' => return Some(out),
            other => out.push(other),
        }
    }
    None
}

fn escape_json(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out
}
