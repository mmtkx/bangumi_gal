//! Bangumi OAuth、API 客户端、拉取、推送队列与冲突计算（M3 实现）。
//!
//! 当前为 M3 离线骨架：同步队列/冲突仓储在 library-core，限速、冲突检测、
//! 拉取比较与推送编排在本 crate。网络实现以 `BangumiClient` 抽象，骨架阶段使用
//! `OfflineClient`，因此全部逻辑可在无网络环境下测试。OAuth 与真实 HTTP 客户端
//! 待联网阶段接入，不改变上层接口。

pub mod client;
pub mod conflict;
pub mod engine;
pub mod error;
pub mod http_client;
pub mod matching;
pub mod metadata;
pub mod oauth;
pub mod proxy;
pub mod rate_limit;
pub mod title_table;
pub mod token;
pub mod types;

pub use client::{BangumiClient, BoxFut, OfflineClient, PushCall};
pub use conflict::{detect_collection_conflicts, detect_progress_conflicts, watched_label};
pub use engine::{PullReport, PushReport, SyncEngine, SyncStats};
pub use error::{SyncError, SyncResult};
pub use http_client::BangumiHttpClient;
pub use matching::{
    best_match, normalize_key, normalize_title, search_keywords, season_of, MatchCandidate,
    AUTO_BIND_THRESHOLD,
};
pub use metadata::{cover_url, MetadataCache};
pub use oauth::{
    build_authorize_url, exchange_code, extract_code, open_browser, wait_for_callback, OAuthConfig,
    StaticConfigProvider, SyncConfigProvider,
};
pub use rate_limit::{backoff_delay, RateLimiter};
pub use title_table::native_title_for;
pub use token::{InMemoryTokenStore, KeyringTokenStore, TokenSet, TokenStore};
pub use types::{
    AccountProfile, CloudCollection, CloudEpisodeProgress, CollectionPayload, LocalCollection,
    LocalProgress, ProgressPayload, RelatedSubject, SubjectSearchHit,
};
