//! PotPlayer 播放状态读取。
//!
//! PotPlayer 没有 HTTP/IPC 接口，但会响应窗口用户消息（WM_USER）：
//! - `0x5001` 播放状态（0 停止 / 1 暂停 / 2 播放中）
//! - `0x5002` 总时长（毫秒）
//! - `0x5004` 当前播放位置（毫秒）
//!
//! 位置由播放器自己上报，因此拖动进度条、跳转章节都能被读到；
//! 暂停时位置不再变化，天然得到「暂停不计入进度」。
//!
//! 为了不新增第三方依赖，这里直接用 user32 的 FFI：按进程号枚举顶层窗口，
//! 只认我们自己启动的那个实例，避免用户同时开了别的 PotPlayer 时读错。

use std::ffi::c_void;
use std::path::Path;

type Hwnd = *mut c_void;
type Bool = i32;

#[link(name = "user32")]
extern "system" {
    fn EnumWindows(callback: extern "system" fn(Hwnd, isize) -> Bool, lparam: isize) -> Bool;
    fn GetWindowThreadProcessId(hwnd: Hwnd, pid: *mut u32) -> u32;
    fn GetClassNameW(hwnd: Hwnd, buffer: *mut u16, max_count: i32) -> i32;
    fn SendMessageTimeoutW(
        hwnd: Hwnd,
        msg: u32,
        wparam: usize,
        lparam: isize,
        flags: u32,
        timeout_ms: u32,
        result: *mut usize,
    ) -> isize;
}

const WM_USER: u32 = 0x0400;
const PPM_GET_PLAYBACK_STATUS: usize = 0x5001;
const PPM_GET_TOTAL_TIME_MS: usize = 0x5002;
const PPM_GET_PLAYBACK_TIME_MS: usize = 0x5004;

/// 只发给未挂起的窗口，且最多等 200 毫秒，避免播放器忙时把轮询线程卡住。
const SMTO_ABORTIFHUNG: u32 = 0x0002;
const QUERY_TIMEOUT_MS: u32 = 200;

/// PotPlayer 的窗口类名，64 位与旧版本各一个。
const WINDOW_CLASSES: [&str; 2] = ["PotPlayer64", "PotPlayer"];

/// 一次读取到的播放状态。
///
/// 注意：`0x5001` 的返回值在不同 PotPlayer 版本语义不一致（实测某些版本播放中恒为 0），
/// 因此这里不暴露「是否暂停」，只以「能读到时长」判定播放器是否已加载媒体。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlaybackState {
    pub position_ms: u64,
    pub duration_ms: u64,
}

/// 是否是 PotPlayer（按可执行文件名判断）。
pub fn is_potplayer(exe: &Path) -> bool {
    exe.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_lowercase().contains("potplayer"))
        .unwrap_or(false)
}

/// 读取指定进程对应的 PotPlayer 播放状态；读不到（窗口未就绪 / 未加载媒体）返回 None。
pub fn query(pid: u32) -> Option<PlaybackState> {
    let hwnd = match find_window(pid) {
        Some(hwnd) => hwnd,
        None => {
            tracing::debug!(pid, "未找到 PotPlayer 窗口");
            return None;
        }
    };
    // SAFETY: hwnd 来自当前进程内的 EnumWindows 结果，发送的是只读查询消息，无副作用。
    unsafe {
        let duration = send_query(hwnd, PPM_GET_TOTAL_TIME_MS)?.max(0);
        if duration == 0 {
            tracing::debug!(pid, "PotPlayer 尚未加载媒体");
            return None;
        }
        let Some(position) = send_query(hwnd, PPM_GET_PLAYBACK_TIME_MS) else {
            tracing::debug!(pid, "读取播放位置超时");
            return None;
        };
        Some(PlaybackState {
            position_ms: position.max(0) as u64,
            duration_ms: duration as u64,
        })
    }
}

/// 发送一次查询消息，超时或失败返回 None。
///
/// # Safety
/// `hwnd` 必须是有效的顶层窗口句柄。
unsafe fn send_query(hwnd: Hwnd, message: usize) -> Option<isize> {
    let mut result: usize = 0;
    let ok = SendMessageTimeoutW(
        hwnd,
        WM_USER,
        message,
        0,
        SMTO_ABORTIFHUNG,
        QUERY_TIMEOUT_MS,
        &mut result,
    );
    if ok == 0 {
        return None;
    }
    Some(result as isize)
}

/// 诊断用：能否按进程号找到 PotPlayer 窗口。
pub fn window_found(pid: u32) -> bool {
    find_window(pid).is_some()
}

/// 诊断用：原样返回 (播放状态, 当前位置毫秒, 总时长毫秒)，不做任何过滤。
pub fn raw_status(pid: u32) -> Option<(isize, isize, isize)> {
    let hwnd = find_window(pid)?;
    // SAFETY: 同 query，只读查询。
    unsafe {
        Some((
            send_query(hwnd, PPM_GET_PLAYBACK_STATUS).unwrap_or(-1),
            send_query(hwnd, PPM_GET_PLAYBACK_TIME_MS).unwrap_or(-1),
            send_query(hwnd, PPM_GET_TOTAL_TIME_MS).unwrap_or(-1),
        ))
    }
}

fn find_window(pid: u32) -> Option<Hwnd> {
    let mut context = SearchContext {
        pid,
        found: std::ptr::null_mut(),
    };

    // SAFETY: 回调只写 context，生命周期覆盖整个 EnumWindows 调用。
    unsafe {
        EnumWindows(enum_window, &mut context as *mut SearchContext as isize);
    }
    Some(context.found).filter(|hwnd| !hwnd.is_null())
}

struct SearchContext {
    pid: u32,
    found: Hwnd,
}

extern "system" fn enum_window(hwnd: Hwnd, lparam: isize) -> Bool {
    if hwnd.is_null() || lparam == 0 {
        return 1;
    }
    // SAFETY: lparam 来自 find_window 传入的 SearchContext 指针，在回调期间有效。
    let context = unsafe { &mut *(lparam as *mut SearchContext) };

    let mut window_pid = 0u32;
    // SAFETY: hwnd 由系统传入，参数合法。
    unsafe {
        GetWindowThreadProcessId(hwnd, &mut window_pid);
    }
    if window_pid != context.pid {
        return 1;
    }
    if !class_matches(hwnd) {
        return 1;
    }

    context.found = hwnd;
    0 // 找到即停止枚举
}

fn class_matches(hwnd: Hwnd) -> bool {
    let mut buffer = [0u16; 64];
    // SAFETY: buffer 长度与传入的容量一致。
    let length = unsafe { GetClassNameW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32) };
    if length <= 0 {
        return false;
    }
    let class = String::from_utf16_lossy(&buffer[..length as usize]);
    WINDOW_CLASSES
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(&class))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_potplayer_by_executable_name() {
        assert!(is_potplayer(Path::new(
            "C:/Program Files/DAUM/PotPlayer/PotPlayerMini64.exe"
        )));
        assert!(is_potplayer(Path::new("D:/tools/potplayermini.exe")));
        assert!(!is_potplayer(Path::new("C:/Program Files/mpv/mpv.exe")));
        assert!(!is_potplayer(Path::new(
            "C:/Program Files/VideoLAN/VLC/vlc.exe"
        )));
    }

    #[test]
    fn query_returns_none_for_non_player_process() {
        // 当前测试进程不是 PotPlayer，应当安全地返回 None 而不是 panic。
        assert!(query(std::process::id()).is_none());
    }
}
