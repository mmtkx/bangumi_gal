use std::path::PathBuf;
use std::sync::Arc;

use library_core::paths;
use library_core::settings::{keys, SettingsStore};
use serde::Deserialize;

use crate::error::{ApiResult, AppError};

/// ADR-009 中的 `<AppName>`。
pub const APP_NAME: &str = "LAGM";

/// 播放器参数模板默认值：只传文件，不断点续播。
pub const DEFAULT_PLAYER_ARGS: &str = "\"{file}\"";

/// PotPlayer 的正确写法：`/new` 必须在文件前面，否则 `/seek` 会被忽略（实测）。
pub const POTPLAYER_ARGS: &str = "/new \"{file}\" /seek={seconds}";

/// 早期版本给出的错误顺序，读取配置时自动纠正，避免用户还要手动重选预设。
const LEGACY_POTPLAYER_ARGS: &str = "\"{file}\" /seek={seconds} /new";

/// 元数据缓存默认目录：`%LOCALAPPDATA%\LAGM\metadata`（ADR-009）。
pub fn default_metadata_dir() -> PathBuf {
    match std::env::var_os("LOCALAPPDATA") {
        Some(base) => PathBuf::from(base).join(APP_NAME).join("metadata"),
        // 非 Windows 或变量缺失时的兜底，保证路径始终可解析。
        None => std::env::temp_dir().join(APP_NAME).join("metadata"),
    }
}

/// 默认 OAuth 回调地址：固定本地端口，需与 Bangumi 应用后台填写值一致。
pub const DEFAULT_BANGUMI_REDIRECT_URI: &str = "http://127.0.0.1:27432/callback";

/// 设置页提交的输入。空字符串/null 表示「清除该配置」。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsInput {
    pub anime_root: Option<String>,
    pub game_root: Option<String>,
    pub metadata_cache_dir: Option<String>,
    pub player_path: Option<String>,
    pub player_args: Option<String>,
    pub plugins_dir: Option<String>,
}

/// Bangumi OAuth 凭据输入。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BangumiConfigInput {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub proxy: String,
}

#[derive(Debug, Clone)]
pub struct AppSettings {
    pub anime_root: Option<PathBuf>,
    pub game_root: Option<PathBuf>,
    pub metadata_cache_dir: PathBuf,
    pub metadata_cache_dir_is_default: bool,
    pub player_path: Option<PathBuf>,
    pub player_args: Option<String>,
    pub bangumi_client_id: Option<String>,
    pub bangumi_client_secret: Option<String>,
    pub bangumi_redirect_uri: Option<String>,
    pub bangumi_proxy: Option<String>,
    /// 自定义插件根目录；None 表示使用默认位置。
    pub plugins_dir: Option<PathBuf>,
}

/// 完整的 Bangumi OAuth 配置（三项齐全才可用）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BangumiConfig {
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    /// 显式代理；为空表示自动探测系统代理。
    pub proxy: Option<String>,
}

/// 设置服务：校验 + 持久化。
///
/// 校验统一放在这一层，扫描与启动路径都复用同一组规则，避免两处判断不一致。
#[derive(Clone)]
pub struct SettingsService {
    store: Arc<dyn SettingsStore>,
}

impl SettingsService {
    pub fn new(store: Arc<dyn SettingsStore>) -> Self {
        Self { store }
    }

