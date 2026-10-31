//! 能力插件（WASM）沙箱运行时（M4 阶段 2）。
//!
//! 隔离手段：wasmtime 单实例 + 指令预算（fuel）+ 内存上限 + 禁止线程；
//! 插件只能通过 `lagm` 模块的四个导入与宿主交互，权限在导入实现内部校验，
//! 未声明权限的调用直接 trap，不会穿透到宿主。
//!
//! 宿主与插件的数据交换统一走线性内存中的 UTF-8 字符串：
//! - 输入：宿主调用 guest 的 `alloc(len)` 拿指针 → 写入 JSON → 调 `invoke(ptr, len)`。
//! - 输出：guest 返回 `(ptr << 32) | len`，宿主按该区间读出 JSON。
//! - 宿主回传（`read_items`）：宿主反向调用 guest 的 `alloc`，写入后返回同样的打包值。

use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use wasmtime::{
    format_err, Caller, Config, Engine, Extern, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder,
};

use crate::manifest::{Permission, PluginError, PluginKind, ValidatedPlugin};

/// 宿主导入所在的模块名。
pub const HOST_MODULE: &str = "lagm";
/// 插件必须导出的入口函数：`(ptr: i32, len: i32) -> i64`。
pub const ENTRY_EXPORT: &str = "invoke";
/// 插件必须导出的内存分配函数：`(len: i32) -> i32`。
pub const ALLOC_EXPORT: &str = "alloc";
/// 插件必须导出的线性内存。
pub const MEMORY_EXPORT: &str = "memory";

/// 单次调用允许的输入 / 输出 JSON 上限。
pub const MAX_PAYLOAD_BYTES: usize = 1024 * 1024;
/// 每次调用累计保留的日志上限（超出部分丢弃，避免插件刷爆内存）。
pub const MAX_LOG_BYTES: usize = 8 * 1024;
/// 默认指令预算：约数十毫秒到数百毫秒量级的纯计算，足够解析文件名。
pub const DEFAULT_FUEL: u64 = 50_000_000;
/// 默认线性内存上限。
pub const DEFAULT_MEMORY_BYTES: usize = 64 * 1024 * 1024;

/// 宿主为插件提供的能力回调，由 app-services 实现（插件宿主不依赖具体功能实现）。
pub trait HostBridge: Send + Sync {
    /// 读取条目库快照，返回 JSON 数组文本。
    fn read_items(&self) -> Result<String, String>;
    /// 向主干道发布插件事件，`payload` 为 JSON 文本。
    fn emit(&self, name: &str, payload: &str) -> Result<(), String>;
}

/// 一次调用的结果。
#[derive(Debug, Clone)]
pub struct InvocationReport {
    /// 插件返回的 JSON 文本（已校验可解析）。
    pub output: String,
    pub logs: Vec<String>,
    pub fuel_used: u64,
    pub duration_ms: u64,
}

/// 单实例宿主状态。
struct HostState {
    permissions: Vec<Permission>,
    limits: StoreLimits,
    logs: Vec<String>,
    log_bytes: usize,
    bridge: Arc<dyn HostBridge>,
}

/// 能力插件运行时：按需编译并实例化插件模块。
///
/// 不缓存编译结果：插件可能在运行期被替换，重编译换来「改文件即生效」，
/// 单次编译在数十毫秒量级，代价可接受。
pub struct CapabilityRuntime {
    engine: Engine,
    fuel: u64,
    memory_bytes: usize,
}

impl CapabilityRuntime {
    pub fn new() -> Result<Self, PluginError> {
        Self::with_limits(DEFAULT_FUEL, DEFAULT_MEMORY_BYTES)
    }

