//! 插件 manifest 解析与校验（plan.md 第 4 节）。
//!
//! 只做纯文件系统与数据校验，不接触数据库、不加载插件代码；
//! 注册表持久化由 app-services 组合 `PluginRegistryRepository` 完成。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// 当前支持的插件 API 版本。
pub const PLUGIN_API_VERSION: &str = "1";
/// 插件清单文件名。
pub const MANIFEST_FILE: &str = "manifest.json";
/// UI 插件可用的挂载点。
pub const MOUNT_POINTS: [&str; 2] = ["library.item.detail.panel", "settings.section"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PluginKind {
    Ui,
    Capability,
}

impl PluginKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ui => "ui",
            Self::Capability => "capability",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "ui" => Some(Self::Ui),
            "capability" => Some(Self::Capability),
            _ => None,
        }
    }
}

/// 权限白名单。未在此列出的权限一律视为清单非法。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    ReadLibrary,
    ReadFilesystem,
    WriteLibrary,
    EmitEvents,
}

impl Permission {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ReadLibrary => "read:library",
            Self::ReadFilesystem => "read:filesystem",
            Self::WriteLibrary => "write:library",
            Self::EmitEvents => "emit:events",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "read:library" => Some(Self::ReadLibrary),
            "read:filesystem" => Some(Self::ReadFilesystem),
            "write:library" => Some(Self::WriteLibrary),
            "emit:events" => Some(Self::EmitEvents),
            _ => None,
        }
    }
}

/// 清单原文。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginManifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: String,
    pub api_version: String,
    pub entry: String,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// UI 插件的挂载点（capability 插件不声明）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mount: Option<String>,
}

/// 通过校验的插件。
#[derive(Debug, Clone)]
pub struct ValidatedPlugin {
    pub manifest: PluginManifest,
    pub kind: PluginKind,
    pub permissions: Vec<Permission>,
    pub dir: PathBuf,
    pub manifest_path: PathBuf,
    pub entry_path: PathBuf,
}

impl ValidatedPlugin {
    pub fn has(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("插件目录缺少 manifest.json：{0}")]
    MissingManifest(String),
    #[error("读取插件清单失败：{0}")]
    Io(#[from] std::io::Error),
    #[error("插件清单不是合法 JSON：{0}")]
    Json(#[from] serde_json::Error),
    #[error("插件清单校验失败：{0}")]
    Invalid(String),
    /// 沙箱执行期的失败（模块加载、导入缺失、宿主 API 拒绝等）。
    #[error("插件执行失败：{0}")]
    Runtime(String),
    /// 指令预算耗尽：插件被强制中止，进程不受影响。
    #[error("插件执行超出指令预算（{0} 步），已强制中止")]
    Timeout(u64),
}

fn invalid(message: impl Into<String>) -> PluginError {
    PluginError::Invalid(message.into())
}

/// 读取并校验插件目录下的 manifest.json。
pub fn load_manifest(plugin_dir: &Path) -> Result<ValidatedPlugin, PluginError> {
    let manifest_path = plugin_dir.join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(PluginError::MissingManifest(
            plugin_dir.display().to_string(),
        ));
    }
    let raw = std::fs::read_to_string(&manifest_path)?;
    let manifest: PluginManifest = serde_json::from_str(&raw)?;

    validate_id(&manifest.id)?;
    if manifest.name.trim().is_empty() {
        return Err(invalid("name 不能为空"));
    }
    if manifest.name.chars().count() > 100 {
        return Err(invalid("name 不能超过 100 字符"));
    }
    if manifest.version.trim().is_empty() {
        return Err(invalid("version 不能为空"));
    }
    let kind = PluginKind::parse(&manifest.kind)
        .ok_or_else(|| invalid(format!("未知插件类型：{}", manifest.kind)))?;
    if manifest.api_version.trim() != PLUGIN_API_VERSION {
        return Err(invalid(format!(
            "不支持的 apiVersion：{}（当前仅支持 {PLUGIN_API_VERSION}）",
            manifest.api_version
        )));
    }

    match kind {
        PluginKind::Ui => {
            let mount = manifest.mount.as_deref().map(str::trim).unwrap_or_default();
            if mount.is_empty() {
                return Err(invalid("ui 插件必须在 manifest 中声明 mount"));
            }
            if !MOUNT_POINTS.contains(&mount) {
                return Err(invalid(format!("未知挂载点：{mount}")));
            }
        }
        PluginKind::Capability => {
            let has_mount = manifest
                .mount
                .as_deref()
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);
            if has_mount {
                return Err(invalid("capability 插件不应声明 mount"));
            }
        }
    }

    let entry_path = resolve_entry(plugin_dir, &manifest.entry, kind)?;
    let permissions = parse_permissions(&manifest.permissions)?;
    for capability in &manifest.capabilities {
        validate_capability(capability)?;
    }

    let dir = plugin_dir
        .canonicalize()
        .map_err(|_| invalid(format!("插件目录不可访问：{}", plugin_dir.display())))?;