    pub fn load(&self) -> ApiResult<AppSettings> {
        let anime_root = non_empty(self.store.get(keys::ANIME_ROOT)?).map(PathBuf::from);
        let game_root = non_empty(self.store.get(keys::GAME_ROOT)?).map(PathBuf::from);
        let stored_cache = non_empty(self.store.get(keys::METADATA_CACHE_DIR)?);

        let is_default = stored_cache.is_none();
        let metadata_cache_dir = stored_cache
            .map(PathBuf::from)
            .unwrap_or_else(default_metadata_dir);

        let player_path = non_empty(self.store.get(keys::PLAYER_PATH)?).map(PathBuf::from);
        let player_args = normalize_player_args(
            non_empty(self.store.get(keys::PLAYER_ARGS)?),
            player_path.as_deref(),
        );

        Ok(AppSettings {
            anime_root,
            game_root,
            metadata_cache_dir,
            metadata_cache_dir_is_default: is_default,
            player_path,
            player_args,
            bangumi_client_id: non_empty(self.store.get(keys::BANGUMI_CLIENT_ID)?),
            bangumi_client_secret: non_empty(self.store.get(keys::BANGUMI_CLIENT_SECRET)?),
            bangumi_redirect_uri: non_empty(self.store.get(keys::BANGUMI_REDIRECT_URI)?),
            bangumi_proxy: non_empty(self.store.get(keys::BANGUMI_PROXY)?),
            plugins_dir: non_empty(self.store.get(keys::PLUGINS_DIR)?).map(PathBuf::from),
        })
    }

    /// 当前构建是否内置了 Bangumi 应用。
    ///
    /// 内置后凭据由软件固定，界面上不出现 App ID / Secret / 回调地址，
    /// 后端也拒绝任何覆盖写入——用户只能授权或退出登录。
    pub fn builtin_bangumi_embedded(&self) -> bool {
        crate::builtin::bangumi_app().is_some()
    }

    /// 读取可用的 OAuth 配置。
    ///
    /// 内置应用存在时**始终使用内置凭据**（用户无法覆盖）；
    /// 只有未内置的构建才读取用户自己保存的三项配置。
    pub fn bangumi_config(&self) -> ApiResult<Option<BangumiConfig>> {
        let proxy = non_empty(self.store.get(keys::BANGUMI_PROXY)?);
        if let Some((client_id, client_secret)) = crate::builtin::bangumi_app() {
            return Ok(Some(BangumiConfig {
                client_id: client_id.to_owned(),
                client_secret: client_secret.to_owned(),
                redirect_uri: crate::builtin::REDIRECT_URI.to_owned(),
                proxy,
            }));
        }

        let client_id = non_empty(self.store.get(keys::BANGUMI_CLIENT_ID)?);
        let client_secret = non_empty(self.store.get(keys::BANGUMI_CLIENT_SECRET)?);
        let redirect_uri = non_empty(self.store.get(keys::BANGUMI_REDIRECT_URI)?);
        Ok(match (client_id, client_secret, redirect_uri) {
            (Some(client_id), Some(client_secret), Some(redirect_uri)) => Some(BangumiConfig {
                client_id,
                client_secret,
                redirect_uri,
                proxy,
            }),
            _ => None,
        })
    }

    /// 保存 Bangumi 凭据。三项全空表示清除；否则必须齐全且回调地址合法。
    pub fn save_bangumi(
        &self,
        client_id: &str,
        client_secret: &str,
        redirect_uri: &str,
        proxy: &str,
    ) -> ApiResult<()> {
        let client_id = client_id.trim();
        let client_secret = client_secret.trim();
        let redirect_uri = redirect_uri.trim();
        let proxy = proxy.trim();

        // 内置应用：凭据由软件固定，只允许改代理（网络环境因人而异）。
        if self.builtin_bangumi_embedded() {
            if !client_id.is_empty() || !client_secret.is_empty() || !redirect_uri.is_empty() {
                // 顺手清掉历史版本可能残留的用户凭据，避免留下误导性的记录。
                self.store.remove(keys::BANGUMI_CLIENT_ID)?;
                self.store.remove(keys::BANGUMI_CLIENT_SECRET)?;
                self.store.remove(keys::BANGUMI_REDIRECT_URI)?;
                return Err(AppError::Validation(
                    "本软件已内置 Bangumi 应用，App ID / Secret / 回调地址不可修改".to_owned(),
                ));
            }
            return self.save_bangumi_proxy(proxy);
        }

        if client_id.is_empty() && client_secret.is_empty() && redirect_uri.is_empty() {
            self.store.remove(keys::BANGUMI_CLIENT_ID)?;
            self.store.remove(keys::BANGUMI_CLIENT_SECRET)?;
            self.store.remove(keys::BANGUMI_REDIRECT_URI)?;
            self.store.remove(keys::BANGUMI_PROXY)?;
            return Ok(());
        }
        // 密码留空表示沿用已保存的 secret（方便只改 ID 或回调地址）。
        let effective_secret = if client_secret.is_empty() {
            non_empty(self.store.get(keys::BANGUMI_CLIENT_SECRET)?)
        } else {
            Some(client_secret.to_owned())
        };
        if client_id.is_empty() || effective_secret.is_none() {
            return Err(AppError::Validation(
                "Bangumi App ID 与 App Secret 不能为空".to_owned(),
            ));
        }
        let parsed = url::Url::parse(redirect_uri).map_err(|err| {
            AppError::Validation(format!("回调地址不合法：{redirect_uri}（{err}）"))
        })?;
        if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
            return Err(AppError::Validation(
                "回调地址必须是 http(s)://host:port/path 形式".to_owned(),
            ));
        }
        if parsed.port().is_none() {
            return Err(AppError::Validation(
                "回调地址必须显式指定端口（如 27432）".to_owned(),
            ));
        }
        validate_proxy(proxy)?;

