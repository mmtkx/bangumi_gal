//! 插件宿主：manifest 校验、插件目录发现、WASM 沙箱执行（M4 阶段 1 / 2）。
//!
//! 本 crate 只做纯文件系统、数据校验与沙箱执行，不接触数据库、不发布事件；
//! 注册表持久化与宿主能力回调由 app-services 组合 library-core 实现。

pub mod manifest;
pub mod wasm;

use std::path::{Path, PathBuf};

pub use manifest::{
    discover, load_manifest, DiscoveredPlugin, Permission, PluginError, PluginKind, PluginManifest,
    ValidatedPlugin, MANIFEST_FILE, PLUGIN_API_VERSION,
};
pub use wasm::{
    CapabilityRuntime, HostBridge, InvocationReport, ALLOC_EXPORT, ENTRY_EXPORT, HOST_MODULE,
    MAX_PAYLOAD_BYTES, MEMORY_EXPORT,
};

/// 插件根目录与发现入口。
#[derive(Debug, Clone)]
pub struct PluginHost {
    root: PathBuf,
}

impl PluginHost {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// 确保根目录存在（首次启动时创建）。
    pub fn ensure_root(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }

    /// 扫描根目录下的插件。
    pub fn discover(&self) -> Vec<DiscoveredPlugin> {
        discover(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_root_creates_directory() {
        let root = std::env::temp_dir().join(format!("lagm-host-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let host = PluginHost::new(&root);
        host.ensure_root().expect("create");
        assert!(host.root().is_dir());
        assert!(host.discover().is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}