    Ok(ValidatedPlugin {
        manifest,
        kind,
        permissions,
        dir,
        manifest_path,
        entry_path,
    })
}

fn validate_id(id: &str) -> Result<(), PluginError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(invalid("id 不能为空"));
    }
    if id.len() > 128 {
        return Err(invalid("id 不能超过 128 字符"));
    }
    let valid = id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'));
    if !valid {
        return Err(invalid("id 只能包含字母、数字、点、下划线与短横线"));
    }
    Ok(())
}

fn validate_capability(capability: &str) -> Result<(), PluginError> {
    let value = capability.trim();
    if value.is_empty() {
        return Err(invalid("capabilities 不能包含空字符串"));
    }
    let mut chars = value.chars();
    let first = chars.next().expect("非空");
    if !first.is_ascii_alphabetic() {
        return Err(invalid(format!("capability 需以字母开头：{value}")));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
        return Err(invalid(format!("capability 含非法字符：{value}")));
    }
    Ok(())
}

fn parse_permissions(raw: &[String]) -> Result<Vec<Permission>, PluginError> {
    let mut out: Vec<Permission> = Vec::new();
    for item in raw {
        let permission = Permission::parse(item)
            .ok_or_else(|| invalid(format!("未声明的权限：{}", item.trim())))?;
        if !out.contains(&permission) {
            out.push(permission);
        }
    }
    Ok(out)
}

/// 解析 entry：必须是插件目录内的相对路径，且扩展名与类型匹配。
fn resolve_entry(plugin_dir: &Path, entry: &str, kind: PluginKind) -> Result<PathBuf, PluginError> {
    let raw = entry.trim();
    if raw.is_empty() {
        return Err(invalid("entry 不能为空"));
    }
    let candidate = PathBuf::from(raw);
    if candidate.is_absolute() {
        return Err(invalid("entry 必须是相对路径"));
    }
    if raw.split(['/', '\\']).any(|segment| segment == "..") {
        return Err(invalid("entry 不能包含 .."));
    }

    let dir = plugin_dir
        .canonicalize()
        .map_err(|_| invalid(format!("插件目录不可访问：{}", plugin_dir.display())))?;
    let full = dir.join(&candidate);
    let canonical = full
        .canonicalize()
        .map_err(|_| invalid(format!("entry 指向的文件不存在：{raw}")))?;
    if !canonical.starts_with(&dir) {
        return Err(invalid("entry 越出插件目录"));
    }
    if !canonical.is_file() {
        return Err(invalid("entry 不是文件"));
    }

    let extension = canonical
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    let expected: &[&str] = match kind {
        PluginKind::Ui => &["js", "mjs"],
        PluginKind::Capability => &["wasm"],
    };
    if !expected.contains(&extension.as_str()) {
        return Err(invalid(format!(
            "{} 插件的入口扩展名必须是 {}，实际为 {extension}",
            kind.as_str(),
            expected.join("/")
        )));
    }
    Ok(canonical)
}

/// 扫描插件根目录，返回每个含 manifest.json 的子目录及校验结果。
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub dir: PathBuf,
    pub manifest: Option<PluginManifest>,
    pub kind: Option<PluginKind>,
    pub error: Option<String>,
}

