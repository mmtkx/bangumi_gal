// 发布构建不弹控制台窗口；调试构建保留控制台以便看 tracing 输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use app_services::{bridge, commands, Services};
use tauri::Manager;

mod logging;

fn main() {
    // 日志在拿到 Tauri 路径之前就要可用：目录按 ADR-009 的同级约定自行解析。
    let log_dir = app_services::logs::default_log_dir();
    let file_logging = logging::init(&log_dir);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        log_dir = %log_dir.display(),
        file_logging,
        "应用启动"
    );

    let result = tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app
                .path()
                .app_data_dir()
                .map_err(|err| std::io::Error::other(format!("无法解析应用数据目录：{err}")))?;
            std::fs::create_dir_all(&data_dir)?;

            let services = Services::new(&data_dir.join("library.db"))
                .map_err(|err| std::io::Error::other(format!("初始化服务失败：{err}")))?;
            tracing::info!(db = %data_dir.join("library.db").display(), "数据库就绪");

            bridge::spawn_event_bridge(handle, services.bus().clone());
            let services = Arc::new(services);
            // 游戏进程树与播放会话的常驻轮询任务。
            services.spawn_background_tasks();
            app.manage(services);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::get_settings,
            commands::save_settings,
            commands::scan_library,
            commands::list_items,
            commands::get_item,
            commands::bind_bangumi_subject,
            commands::list_subdirs,
            commands::list_exe_files,
            commands::game_status,
            commands::set_game_binding,
            commands::start_game,
            commands::running_games,
            commands::active_sessions,
            commands::play_episode,
            commands::play_next_episode,
            commands::item_cover,
            commands::item_covers,
            commands::import_drop,
            commands::list_progress,
            commands::set_progress,
            commands::set_collection,
            commands::list_sync_queue,
            commands::sync_stats,
            commands::push_pending,
            commands::sync_pull,
            commands::list_conflicts,
            commands::resolve_conflicts,
            commands::cache_metadata,
            commands::bangumi_status,
            commands::bangumi_account,
            commands::bangumi_subject_preview,
            commands::open_external,
            commands::save_bangumi_config,
            commands::bangumi_authorize,
            commands::bangumi_logout,
            commands::bangumi_complete_manual,
            commands::auto_match,
            commands::search_bangumi,
            commands::plugin_list,
            commands::plugin_install,
            commands::plugin_set_enabled,
            commands::plugin_uninstall,
            commands::plugin_ui_entry,
            commands::plugin_read_items,
            commands::plugin_emit,
            commands::plugin_ui_unloaded,
            commands::plugin_invoke,
            commands::delete_item,
            commands::clear_items,
            commands::log_location,
            commands::metadata_cache_stats,
            commands::clear_metadata_cache,
            commands::open_log_dir,
            commands::open_in_explorer,
            commands::pull_item_now,
            commands::refresh_metadata_batch,
            commands::trash_items,
            commands::discard_sync_entry,
            commands::ui_ready,
        ])
        .run(tauri::generate_context!());

    if let Err(err) = result {
        tracing::error!(error = %err, "应用退出（异常）");
        eprintln!("应用启动失败：{err}");
    }
}
