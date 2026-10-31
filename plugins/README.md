# 插件开发指南

LAGM 支持两种插件，用来扩展客户端而不改动主程序：

| 类型 | 入口 | 运行环境 | 用途 |
| --- | --- | --- | --- |
| **UI 插件** | `.js` / `.mjs` | 前端（WebView，Blob 动态 import） | 往界面里挂面板、加设置分区 |
| **能力插件** | `.wasm` | wasmtime 沙箱 | 无副作用的计算，例如自定义文件名解析 |

两种插件都由 `manifest.json` 描述，放进插件根目录即被自动发现并登记（默认禁用），
在「设置 → 插件」里启用后生效。

## 1. 目录结构

每个插件独占一个子目录，至少包含 `manifest.json` 与入口文件：

```
plugins/
  demo-panel/
    manifest.json
    main.js              # kind=ui
  demo-scanner/
    manifest.json
    main.wasm            # kind=capability（由 src/lib.rs 编译得到）
    src/lib.rs           # 插件源码（示例；编译产物才是入口）
```

应用内的插件根目录是 `%APPDATA%\com.lagm.desktop\plugins`（与 `library.db` 同目录）。
仓库里的 `plugins/` 只是示例；把目录复制过去重启即可自动登记，也可在设置页选择任意目录安装。

## 2. manifest.json 参考

```json
{
  "id": "com.lagm.demo-panel",
  "name": "示例面板",
  "version": "0.1.0",
  "kind": "ui",
  "apiVersion": "1",
  "entry": "main.js",
  "capabilities": [],
  "permissions": ["read:library"],
  "mount": "library.item.detail.panel"
}
```

字段与校验规则：

| 字段 | 必填 | 说明 |
| --- | --- | --- |
| `id` | 是 | 全局唯一标识。字母/数字/`.`/`_`/`-`，≤128 字符 |
| `name` | 是 | 显示名，非空且 ≤100 字符 |
| `version` | 是 | 任意非空字符串（语义化版本建议 `x.y.z`） |
| `kind` | 是 | `ui` 或 `capability` |
| `apiVersion` | 是 | 当前仅支持 `"1"` |
| `entry` | 是 | 入口相对路径，不能含 `..`，必须落在插件目录内；`ui` 用 `.js/.mjs`，`capability` 用 `.wasm` |
| `capabilities` | 否 | 声明本插件实现的能力，点分标识、字母开头，如 `scanner.anime` |
| `permissions` | 否 | 权限白名单，见下 |
| `mount` | 仅 ui | 挂载点，必须是已知值；`capability` 不得声明 |

校验失败会记录错误并显示在插件列表里，不会崩溃宿主。

## 3. 权限模型

插件能做什么由 manifest 声明的 `permissions` 决定，未声明即调用会直接失败。

| 权限 | 宿主 API | 状态 |
| --- | --- | --- |
| `read:library` | `read_items()` / `host.listItems()` | ✅ 已实现（读条目库快照） |
| `emit:events` | `emit()` | ✅ 已实现（发插件事件） |
| `read:filesystem` | — | ⏳ 已预留，尚未接入任何宿主调用 |
| `write:library` | — | ⏳ 已预留，尚未接入任何宿主调用 |

也就是说，现阶段能用的只有 `read:library` 和 `emit:events`，另外两个先声明备用。

## 4. UI 插件

### 4.1 入口契约

入口文件是 ES 模块，`export default` 一个对象：

```js
export default {
  // 挂载：宿主注入 container（DOM 节点）、host（宿主 API）、context（挂载点上下文）。
  // 返回一个清理函数（可选），插件被卸载或页面切换时调用。
  mount(container, host, context) {
    container.textContent = `你好，这是 ${host.pluginId}`;
    return () => { /* 清理 DOM、定时器、监听器 */ };
  },
};
```

### 4.2 宿主 API（`host`）

```ts
interface PluginHostApi {
  apiVersion: '1';
  pluginId: string;
  listItems(): Promise<ItemSummary[]>;        // 需 read:library
  emit(name: string, payload?: unknown): Promise<void>;  // 需 emit:events
}
```

- `listItems()` 返回条目摘要数组，字段见 `apps/desktop/src/api.ts` 的 `ItemSummary`。
- `emit()` 向主干道发事件，前端监听主题 `plugin:event:emitted`，`payload` 形如
  `{ "id": "<插件id>", "name": "...", "payload": {...} }`。

### 4.3 挂载点（`mount`）

当前支持两个挂载点值：

| 挂载点 | 状态 | `context` 内容 |
| --- | --- | --- |
| `library.item.detail.panel` | ✅ 已渲染（条目详情页面板区） | `{ itemId: string }` |
| `settings.section` | ⏳ 已声明，前端尚未挂载（预留） | — |

### 4.4 限制

- 入口源码 ≤ 512 KB，读取后经 Blob URL 动态 `import`（CSP 已放行 `blob:`）。
- 单个插件挂载或运行异常只会隔离它自己：出错面板显示「插件运行出错，已隔离」，不影响主界面和其它插件。
- 每次宿主 API 调用都回到后端重新做权限校验，前端不替插件把关。

### 4.5 最小示例

`plugins/demo-panel/main.js` 是一个可运行的 UI 插件：挂载面板、读条目数、发事件，照着改即可。

## 5. 能力插件（WASM）ABI

沙箱由 wasmtime 提供。插件与宿主只通过线性内存里的 UTF-8 JSON 交换数据，
**不需要** wasm-bindgen 之类的绑定生成器。

