//! 条目模型、SQLite 访问与迁移、主干道事件总线定义、公共错误。
//!
//! 依赖规则（plan.md 3.2）：本 crate 不依赖任何功能 crate，只被功能 crate 依赖。

pub mod binding;
pub mod db;
pub mod error;
pub mod events;
pub mod metadata;
pub mod migrate;
pub mod model;
pub mod paths;
pub mod plugin;
pub mod progress;
pub mod repo;
pub mod settings;
pub mod sync;
pub mod time;

pub use binding::{GameBindingRepository, SqliteGameBindingRepository};
pub use db::Db;
pub use error::{CoreError, Result};
pub use events::{AppEvent, EventBus, EventEnvelope, Topic};
pub use metadata::{MetadataIndexRepository, SqliteMetadataIndexRepository};
pub use model::{
    Conflict, GameBindingIndex, ItemKind, LibraryItem, MediaFile, MediaProbe, MetadataIndex,
    NewConflict, NewItem, PluginRegistryEntry, Progress, ProgressSource, ScanReport,
    SyncQueueEntry, SyncStatus, UpsertOutcome,
};
pub use plugin::{PluginRegistryRepository, SqlitePluginRegistryRepository};
pub use progress::{ProgressRepository, SqliteProgressRepository};
pub use repo::{LibraryRepository, SqliteRepository};
pub use sync::{
    ConflictRepository, SqliteConflictRepository, SqliteSyncQueueRepository, SyncQueueRepository,
};