    /// 允许测试注入更小的预算，便于验证超时与内存上限。
    pub fn with_limits(fuel: u64, memory_bytes: usize) -> Result<Self, PluginError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        // 未启用 wasm 线程特性：插件只能单线程执行，无法通过共享内存逃逸到其它实例。
        let engine = Engine::new(&config)
            .map_err(|err| PluginError::Runtime(format!("初始化 WASM 引擎失败：{err}")))?;
        Ok(Self {
            engine,
            fuel,
            memory_bytes,
        })
    }

    /// 执行一次能力插件调用。`input` 必须是合法 JSON 文本。
    pub fn invoke(
        &self,
        plugin: &ValidatedPlugin,
        input: &str,
        bridge: Arc<dyn HostBridge>,
    ) -> Result<InvocationReport, PluginError> {
        if plugin.kind != PluginKind::Capability {
            return Err(PluginError::Runtime(format!(
                "插件 {} 是 {} 类型，不能作为能力插件执行",
                plugin.manifest.id,
                plugin.kind.as_str()
            )));
        }
        if input.len() > MAX_PAYLOAD_BYTES {
            return Err(PluginError::Runtime(format!(
                "输入超过 {MAX_PAYLOAD_BYTES} 字节，已拒绝执行"
            )));
        }
        // 输入必须是 JSON：避免插件收到半截文本后再报出难以定位的解析错误。
        serde_json::from_str::<serde_json::Value>(input)
            .map_err(|err| PluginError::Runtime(format!("输入不是合法 JSON：{err}")))?;

        let bytes = std::fs::read(&plugin.entry_path)?;
        // 用 Module::new 而非 from_binary：它同时接受 wasm 二进制与 wat 文本，
        // 插件作者可以直接把 .wat 改名成入口文件做调试，省掉工具链。
        let module = Module::new(&self.engine, &bytes).map_err(|err| {
            PluginError::Runtime(format!("WASM 模块加载失败：{}", describe(&err)))
        })?;

        let limits = StoreLimitsBuilder::new()
            .memory_size(self.memory_bytes)
            .memories(1)
            .tables(4)
            .table_elements(4096)
            .instances(1)
            .build();
        let state = HostState {
            permissions: plugin.permissions.clone(),
            limits,
            logs: Vec::new(),
            log_bytes: 0,
            bridge,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|state| &mut state.limits);
        store
            .set_fuel(self.fuel)
            .map_err(|err| PluginError::Runtime(format!("设置指令预算失败：{err}")))?;

        let linker = self.linker()?;
        let started = Instant::now();
        let instance = linker
            .instantiate(&mut store, &module)
            .map_err(|err| classify(&store, err, self.fuel))?;

        let memory = instance
            .get_memory(&mut store, MEMORY_EXPORT)
            .ok_or_else(|| PluginError::Runtime(format!("插件未导出线性内存 `{MEMORY_EXPORT}`")))?;
        let alloc = instance
            .get_typed_func::<i32, i32>(&mut store, ALLOC_EXPORT)
            .map_err(|_| {
                PluginError::Runtime(format!("插件未导出 `{ALLOC_EXPORT}(len: i32) -> i32`"))
            })?;
        let entry = instance
            .get_typed_func::<(i32, i32), i64>(&mut store, ENTRY_EXPORT)
            .map_err(|_| {
                PluginError::Runtime(format!(
                    "插件未导出 `{ENTRY_EXPORT}(ptr: i32, len: i32) -> i64`"
                ))
            })?;

        let input_len = input.len() as i32;
        let ptr = alloc
            .call(&mut store, input_len)
            .map_err(|err| classify(&store, err, self.fuel))?;
        if ptr < 0 {
            return Err(PluginError::Runtime(format!(
                "插件分配的输入缓冲区地址非法：{ptr}"
            )));
        }
        memory
            .write(&mut store, ptr as usize, input.as_bytes())
            .map_err(|_| {
                PluginError::Runtime("写入插件内存失败（输入超出插件内存上限）".to_owned())
            })?;

        let packed = entry
            .call(&mut store, (ptr, input_len))
            .map_err(|err| classify(&store, err, self.fuel))?;
        let (out_ptr, out_len) = unpack(packed);
        if out_len > MAX_PAYLOAD_BYTES {
            return Err(PluginError::Runtime(format!(
                "插件输出 {out_len} 字节，超过 {MAX_PAYLOAD_BYTES} 字节限制"
            )));
        }
        let mut buffer = vec![0u8; out_len];
        memory.read(&store, out_ptr, &mut buffer).map_err(|_| {
            PluginError::Runtime("读取插件输出失败（返回的指针或长度越界）".to_owned())
        })?;
        let output = String::from_utf8(buffer)
            .map_err(|_| PluginError::Runtime("插件输出不是合法 UTF-8".to_owned()))?;
        serde_json::from_str::<serde_json::Value>(&output)
            .map_err(|err| PluginError::Runtime(format!("插件输出不是合法 JSON：{err}")))?;

        let remaining = store.get_fuel().unwrap_or(0);
        Ok(InvocationReport {
            output,
            logs: store.data().logs.clone(),
            fuel_used: self.fuel.saturating_sub(remaining),
            duration_ms: started.elapsed().as_millis() as u64,
        })
    }

    fn linker(&self) -> Result<Linker<HostState>, PluginError> {
        let mut linker = Linker::new(&self.engine);

        linker
            .func_wrap(
                HOST_MODULE,
                "log",
                |mut caller: Caller<'_, HostState>, level: i32, ptr: i32, len: i32| {
                    let text = read_guest_string(&mut caller, ptr, len)?;
                    let state = caller.data_mut();
                    if state.log_bytes + text.len() > MAX_LOG_BYTES {
                        return Ok(());
                    }
                    state.log_bytes += text.len();
                    let tag = match level {
                        0 => "debug",
                        1 => "info",
                        2 => "warn",
                        _ => "error",
                    };
                    state.logs.push(format!("[{tag}] {text}"));
                    Ok(())
                },
            )
            .map_err(runtime_error)?;

        linker
            .func_wrap(HOST_MODULE, "now_ms", || -> i64 {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|value| value.as_millis() as i64)
                    .unwrap_or(0)
            })
            .map_err(runtime_error)?;

        linker
            .func_wrap(
                HOST_MODULE,
                "read_items",
                |mut caller: Caller<'_, HostState>| -> Result<i64, wasmtime::Error> {
                    if !caller.data().permissions.contains(&Permission::ReadLibrary) {
                        return Err(format_err!(
                            "插件未声明 read:library 权限，已拒绝读取条目库"
                        ));
                    }
                    let payload = caller
                        .data()
                        .bridge
                        .read_items()
                        .map_err(|err| format_err!("读取条目库失败：{err}"))?;
                    write_guest_string(&mut caller, &payload)
                },
            )
            .map_err(runtime_error)?;

        linker
            .func_wrap(
                HOST_MODULE,
                "emit",
                |mut caller: Caller<'_, HostState>,
                 ptr: i32,
                 len: i32|
                 -> Result<(), wasmtime::Error> {
                    if !caller.data().permissions.contains(&Permission::EmitEvents) {
                        return Err(format_err!("插件未声明 emit:events 权限，已拒绝发送事件"));
                    }
                    let text = read_guest_string(&mut caller, ptr, len)?;
                    let value: serde_json::Value = serde_json::from_str(&text)
                        .map_err(|err| format_err!("事件参数不是合法 JSON：{err}"))?;
                    let name = value
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .trim()
                        .to_owned();
                    if name.is_empty() {
                        return Err(format_err!("事件参数缺少 name 字段"));
                    }
                    let payload = value
                        .get("payload")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null)
                        .to_string();
                    caller
                        .data()
                        .bridge
                        .emit(&name, &payload)
                        .map_err(|err| format_err!("发送事件失败：{err}"))?;
                    Ok(())
                },
            )
            .map_err(runtime_error)?;

        Ok(linker)
    }
}

