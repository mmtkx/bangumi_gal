use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use sysinfo::{ProcessesToUpdate, System};

use crate::error::{LauncherError, Result};

/// Windows：让游戏进入独立进程组，避免宿主进程的 Ctrl+C 等控制事件影响它。
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 启动可执行文件，返回 pid。
///
/// 丢弃 `Child` 不会结束进程；运行状态由进程表轮询判定，因此不需要持有句柄。
pub fn spawn(exe: &Path, args: &[String], working_dir: &Path) -> Result<u32> {
    let mut command = Command::new(exe);
    command.args(args).current_dir(working_dir);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(CREATE_NEW_PROCESS_GROUP);
    }

    let child = command.spawn().map_err(|err| LauncherError::Spawn {
        path: exe.to_path_buf(),
        reason: err.to_string(),
    })?;
    Ok(child.id())
}

/// 抓取一份 pid -> 父 pid 快照。
///
/// 取不到进程表时返回 None，调用方应视为「本轮状态未知」而不是「全部已退出」，
/// 否则一次采样失败会把运行中的游戏误判成已退出。
pub fn process_table() -> Option<HashMap<u32, Option<u32>>> {
    let mut system = System::new().ok()?;
    system.refresh_processes(ProcessesToUpdate::All, true);
    Some(
        system
            .processes()
            .iter()
            .map(|(pid, process)| (pid.as_u32(), process.parent().map(|parent| parent.as_u32())))
            .collect(),
    )
}

/// 从一组根进程出发，收集仍然存活的可达进程（含根自身与全部后代）。
///
/// 游戏常见形态是启动器先起一个进程、再拉起真正的游戏进程；只要树里还有活着的节点，
/// 就认为该游戏仍在运行。
pub fn reachable(roots: &[u32], table: &HashMap<u32, Option<u32>>) -> Vec<u32> {
    let mut alive: Vec<u32> = roots
        .iter()
        .copied()
        .filter(|pid| table.contains_key(pid))
        .collect();

    loop {
        let mut added = false;
        for (pid, parent) in table {
            if let Some(parent) = parent {
                if alive.contains(parent) && !alive.contains(pid) {
                    alive.push(*pid);
                    added = true;
                }
            }
        }
        if !added {
            break;
        }
    }
    alive
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(rows: &[(u32, Option<u32>)]) -> HashMap<u32, Option<u32>> {
        rows.iter().copied().collect()
    }

    #[test]
    fn includes_descendants_of_root() {
        let table = table(&[(100, None), (101, Some(100)), (102, Some(101)), (200, None)]);
        let mut alive = reachable(&[100], &table);
        alive.sort_unstable();
        assert_eq!(alive, vec![100, 101, 102]);
    }

    #[test]
    fn empty_when_root_is_gone() {
        let table = table(&[(101, None)]);
        assert!(reachable(&[100], &table).is_empty());
    }

    #[test]
    fn keeps_orphaned_children_that_were_already_tracked() {
        // 启动器 100 退出，真正的游戏 101 还在；101 已在 tracked 中，因此仍视为运行。
        let table = table(&[(101, None)]);
        assert_eq!(reachable(&[100, 101], &table), vec![101]);
    }
}