pub fn discover(root: &Path) -> Vec<DiscoveredPlugin> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() || !dir.join(MANIFEST_FILE).is_file() {
            continue;
        }
        match load_manifest(&dir) {
            Ok(validated) => out.push(DiscoveredPlugin {
                dir,
                manifest: Some(validated.manifest),
                kind: Some(validated.kind),
                error: None,
            }),
            Err(err) => {
                // 校验失败时仍尽量读出原始清单，便于界面展示与排错。
                let manifest = std::fs::read_to_string(dir.join(MANIFEST_FILE))
                    .ok()
                    .and_then(|raw| serde_json::from_str::<PluginManifest>(&raw).ok());
                let kind = manifest
                    .as_ref()
                    .and_then(|item| PluginKind::parse(&item.kind));
                out.push(DiscoveredPlugin {
                    dir,
                    manifest,
                    kind,
                    error: Some(err.to_string()),
                });
            }
        }
    }
    out.sort_by(|left, right| left.dir.cmp(&right.dir));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lagm-plugin-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn write_plugin(dir: &Path, manifest: &str, entry: &str) {
        std::fs::write(dir.join(MANIFEST_FILE), manifest).expect("manifest");
        let entry_path = dir.join(entry);
        if let Some(parent) = entry_path.parent() {
            std::fs::create_dir_all(parent).expect("entry parent");
        }
        std::fs::write(entry_path, b"// stub").expect("entry");
    }

    fn valid_manifest() -> String {
        serde_json::json!({
            "id": "com.example.demo",
            "name": "示例插件",
            "version": "0.1.0",
            "kind": "capability",
            "apiVersion": "1",
            "entry": "main.wasm",
            "capabilities": ["scanner.anime"],
            "permissions": ["read:library", "read:filesystem"]
        })
        .to_string()
    }

    #[test]
    fn accepts_valid_manifest() {
        let dir = temp_dir("ok");
        write_plugin(&dir, &valid_manifest(), "main.wasm");
        let plugin = load_manifest(&dir).expect("valid");
        assert_eq!(plugin.kind, PluginKind::Capability);
        assert!(plugin.has(Permission::ReadLibrary));
        assert!(plugin.has(Permission::ReadFilesystem));
        assert_eq!(plugin.permissions.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_unknown_api_version() {
        let dir = temp_dir("api");
        let manifest = valid_manifest().replace("\"apiVersion\":\"1\"", "\"apiVersion\":\"2\"");
        write_plugin(&dir, &manifest, "main.wasm");
        let err = load_manifest(&dir).expect_err("应拒绝");
        assert!(err.to_string().contains("apiVersion"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_unknown_permission() {
        let dir = temp_dir("perm");
        let manifest = valid_manifest().replace("read:library", "admin:everything");
        write_plugin(&dir, &manifest, "main.wasm");
        let err = load_manifest(&dir).expect_err("应拒绝");
        assert!(err.to_string().contains("未声明的权限"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_entry_escape() {
        let dir = temp_dir("escape");
        let manifest = valid_manifest().replace("main.wasm", "../evil.wasm");
        std::fs::write(dir.join(MANIFEST_FILE), &manifest).expect("manifest");
        let err = load_manifest(&dir).expect_err("应拒绝");
        assert!(err.to_string().contains(".."));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_wrong_entry_extension() {
        let dir = temp_dir("ext");
        write_plugin(&dir, &valid_manifest(), "main.js");
        let err = load_manifest(&dir).expect_err("应拒绝");
        assert!(err.to_string().contains("wasm"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_missing_manifest() {
        let dir = temp_dir("missing");
        let err = load_manifest(&dir).expect_err("应拒绝");
        assert!(matches!(err, PluginError::MissingManifest(_)));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn discover_reports_invalid_plugins() {
        let root = temp_dir("discover");
        let good = root.join("good");
        let bad = root.join("bad");
        std::fs::create_dir_all(&good).expect("good");
        std::fs::create_dir_all(&bad).expect("bad");
        write_plugin(&good, &valid_manifest(), "main.wasm");
        let broken = valid_manifest().replace("\"apiVersion\":\"1\"", "\"apiVersion\":\"9\"");
        std::fs::write(bad.join(MANIFEST_FILE), broken).expect("bad manifest");

        let found = discover(&root);
        assert_eq!(found.len(), 2);
        let good_entry = found
            .iter()
            .find(|item| item.dir.ends_with("good"))
            .expect("good");
        assert!(good_entry.error.is_none());
        let bad_entry = found
            .iter()
            .find(|item| item.dir.ends_with("bad"))
            .expect("bad");
        assert!(bad_entry.error.is_some());
        assert!(bad_entry.manifest.is_some());

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn ui_requires_known_mount() {
        let missing = temp_dir("mount-missing");
        let manifest = serde_json::json!({
            "id": "com.example.ui",
            "name": "UI",
            "version": "0.1.0",
            "kind": "ui",
            "apiVersion": "1",
            "entry": "main.js"
        })
        .to_string();
        write_plugin(&missing, &manifest, "main.js");
        let err = load_manifest(&missing).expect_err("缺 mount 应拒绝");
        assert!(err.to_string().contains("mount"));
        std::fs::remove_dir_all(&missing).ok();

        let unknown = temp_dir("mount-unknown");
        let manifest = serde_json::json!({
            "id": "com.example.ui",
            "name": "UI",
            "version": "0.1.0",
            "kind": "ui",
            "apiVersion": "1",
            "entry": "main.js",
            "mount": "somewhere.else"
        })
        .to_string();
        write_plugin(&unknown, &manifest, "main.js");
        let err = load_manifest(&unknown).expect_err("未知挂载点应拒绝");
        assert!(err.to_string().contains("未知挂载点"));
        std::fs::remove_dir_all(&unknown).ok();

        let ok = temp_dir("mount-ok");
        let manifest = serde_json::json!({
            "id": "com.example.ui",
            "name": "UI",
            "version": "0.1.0",
            "kind": "ui",
            "apiVersion": "1",
            "entry": "main.js",
            "mount": "library.item.detail.panel"
        })
        .to_string();
        write_plugin(&ok, &manifest, "main.js");
        let plugin = load_manifest(&ok).expect("valid");
        assert_eq!(
            plugin.manifest.mount.as_deref(),
            Some("library.item.detail.panel")
        );
        std::fs::remove_dir_all(&ok).ok();
    }

    #[test]
    fn capability_rejects_mount() {
        let dir = temp_dir("cap-mount");
        let manifest = valid_manifest().replace(
            "\"permissions\"",
            "\"mount\":\"settings.section\",\"permissions\"",
        );
        write_plugin(&dir, &manifest, "main.wasm");
        let err = load_manifest(&dir).expect_err("capability 不应带 mount");
        assert!(err.to_string().contains("mount"));
        std::fs::remove_dir_all(&dir).ok();
    }
}