fn runtime_error(err: wasmtime::Error) -> PluginError {
    PluginError::Runtime(format!("注册宿主 API 失败：{}", describe(&err)))
}

/// 指令预算耗尽时 wasmtime 报的是普通 trap，靠余额归零判定为超时。
fn classify(store: &Store<HostState>, err: wasmtime::Error, fuel: u64) -> PluginError {
    if store.get_fuel().map(|left| left == 0).unwrap_or(false) {
        return PluginError::Timeout(fuel);
    }
    PluginError::Runtime(format!("插件执行失败：{}", describe(&err)))
}

/// wasmtime 的顶层错误只有「执行中断」一类的泛化描述，
/// 宿主导入返回的具体原因（例如权限拒绝）在原因链里，需要展开才能给用户看。
fn describe(err: &wasmtime::Error) -> String {
    let mut parts: Vec<String> = Vec::new();
    for cause in err.chain() {
        let text = cause.to_string();
        let text = text.trim();
        if text.is_empty() || parts.iter().any(|item| item == text) {
            continue;
        }
        parts.push(text.to_owned());
    }
    if parts.is_empty() {
        return err.to_string();
    }
    parts.join("；")
}

fn unpack(packed: i64) -> (usize, usize) {
    let value = packed as u64;
    ((value >> 32) as usize, (value & 0xffff_ffff) as usize)
}

