//! 组合根：装配 library-core 与各功能 crate，向上暴露 Tauri command 与事件桥。
//!
//! 依赖方向（plan.md 3.2）：app-services → 功能 crate → library-core。
//! 功能 crate 之间不直接依赖，只经 library-core 的 trait 与事件总线协作。

pub mod bridge;
pub mod builtin;
pub mod commands;
pub mod cover;
pub mod delete;
pub mod dirs;
pub mod dto;
pub mod error;
pub mod import;
pub mod logs;
pub mod services;
pub mod settings;

pub use commands::{
    active_sessions, auto_match, bangumi_account, bangumi_authorize, bangumi_complete_manual,
    bangumi_logout, bangumi_status, bind_bangumi_subject, cache_metadata, clear_items,
    clear_metadata_cache, delete_item, discard_sync_entry, game_status, get_item, get_settings,
    item_cover, list_conflicts, list_exe_files, list_items, list_progress, list_subdirs,
    list_sync_queue, log_location, metadata_cache_stats, open_in_explorer, open_log_dir,
    play_episode, play_next_episode, plugin_emit, plugin_install, plugin_invoke, plugin_list,
    plugin_read_items, plugin_set_enabled, plugin_ui_entry, plugin_ui_unloaded, plugin_uninstall,
    pull_item_now, push_pending, refresh_metadata_batch, resolve_conflicts, running_games,
    save_bangumi_config, save_settings, scan_library, search_bangumi, set_collection,
    set_game_binding, set_progress, start_game, sync_pull, sync_stats, trash_items, ui_ready,
};
pub use delete::DeleteMode;
pub use error::{ApiResult, AppError};
pub use logs::{default_log_dir, LOG_FILE_NAME, LOG_KEEP_FILES, LOG_MAX_BYTES};
pub use services::Services;
pub use settings::{default_metadata_dir, SettingsInput};
