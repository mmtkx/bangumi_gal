// 发布构建不弹控制台窗口；调试构建保留控制台以便看 tracing 输出。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::Arc;

use app_services::{bridge, commands, Services};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager};

mod logging;

/// 托盘专用小图标：32×32、配色沿用应用图标，图形简化为圆角方块 + 播放三角。
/// 直接用应用图标（插画风格）缩到 16～24px 会糊成一团，所以单独做一张。
const TRAY_ICON_PNG: &[u8] = include_bytes!("../icons/tray.png");

/// 把主窗口显示出来并置前。托盘左键、托盘菜单都走这里。
fn show_main_window(app: &AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        tracing::warn!("找不到主窗口，无法从托盘唤回");
        return;
    };
    if let Err(err) = window.show() {
        tracing::warn!(error = %err, "显示主窗口失败");
    }
    if let Err(err) = window.unminimize() {
        tracing::warn!(error = %err, "取消最小化失败");
    }
    if let Err(err) = window.set_focus() {
        tracing::warn!(error = %err, "聚焦主窗口失败");
    }
}

/// 托盘图标与菜单：左键唤回窗口，菜单提供「显示」与「退出」。
///
/// 没有托盘就没有「留驻」的意义——窗口一隐藏，用户就再也找不到应用了，
/// 所以托盘与关闭行为总是一起存在，只在关闭那一刻按设置决定是隐藏还是退出。
fn setup_tray(app: &tauri::App) -> tauri::Result<()> {
    let show_item = MenuItem::with_id(app, "show", "显示主窗口", true, None::<&str>)?;
    let quit_item = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

    let mut builder = TrayIconBuilder::with_id("main")
        .tooltip("LAGM · 本地动画 / 游戏库")
        // 左键直接唤回窗口更符合直觉，右键才出菜单。
        .show_menu_on_left_click(false)
        .menu(&menu)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main_window(app),
            "quit" => {
                tracing::info!("从托盘退出应用");
                // 退出前把进度落一次库，别让最后一集的位置停在上一秒。
                if let Some(services) = app.try_state::<Arc<Services>>() {
                    services.flush_before_exit();
                }
                app.exit(0);
            }
            other => tracing::warn!(id = other, "未知的托盘菜单项"),
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });

    match tauri::image::Image::from_bytes(TRAY_ICON_PNG) {
        Ok(icon) => builder = builder.icon(icon),
        Err(err) => {
            // 图标解不出来不该让应用起不来：退回应用默认图标。
            tracing::warn!(error = %err, "托盘图标解析失败，改用应用默认图标");
            if let Some(icon) = app.default_window_icon().cloned() {
                builder = builder.icon(icon);
            }
        }
    }
    builder.build(app)?;
    Ok(())
}

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
        // 单实例插件必须是第一个注册的：第二次启动时它会在已有实例里回调，
        // 然后把新进程关掉。没有它，留驻托盘期间双击 exe 就会开出第二个进程抢同一个库。
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            tracing::info!("检测到应用已在运行，唤回已有窗口");
            show_main_window(app);
        }))
        // 更新器：端点与公钥在 tauri.conf.json 的 plugins.updater 里，
        // 私钥只在打包时通过 TAURI_SIGNING_PRIVATE_KEY 提供，绝不进仓库。
        .plugin(tauri_plugin_updater::Builder::new().build())
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
            let close_services = Arc::clone(&services);
            app.manage(Arc::clone(&services));

            // 关闭主窗口：留驻模式下只隐藏窗口并阻止关闭，后台轮询继续跑；
            // 关掉留驻则什么也不做，交给默认行为——最后一个窗口关闭即退出进程。
            let window = app
                .get_webview_window("main")
                .ok_or_else(|| std::io::Error::other("找不到主窗口"))?;
            let closing_window = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    if close_services.close_to_tray() {
                        api.prevent_close();
                        if let Err(err) = closing_window.hide() {
                            tracing::warn!(error = %err, "隐藏主窗口失败");
                        }
                        tracing::info!("主窗口已隐藏，应用留驻托盘");
                    }
                }
            });

            setup_tray(app)?;
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
            commands::check_update,
            commands::install_update,
            commands::app_version,
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