fn pack(ptr: i32, len: usize) -> i64 {
    (((ptr as u32 as u64) << 32) | len as u32 as u64) as i64
}

fn guest_memory(caller: &mut Caller<'_, HostState>) -> Result<Memory, wasmtime::Error> {
    match caller.get_export(MEMORY_EXPORT) {
        Some(Extern::Memory(memory)) => Ok(memory),
        _ => Err(format_err!("插件未导出线性内存 `{MEMORY_EXPORT}`")),
    }
}

fn read_guest_string(
    caller: &mut Caller<'_, HostState>,
    ptr: i32,
    len: i32,
) -> Result<String, wasmtime::Error> {
    if ptr < 0 || len < 0 {
        return Err(format_err!("插件传入非法内存区间：ptr={ptr} len={len}"));
    }
    if len as usize > MAX_PAYLOAD_BYTES {
        return Err(format_err!(
            "插件传入的字符串超过 {MAX_PAYLOAD_BYTES} 字节限制"
        ));
    }
    let memory = guest_memory(caller)?;
    let mut buffer = vec![0u8; len as usize];
    memory
        .read(&*caller, ptr as usize, &mut buffer)
        .map_err(|_| format_err!("读取插件内存失败（越界）"))?;
    String::from_utf8(buffer).map_err(|_| format_err!("插件传入的字符串不是合法 UTF-8"))
}