        self.store.set(keys::BANGUMI_CLIENT_ID, client_id)?;
        self.store.set(
            keys::BANGUMI_CLIENT_SECRET,
            &effective_secret.expect("已校验"),
        )?;
        self.store.set(keys::BANGUMI_REDIRECT_URI, redirect_uri)?;
        self.save_bangumi_proxy(proxy)
    }

    /// 只保存代理设置（内置应用构建下唯一允许改动的 Bangumi 配置）。
    pub fn save_bangumi_proxy(&self, proxy: &str) -> ApiResult<()> {
        let proxy = proxy.trim();
        validate_proxy(proxy)?;
        match non_empty(Some(proxy.to_owned())) {
            Some(value) => self.store.set(keys::BANGUMI_PROXY, &value)?,
            None => self.store.remove(keys::BANGUMI_PROXY)?,
        }
        Ok(())
    }

    /// 内置应用构建下清理历史遗留的用户凭据。
    ///
    /// 旧版本允许用户填自己的 App；内置后这些值已无意义，
    /// 留着只会在将来「去掉内置」时突然生效，制造难以排查的行为差异。
    pub fn purge_saved_bangumi_credentials(&self) -> ApiResult<bool> {
        if !self.builtin_bangumi_embedded() {
            return Ok(false);
        }
        let has_any = non_empty(self.store.get(keys::BANGUMI_CLIENT_ID)?).is_some()
            || non_empty(self.store.get(keys::BANGUMI_CLIENT_SECRET)?).is_some()
            || non_empty(self.store.get(keys::BANGUMI_REDIRECT_URI)?).is_some();
        if !has_any {
            return Ok(false);
        }
        self.store.remove(keys::BANGUMI_CLIENT_ID)?;
        self.store.remove(keys::BANGUMI_CLIENT_SECRET)?;
        self.store.remove(keys::BANGUMI_REDIRECT_URI)?;
        Ok(true)
    }

    /// 保存设置。全部校验通过后才落库，避免出现「一半新一半旧」的配置。
    pub fn save(&self, input: &SettingsInput) -> ApiResult<AppSettings> {
        let anime_root = non_empty(input.anime_root.clone()).map(PathBuf::from);
        let game_root = non_empty(input.game_root.clone()).map(PathBuf::from);

        if let Some(path) = &anime_root {
            paths::ensure_directory(path)?;
        }
        if let Some(path) = &game_root {
            paths::ensure_directory(path)?;
        }
        if let (Some(anime), Some(game)) = (&anime_root, &game_root) {
            // plan.md 1.2：动画根目录与游戏根目录互不为父子关系。
            paths::ensure_disjoint_roots(anime, game)?;
        }

        let cache_dir = match non_empty(input.metadata_cache_dir.clone()) {
            Some(raw) => {
                let path = PathBuf::from(raw);
                if let Err(err) = std::fs::create_dir_all(&path) {
                    return Err(AppError::Core(library_core::CoreError::PathUnavailable {
                        path,
                        reason: err.to_string(),
                    }));
                }
                Some(path)
            }
            None => None,
        };

        let plugins_dir = match non_empty(input.plugins_dir.clone()) {
            Some(raw) => {
                let path = PathBuf::from(raw);
                if let Err(err) = std::fs::create_dir_all(&path) {
                    return Err(AppError::Core(library_core::CoreError::PathUnavailable {
                        path,
                        reason: err.to_string(),
                    }));
                }
                Some(path)
            }
            None => None,
        };

        write_optional(&*self.store, keys::ANIME_ROOT, anime_root.as_deref())?;
        write_optional(&*self.store, keys::GAME_ROOT, game_root.as_deref())?;
        write_optional(&*self.store, keys::METADATA_CACHE_DIR, cache_dir.as_deref())?;
        write_optional(&*self.store, keys::PLUGINS_DIR, plugins_dir.as_deref())?;

        let player_path = match non_empty(input.player_path.clone()) {
            Some(raw) => {
                let path = PathBuf::from(raw);
                let is_exe = path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| ext.eq_ignore_ascii_case("exe"))
                    .unwrap_or(false);
                if !is_exe || !path.is_file() {
                    return Err(AppError::Validation(format!(
                        "播放器必须是已存在的 .exe 文件：{}",
                        path.display()
                    )));
                }
                Some(path)
            }
            None => None,
        };
        write_optional(&*self.store, keys::PLAYER_PATH, player_path.as_deref())?;

        // 参数模板：trim 后为空表示「恢复默认」，此时不下发占位符替换，由上层用默认模板。
        let player_args = non_empty(input.player_args.clone());
        match &player_args {
            Some(args) => self.store.set(keys::PLAYER_ARGS, args)?,
            None => self.store.remove(keys::PLAYER_ARGS)?,
        }

        self.load()
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|raw| raw.trim().to_string())
        .filter(|raw| !raw.is_empty())
}

