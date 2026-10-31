//! 日志初始化：控制台 + 按大小/日期滚动的文件输出（M5）。
//!
//! 滚动规则由 `rotation_reason` 表达（纯函数、可单测）；文件名与限额来自
//! `app_services::logs`，设置页展示的是同一份常量。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use app_services::logs::{LOG_FILE_NAME, LOG_KEEP_FILES, LOG_MAX_BYTES};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::prelude::*;
use tracing_subscriber::EnvFilter;

/// 需要滚动的原因；`None` 表示当前文件还能继续追加。
fn rotation_reason(
    current_day: &str,
    today: &str,
    written: u64,
    incoming: usize,
    max_bytes: u64,
) -> Option<&'static str> {
    if current_day != today {
        // 跨天：即使没写满也换文件，便于按天检索。
        return Some("date");
    }
    if written > 0 && written + incoming as u64 > max_bytes {
        return Some("size");
    }
    None
}

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

struct State {
    dir: PathBuf,
    file: File,
    written: u64,
    day: String,
}

impl State {
    fn rotate(&mut self) {
        let _ = self.file.flush();
        let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S").to_string();
        let mut archive = self.dir.join(format!("lagm-{stamp}.log"));
        let mut index = 1;
        while archive.exists() {
            archive = self.dir.join(format!("lagm-{stamp}-{index}.log"));
            index += 1;
        }
        // 归档失败也要继续用新文件写，否则日志会一直涨下去。
        let _ = fs::rename(self.dir.join(LOG_FILE_NAME), &archive);
        if let Ok(file) = open_log(self.dir.join(LOG_FILE_NAME)) {
            self.file = file;
            self.written = 0;
        }
        self.day = today();
        prune(&self.dir);
    }
}

fn open_log(path: PathBuf) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

/// 只保留最近 `LOG_KEEP_FILES` 个归档，当前文件永不删除。
fn prune(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut archives: Vec<(SystemTime, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_string_lossy().to_string();
            if name == LOG_FILE_NAME || !name.starts_with("lagm-") || !name.ends_with(".log") {
                return None;
            }
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((modified, path))
        })
        .collect();
    archives.sort_by_key(|left| std::cmp::Reverse(left.0));
    for (_, path) in archives.into_iter().skip(LOG_KEEP_FILES) {
        let _ = fs::remove_file(path);
    }
}

/// 供 tracing 使用的写入器：每次写入前判断是否该滚动。
#[derive(Clone)]
pub struct RollingWriter {
    state: Arc<Mutex<State>>,
}

impl RollingWriter {
    pub fn new(dir: &Path) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(LOG_FILE_NAME);
        let file = open_log(path)?;
        let written = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        Ok(Self {
            state: Arc::new(Mutex::new(State {
                dir: dir.to_path_buf(),
                file,
                written,
                day: today(),
            })),
        })
    }
}

impl Write for RollingWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let Ok(mut state) = self.state.lock() else {
            return Err(io::Error::other("日志写入锁已损坏"));
        };
        if rotation_reason(
            &state.day,
            &today(),
            state.written,
            buf.len(),
            LOG_MAX_BYTES,
        )
        .is_some()
        {
            state.rotate();
        }
        state.file.write_all(buf)?;
        state.written += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.state.lock() {
            Ok(mut state) => state.file.flush(),
            Err(_) => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for RollingWriter {
    type Writer = RollingWriter;

    fn make_writer(&self) -> Self::Writer {
        self.clone()
    }
}

/// 初始化日志。返回是否成功落盘到文件（失败时仅控制台，不影响启动）。
pub fn init(log_dir: &Path) -> bool {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,lagm_desktop=debug,app_services=debug"));

    let file_layer = match RollingWriter::new(log_dir) {
        Ok(writer) => Some(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_ansi(false)
                .with_writer(writer),
        ),
        Err(err) => {
            eprintln!("日志文件不可用（{err}），本次仅输出到控制台");
            None
        }
    };
    let to_file = file_layer.is_some();

    let registry = tracing_subscriber::registry().with(filter).with(
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_writer(io::stdout),
    );
    if let Err(err) = registry.with(file_layer).try_init() {
        eprintln!("日志初始化失败：{err}");
        return false;
    }

    // panic 也要留下痕迹：发布版没有控制台，只能靠文件。
    std::panic::set_hook(Box::new(|info| {
        tracing::error!(panic = %info, "未捕获的 panic");
        eprintln!("未捕获的 panic：{info}");
    }));

    to_file
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lagm-logging-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        dir
    }

    #[test]
    fn rotation_reason_covers_date_size_and_room() {
        assert_eq!(
            rotation_reason("2026-10-29", "2026-10-30", 0, 10, 100),
            Some("date")
        );
        assert_eq!(
            rotation_reason("2026-10-30", "2026-10-30", 95, 10, 100),
            Some("size")
        );
        // 空文件即使单条超过上限也先写进去，避免无限滚动。
        assert_eq!(
            rotation_reason("2026-10-30", "2026-10-30", 0, 500, 100),
            None
        );
        assert_eq!(
            rotation_reason("2026-10-30", "2026-10-30", 10, 10, 100),
            None
        );
    }

    #[test]
    fn writer_rotates_and_keeps_archive_count() {
        let dir = temp_dir("rotate");
        let writer = RollingWriter::new(&dir).expect("writer");

        // 每次写 1 MiB，触发多次滚动（上限 5 MiB，保留 10 个归档）。
        let chunk = vec![b'x'; 1024 * 1024];
        {
            let mut handle = writer.clone();
            for _ in 0..40 {
                handle.write_all(&chunk).expect("write");
            }
            handle.flush().expect("flush");
        }

        let entries: Vec<String> = fs::read_dir(&dir)
            .expect("read")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .collect();
        let archives = entries
            .iter()
            .filter(|name| name.as_str() != LOG_FILE_NAME)
            .count();
        assert!(entries.iter().any(|name| name == LOG_FILE_NAME));
        assert!(archives > 0, "应产生归档文件");
        assert!(
            archives <= LOG_KEEP_FILES,
            "归档数量应受限，实际 {archives}"
        );
        let current = fs::metadata(dir.join(LOG_FILE_NAME))
            .expect("current")
            .len();
        assert!(current <= LOG_MAX_BYTES, "当前文件不应超过上限：{current}");

        fs::remove_dir_all(&dir).ok();
    }
}