fn write_guest_string(
    caller: &mut Caller<'_, HostState>,
    text: &str,
) -> Result<i64, wasmtime::Error> {
    let memory = guest_memory(caller)?;
    let alloc = match caller.get_export(ALLOC_EXPORT) {
        Some(Extern::Func(func)) => func,
        _ => return Err(format_err!("插件未导出 `{ALLOC_EXPORT}`")),
    };
    let alloc = alloc
        .typed::<i32, i32>(&mut *caller)
        .map_err(|_| format_err!("插件导出的 `{ALLOC_EXPORT}` 签名不匹配"))?;
    let len = text.len();
    let ptr = alloc.call(&mut *caller, len as i32)?;
    if ptr < 0 {
        return Err(format_err!("插件分配的缓冲区地址非法：{ptr}"));
    }
    memory
        .write(&mut *caller, ptr as usize, text.as_bytes())
        .map_err(|_| format_err!("写入插件内存失败"))?;
    Ok(pack(ptr, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::load_manifest;
    use std::path::{Path, PathBuf};

    /// 记录宿主回调的测试桩。
    #[derive(Default)]
    struct TestBridge {
        items: String,
        emitted: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl TestBridge {
        fn new(items: &str) -> Self {
            Self {
                items: items.to_owned(),
                emitted: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl HostBridge for TestBridge {
        fn read_items(&self) -> Result<String, String> {
            Ok(self.items.clone())
        }

        fn emit(&self, name: &str, payload: &str) -> Result<(), String> {
            self.emitted
                .lock()
                .expect("lock")
                .push((name.to_owned(), payload.to_owned()));
            Ok(())
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lagm-wasm-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    /// 把 WAT 编译成 wasm 并落盘，返回校验过的插件。
    fn plugin_with(name: &str, wat: &str, permissions: &[&str]) -> (PathBuf, ValidatedPlugin) {
        let dir = temp_dir(name);
        let manifest = serde_json::json!({
            "id": format!("com.example.{name}"),
            "name": name,
            "version": "0.1.0",
            "kind": "capability",
            "apiVersion": "1",
            "entry": "main.wasm",
            "capabilities": ["scanner.anime"],
            "permissions": permissions,
        });
        std::fs::write(
            dir.join(crate::manifest::MANIFEST_FILE),
            manifest.to_string(),
        )
        .expect("manifest");
        std::fs::write(dir.join("main.wasm"), wat).expect("wasm");
        let plugin = load_manifest(&dir).expect("valid plugin");
        (dir, plugin)
    }

    fn run(
        plugin: &ValidatedPlugin,
        input: &str,
        bridge: Arc<dyn HostBridge>,
    ) -> Result<InvocationReport, PluginError> {
        CapabilityRuntime::new()
            .expect("runtime")
            .invoke(plugin, input, bridge)
    }

    /// 最小的直通插件：alloc 用固定线性内存，invoke 原样返回入参。
    const ECHO_WAT: &str = r#"
(module
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $len)))
    (local.get $ptr))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
      (i64.extend_i32_u (local.get $len)))))
"#;

    #[test]
    fn echo_roundtrip() {
        let (dir, plugin) = plugin_with("echo", ECHO_WAT, &[]);
        let report = run(
            &plugin,
            r#"{"command":"parse"}"#,
            Arc::new(TestBridge::new("")),
        )
        .expect("invoke");
        assert_eq!(report.output, r#"{"command":"parse"}"#);
        assert!(report.fuel_used > 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_non_json_input() {
        let (dir, plugin) = plugin_with("bad-input", ECHO_WAT, &[]);
        let err = run(&plugin, "not json", Arc::new(TestBridge::new(""))).expect_err("应拒绝");
        assert!(err.to_string().contains("合法 JSON"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn read_items_requires_permission() {
        let wat = r#"
(module
  (import "lagm" "read_items" (func $read_items (result i64)))
  (memory (export "memory") 2)
  (global $next (mut i32) (i32.const 1024))
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $len)))
    (local.get $ptr))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (call $read_items)))
"#;
        let (dir, plugin) = plugin_with("no-perm", wat, &[]);
        let err = run(&plugin, "{}", Arc::new(TestBridge::new(r#"[{"id":"a"}]"#)))
            .expect_err("缺少权限应拒绝");
        assert!(err.to_string().contains("read:library"), "实际：{err}");
        std::fs::remove_dir_all(&dir).ok();

        let (dir2, allowed) = plugin_with("with-perm", wat, &["read:library"]);
        let report = run(&allowed, "{}", Arc::new(TestBridge::new(r#"[{"id":"a"}]"#)))
            .expect("有权限应放行");
        assert_eq!(report.output, r#"[{"id":"a"}]"#);
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn emit_requires_permission_and_is_forwarded() {
        let wat = r#"
(module
  (import "lagm" "emit" (func $emit (param i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "{\"name\":\"demo\",\"payload\":{\"a\":1}}")
  (global $next (mut i32) (i32.const 1024))
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $len)))
    (local.get $ptr))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (call $emit (i32.const 16) (i32.const 33))
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
      (i64.extend_i32_u (local.get $len)))))
"#;
        let (dir, plugin) = plugin_with("emit-denied", wat, &[]);
        let err = run(&plugin, "{}", Arc::new(TestBridge::new(""))).expect_err("缺少权限应拒绝");
        assert!(err.to_string().contains("emit:events"), "实际：{err}");
        std::fs::remove_dir_all(&dir).ok();

        let (dir2, allowed) = plugin_with("emit-ok", wat, &["emit:events"]);
        let bridge = Arc::new(TestBridge::new(""));
        run(&allowed, "{}", bridge.clone()).expect("有权限应放行");
        let emitted = bridge.emitted.lock().expect("lock").clone();
        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].0, "demo");
        assert_eq!(emitted[0].1, r#"{"a":1}"#);
        std::fs::remove_dir_all(&dir2).ok();
    }

    #[test]
    fn log_is_collected() {
        let wat = r#"
(module
  (import "lagm" "log" (func $log (param i32 i32 i32)))
  (memory (export "memory") 2)
  (data (i32.const 16) "hello")
  (global $next (mut i32) (i32.const 1024))
  (func (export "alloc") (param $len i32) (result i32)
    (local $ptr i32)
    (local.set $ptr (global.get $next))
    (global.set $next (i32.add (global.get $next) (local.get $len)))
    (local.get $ptr))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (call $log (i32.const 1) (i32.const 16) (i32.const 5))
    (i64.or
      (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
      (i64.extend_i32_u (local.get $len)))))
"#;
        let (dir, plugin) = plugin_with("log", wat, &[]);
        let report = run(&plugin, "{}", Arc::new(TestBridge::new(""))).expect("invoke");
        assert_eq!(report.logs, vec!["[info] hello".to_owned()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn infinite_loop_is_stopped_by_fuel() {
        let wat = r#"
(module
  (memory (export "memory") 1)
  (func (export "alloc") (param $len i32) (result i32) (i32.const 1024))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (loop $spin (br $spin))
    (i64.const 0)))
"#;
        let (dir, plugin) = plugin_with("spin", wat, &[]);
        let started = Instant::now();
        let err = run(&plugin, "{}", Arc::new(TestBridge::new(""))).expect_err("应超时中止");
        assert!(matches!(err, PluginError::Timeout(_)), "实际：{err}");
        assert!(started.elapsed().as_secs() < 30, "超时中止耗时过长");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_entry_export_is_reported() {
        let wat = r#"
(module
  (memory (export "memory") 1)
  (func (export "alloc") (param $len i32) (result i32) (i32.const 0)))
"#;
        let (dir, plugin) = plugin_with("no-entry", wat, &[]);
        let err = run(&plugin, "{}", Arc::new(TestBridge::new(""))).expect_err("应报错");
        assert!(err.to_string().contains("invoke"), "实际：{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn oversized_output_is_rejected() {
        let wat = r#"
(module
  (memory (export "memory") 1)
  (func (export "alloc") (param $len i32) (result i32) (i32.const 1024))
  (func (export "invoke") (param $ptr i32) (param $len i32) (result i64)
    (i64.or
      (i64.shl (i64.const 1024) (i64.const 32))
      (i64.const 4194304))))
"#;
        let (dir, plugin) = plugin_with("big-out", wat, &[]);
        let err = run(&plugin, "{}", Arc::new(TestBridge::new(""))).expect_err("应拒绝");
        assert!(err.to_string().contains("超过"), "实际：{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_ui_plugin() {
        let dir = temp_dir("ui-kind");
        let manifest = serde_json::json!({
            "id": "com.example.ui",
            "name": "ui",
            "version": "0.1.0",
            "kind": "ui",
            "apiVersion": "1",
            "entry": "main.js",
            "mount": "settings.section"
        });
        std::fs::write(
            dir.join(crate::manifest::MANIFEST_FILE),
            manifest.to_string(),
        )
        .expect("manifest");
        std::fs::write(dir.join("main.js"), "export function mount() {}").expect("js");
        let plugin = load_manifest(Path::new(&dir)).expect("valid");
        let err = run(&plugin, "{}", Arc::new(TestBridge::new(""))).expect_err("应拒绝");
        assert!(err.to_string().contains("不能作为能力插件"), "实际：{err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