/// 校验出站代理：空串表示自动探测系统代理；非空必须是带 scheme 的地址。
fn validate_proxy(proxy: &str) -> ApiResult<()> {
    if proxy.is_empty() {
        return Ok(());
    }
    let parsed = url::Url::parse(proxy)
        .map_err(|err| AppError::Validation(format!("代理地址不合法：{proxy}（{err}）")))?;
    if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err(AppError::Validation(
            "代理地址需以 http://、https:// 或 socks5:// 开头".to_owned(),
        ));
    }
    Ok(())
}

/// 旧版 PotPlayer 模板顺序有误（`/new` 放在最后会让 `/seek` 失效），读取时自动纠正。
fn normalize_player_args(
    args: Option<String>,
    player_path: Option<&std::path::Path>,
) -> Option<String> {
    let value = args?;
    let is_potplayer = player_path
        .map(|path| path.to_string_lossy().to_lowercase().contains("potplayer"))
        .unwrap_or(false);
    if is_potplayer && value == LEGACY_POTPLAYER_ARGS {
        tracing::info!("检测到旧版 PotPlayer 参数模板，已修正 /new 的位置");
        return Some(POTPLAYER_ARGS.to_owned());
    }
    Some(value)
}

fn write_optional(
    store: &dyn SettingsStore,
    key: &str,
    value: Option<&std::path::Path>,
) -> ApiResult<()> {
    match value {
        Some(path) => store.set(key, &path.to_string_lossy())?,
        None => store.remove(key)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use library_core::db::Db;
    use library_core::settings::SqliteSettingsStore;
    use std::fs;

    fn service() -> (SettingsService, std::path::PathBuf) {
        let db = Db::open_in_memory().expect("db");
        db.migrate().expect("migrate");
        let store = Arc::new(SqliteSettingsStore::new(Arc::new(db)));
        let base = std::env::temp_dir().join(format!("lagm-settings-{}", uuid_like()));
        fs::create_dir_all(&base).expect("base");
        (SettingsService::new(store), base)
    }

    fn uuid_like() -> String {
        format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default()
        )
    }

    #[test]
    fn defaults_to_adr009_cache_dir() {
        let (service, base) = service();
        let settings = service.load().expect("load");
        assert!(settings.metadata_cache_dir_is_default);
        assert!(
            settings.metadata_cache_dir.ends_with("LAGM/metadata")
                || settings.metadata_cache_dir.ends_with("LAGM\\metadata")
        );
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn rejects_nested_roots() {
        let (service, base) = service();
        let game = base.join("game");
        fs::create_dir_all(base.join("anime").join("inner")).expect("dirs");
        fs::create_dir_all(&game).expect("game");

        let err = service
            .save(&SettingsInput {
                anime_root: Some(base.to_string_lossy().into()),
                game_root: Some(game.to_string_lossy().into()),
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect_err("父子目录应当被拒绝");
        assert_eq!(err.code(), "roots_overlap");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn saves_roots_and_clears_with_empty_string() {
        let (service, base) = service();
        let anime = base.join("anime");
        let game = base.join("game");
        fs::create_dir_all(&anime).expect("anime");
        fs::create_dir_all(&game).expect("game");

        let saved = service
            .save(&SettingsInput {
                anime_root: Some(anime.to_string_lossy().into()),
                game_root: Some(game.to_string_lossy().into()),
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect("save");
        assert_eq!(saved.anime_root.as_deref(), Some(anime.as_path()));
        assert_eq!(saved.game_root.as_deref(), Some(game.as_path()));

        let cleared = service
            .save(&SettingsInput {
                anime_root: Some("   ".into()),
                game_root: None,
                metadata_cache_dir: Some("".into()),
                player_path: None,
                ..Default::default()
            })
            .expect("clear");
        assert!(cleared.anime_root.is_none());
        assert!(cleared.game_root.is_none());

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn missing_directory_is_rejected() {
        let (service, base) = service();
        let missing = base.join("not-exists");
        let err = service
            .save(&SettingsInput {
                anime_root: Some(missing.to_string_lossy().into()),
                game_root: None,
                metadata_cache_dir: None,
                player_path: None,
                ..Default::default()
            })
            .expect_err("不存在的目录应当被拒绝");
        assert_eq!(err.code(), "fs_error");
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn legacy_potplayer_args_are_corrected() {
        let legacy = Some(LEGACY_POTPLAYER_ARGS.to_owned());
        assert_eq!(
            normalize_player_args(
                legacy.clone(),
                Some(std::path::Path::new("D:/PotPlayer/PotPlayerMini64.exe"))
            ),
            Some(POTPLAYER_ARGS.to_owned())
        );
        // 非 PotPlayer 不动用户配置。
        assert_eq!(
            normalize_player_args(legacy, Some(std::path::Path::new("D:/mpv/mpv.exe"))),
            Some(LEGACY_POTPLAYER_ARGS.to_owned())
        );
    }

    #[test]
    fn player_path_must_be_existing_exe() {
        let (service, base) = service();

        let err = service
            .save(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: None,
                player_path: Some("D:/nope/player.exe".into()),
                ..Default::default()
            })
            .expect_err("不存在的播放器应当被拒绝");
        assert_eq!(err.code(), "validation_error");

        let not_exe = base.join("player.txt");
        fs::write(&not_exe, b"x").expect("txt");
        let err = service
            .save(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: None,
                player_path: Some(not_exe.to_string_lossy().into()),
                ..Default::default()
            })
            .expect_err("非 exe 应当被拒绝");
        assert_eq!(err.code(), "validation_error");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn player_path_is_optional_and_clears() {
        let (service, base) = service();
        let saved = service
            .save(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: None,
                player_path: Some("C:\\Windows\\System32\\where.exe".into()),
                ..Default::default()
            })
            .expect("save player");
        assert!(saved.player_path.is_some());

        let cleared = service
            .save(&SettingsInput {
                anime_root: None,
                game_root: None,
                metadata_cache_dir: None,
                player_path: Some("".into()),
                ..Default::default()
            })
            .expect("clear player");
        assert!(cleared.player_path.is_none());
        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn plugins_dir_is_saved_and_cleared() {
        let (service, base) = service();
        let dir = base.join("custom-plugins");
        service
            .save(&SettingsInput {
                plugins_dir: Some(dir.to_string_lossy().into()),
                ..Default::default()
            })
            .expect("save");
        let loaded = service.load().expect("load");
        assert_eq!(loaded.plugins_dir.as_deref(), Some(dir.as_path()));
        assert!(dir.is_dir());

        service
            .save(&SettingsInput {
                plugins_dir: Some("   ".into()),
                ..Default::default()
            })
            .expect("clear");
        assert!(service.load().expect("load").plugins_dir.is_none());

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn bangumi_config_prefers_builtin_and_rejects_override() {
        let (service, base) = service();

        if let Some((builtin_id, builtin_secret)) = crate::builtin::bangumi_app() {
            // 内置应用：零配置可用，且用户改不了。
            let config = service.bangumi_config().expect("config").expect("内置可用");
            assert_eq!(config.client_id, builtin_id);
            assert_eq!(config.client_secret, builtin_secret);
            assert_eq!(config.redirect_uri, crate::builtin::REDIRECT_URI);

            let err = service
                .save_bangumi("bgm123", "secret", DEFAULT_BANGUMI_REDIRECT_URI, "")
                .expect_err("内置应用不允许覆盖凭据");
            assert_eq!(err.code(), "validation_error");
            // 拒绝后配置未被写坏。
            assert_eq!(
                service
                    .bangumi_config()
                    .expect("config")
                    .expect("some")
                    .client_id,
                builtin_id
            );

            // 只改代理是允许的（网络环境因人而异）。
            service
                .save_bangumi("", "", "", "http://127.0.0.1:7890")
                .expect("仅改代理");
            let config = service.bangumi_config().expect("config").expect("some");
            assert_eq!(config.proxy.as_deref(), Some("http://127.0.0.1:7890"));
            assert_eq!(config.client_id, builtin_id);
        } else {
            // 未内置的构建：退回用户自填，并支持清除。
            assert!(service.bangumi_config().expect("config").is_none());
            service
                .save_bangumi("bgm123", "secret", DEFAULT_BANGUMI_REDIRECT_URI, "")
                .expect("save");
            let config = service.bangumi_config().expect("config").expect("some");
            assert_eq!(config.client_id, "bgm123");
            assert_eq!(config.client_secret, "secret");
            assert!(config.proxy.is_none());
            service.save_bangumi("", "", "", "").expect("clear");
            assert!(service.bangumi_config().expect("config").is_none());
        }

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn bangumi_saves_proxy_override() {
        let (service, base) = service();
        if service.builtin_bangumi_embedded() {
            service
                .save_bangumi("", "", "", "http://127.0.0.1:7890")
                .expect("save");
        } else {
            service
                .save_bangumi(
                    "bgm",
                    "s",
                    DEFAULT_BANGUMI_REDIRECT_URI,
                    "http://127.0.0.1:7890",
                )
                .expect("save");
        }
        assert_eq!(
            service
                .bangumi_config()
                .expect("config")
                .expect("some")
                .proxy
                .as_deref(),
            Some("http://127.0.0.1:7890")
        );

        let err = service
            .save_bangumi("", "", "", "not-a-url")
            .expect_err("代理地址非法应拒绝");
        assert_eq!(err.code(), "validation_error");

        fs::remove_dir_all(base).ok();
    }

    #[test]
    fn bangumi_rejects_invalid_redirect_and_missing_parts() {
        let (service, base) = service();

        let err = service
            .save_bangumi("bgm", "secret", "ftp://127.0.0.1:1/cb", "")
            .expect_err("非 http 应拒绝");
        assert_eq!(err.code(), "validation_error");

        let err = service
            .save_bangumi("bgm", "secret", "http://localhost/cb", "")
            .expect_err("缺少端口应拒绝");
        assert_eq!(err.code(), "validation_error");

        let err = service
            .save_bangumi("bgm", "", DEFAULT_BANGUMI_REDIRECT_URI, "")
            .expect_err("缺 secret 应拒绝");
        assert_eq!(err.code(), "validation_error");

        fs::remove_dir_all(base).ok();
    }
}