### 5.1 插件必须导出

| 导出 | 签名 | 说明 |
| --- | --- | --- |
| `memory` | 线性内存 | 宿主与插件共享的读写区 |
| `alloc(len: i32) -> i32` | — | 分配 `len` 字节，返回指针；宿主把输入 JSON 写进去 |
| `invoke(ptr: i32, len: i32) -> i64` | — | 唯一入口；返回 `(out_ptr << 32) | out_len` 指向输出 JSON |

### 5.2 宿主导入（模块名固定为 `lagm`）

| 导入 | 签名 | 权限 | 说明 |
| --- | --- | --- | --- |
| `log` | `(level: i32, ptr: i32, len: i32)` | 无 | `level` 0 debug / 1 info / 2 warn / 3 error |
| `now_ms` | `() -> i64` | 无 | 当前 Unix 毫秒时间戳 |
| `read_items` | `() -> i64` | `read:library` | 条目库快照（JSON 数组文本），返回同样打包的指针 |
| `emit` | `(ptr: i32, len: i32)` | `emit:events` | 参数为 `{"name":"...","payload":{...}}` |

### 5.3 返回打包约定

`invoke` 的返回值是一个 `i64`，高 32 位是输出指针、低 32 位是输出长度：

```text
return (ptr << 32) | len
```

输出必须是合法 JSON 文本（宿主会校验，解析失败按运行时错误处理）。

### 5.4 沙箱与限额

| 项 | 限制 |
| --- | --- |
| 指令预算 | 5000 万步，超限强制中止（`PluginError::Timeout`），宿主进程不受影响 |
| 线性内存 | ≤ 64 MiB；表、实例数同样受限 |
| 输入 / 输出 | 各 ≤ 1 MiB |
| 日志 | 单次调用累计 ≤ 8 KB |
| 线程 | 未启用 wasm 线程特性，只能单线程执行 |

### 5.5 Rust 写法

`plugins/demo-scanner/src/lib.rs` 是完整最小样例：手动 `alloc` / `invoke`，`extern "C"` 声明 `lagm` 的 `log`，
不依赖任何外部 crate。要点：

```rust
#[link(wasm_import_module = "lagm")]
extern "C" {
    fn log(level: i32, ptr: *const u8, len: usize);
}

#[no_mangle]
pub extern "C" fn alloc(len: usize) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len.max(1));
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf); // 单次调用只分配一次，实例随调用销毁
    ptr
}

#[no_mangle]
pub extern "C" fn invoke(ptr: *const u8, len: usize) -> u64 {
    // 读 ptr/len 得到输入 JSON，处理，再把输出 JSON 写回内存并打包返回
    // (out_ptr << 32) | out_len
}
```

## 6. 构建能力插件

`demo-scanner` 是独立 crate（不在主工作区），构建流程：

```bash
cd plugins/demo-scanner
rustup target add wasm32-unknown-unknown
cargo build --release --target wasm32-unknown-unknown
cp target/wasm32-unknown-unknown/release/demo_scanner.wasm main.wasm
```

仓库里的 `main.wasm` 已按此构建，直接登记即可运行。

## 7. 安装、启用与运行

1. **安装**：把插件目录放到 `%APPDATA%\com.lagm.desktop\plugins`，或在「设置 → 插件 → 安装插件…」选择目录（不复制文件，只登记）。
2. **启用**：插件默认禁用，在插件列表里开启。
3. **运行**：
   - UI 插件：启用后自动挂载到声明的 `mount` 点（进详情页/设置页即可见）。
   - 能力插件：在设置页「运行…」手动触发，输入 JSON，返回 `{ output, logs, fuelUsed, durationMs }`。

卸载（`plugin_uninstall`）只移除注册表登记，插件文件保留，方便重新安装。

## 8. 插件事件

能力插件或 UI 插件都可以 `emit`（需 `emit:events`）。事件统一走主干道，主题为
`plugin:event:emitted`，信封 `payload` 形如：

```json
{ "id": "com.lagm.demo-scanner", "name": "scan.finished", "payload": { "count": 3 } }
```

前端在 `App.tsx` 订阅该主题即可消费；`name` 由插件自定义（≤64 字符，字母/数字/`.`/`_`/`-`）。

## 9. 安全边界（设计保证）

- 能力插件跑在 wasmtime 沙箱里，指令预算与内存双上限，超限即中止，**无法拖垮宿主进程**。
- 未声明权限的宿主 API 调用直接返回错误，不会静默放行。
- UI 插件以 Blob 动态 import，宿主 API 每个方法都回到后端做权限校验。
- 插件读写数据库只能通过宿主 API，拿不到直接连接。
- 插件无法访问未授权的文件系统（`read:filesystem` 尚未实现，`entry` 越界在清单校验阶段即拒绝）。

## 10. 当前限制与后续

- 挂载点目前只有 `library.item.detail.panel` 真正渲染；`settings.section` 已声明但前端尚未挂载。
- `read:filesystem` / `write:library` 是预留权限，尚无对应宿主 API。
- 能力插件尚未接入扫描流水线：现在由设置页「运行…」手动触发，接进 `scanner.anime` 是后续验收项。

---

示例与契约的最新实现以代码为准：`crates/plugin-host/src/{manifest.rs,wasm.rs}`、
`apps/desktop/src/plugins/host.ts`、`plugins/demo-*`。
