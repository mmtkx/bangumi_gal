# LAGM

本地动画 / 游戏库管理客户端（Windows 桌面，Tauri v2 + React + Rust）。

把散落在硬盘上的番剧与游戏目录扫描成条目，自动与 Bangumi 匹配绑定，同步收藏状态与观看进度。
**只做本地文件的管理与元数据同步，不提供任何资源下载。**

## 功能

- **扫描**：目录型条目（一部番一个文件夹）与「根目录松散文件」两种组织方式都支持；松散视频按文件名归组
- **自动匹配**：用内置「罗马音 → 日文原名」对照表把 `Shingeki no Kyojin` 这类文件名映射到 Bangumi 条目；文件夹名对不上时还会退回用里面的文件名再试一次
- **播放**：支持外部播放器（PotPlayer / VLC / mpv / MPC），未配置时走系统默认关联程序；断点续播
- **进度**：按集记录观看进度，本地保存并与 Bangumi 双向同步（冲突可批量裁决）
- **分类**：想看 / 在看 / 看过 / 搁置 / 抛弃，动画与游戏措辞分开（想看 vs 想玩）
- **插件**：支持 UI 插件与 WASM 能力插件

## 安装

到 [Releases](https://github.com/mmtkx/bangumi_gal/releases) 下载最新的 `LAGM_x.y.z_x64-setup.exe`，双击安装即可。

系统需 Windows 10 / 11 并具备 WebView2（Win11 自带，Win10 多数已预装；缺失时[手动安装](https://developer.microsoft.com/microsoft-edge/webview2/)）。

## 环境要求

| 依赖 | 版本 | 说明 |
|---|---|---|
| Windows | 10 / 11 | 仅支持 Windows |
| WebView2 | 系统自带或手动安装 | Tauri 运行必需 |
| Rust | 1.95+ | `rustup` 安装 |
| Node.js | 20+ | 建议 22 |
| pnpm | 9+ | `npm i -g pnpm` |

## 如果你是开发者

### 开始开发

```powershell
# 1. 准备内置 Bangumi 应用凭据（真实 secret 不入库，需自行复制模板）
copy crates/app-services/src/builtin.rs.example crates/app-services/src/builtin.rs
#    然后在 builtin.rs 里填入你自己的 client_id / client_secret
#    Bangumi 后台回调地址填 http://127.0.0.1:27432/callback

# 2. 安装依赖
pnpm install

# 3. 开发模式
pnpm tauri dev

# 4. 分别执行 Rust 和前端检查
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
pnpm format:check
pnpm typecheck
pnpm lint
pnpm build
```

### 打包

```powershell
# 对外分发必须带路径脱敏，否则 exe 里会写进编译机的用户名与仓库路径（本机自用可省掉这一行）
$env:RUSTFLAGS = "--remap-path-prefix=$env:USERPROFILE=cargo --remap-path-prefix=$PWD=repo"
pnpm tauri build
```

产物：`target/release/bundle/nsis/LAGM_x.y.z_x64-setup.exe`。

构建完清掉 `RUSTFLAGS`（`Remove-Item Env:RUSTFLAGS`）：它参与编译指纹，留着会让后续 `cargo` 全量重建。

## 数据存放位置

| 内容 | 路径 |
|---|---|
| 条目库、设置 | `%APPDATA%\com.lagm.desktop\library.db` |
| 日志 | `%LOCALAPPDATA%\LAGM\logs`（滚动保存） |
| 封面缓存 | 设置页可配置，默认在用户缓存目录 |
| Bangumi token | Windows 凭据管理器（不落明文文件） |

软件只读媒体文件；「删除资源」是需要二次确认的显式操作，默认移入回收站。

## 第三方数据

自动匹配用到的「罗马音 → 日文原名」对照表来自
[anime-offline-database](https://github.com/manami-project/anime-offline-database)，
以 **ODbL v1.0 + DbCL v1.0** 分发。衍生文件 `crates/bangumi-sync/data/titles.tsv`
同样以 ODbL v1.0 提供，来源、快照日期与更新方式见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

## 许可

代码以 [MIT](LICENSE) 许可发布；第三方数据文件的许可见上节。

内置 Bangumi 应用凭据的性质（属公开信息、不构成漏洞）与安全范围，见 [SECURITY.md](SECURITY.md)。

## 二次开发与分发

欢迎 fork、修改和再分发，不需要事先征得同意。分发时注意四件事：

1. **保留许可与版权声明**——这是 MIT 唯一的硬性要求：再分发源码或二进制时，须附带 [LICENSE](LICENSE) 与原版权声明。
2. **换掉内置的 Bangumi 应用凭据**——`crates/app-services/src/builtin.rs` 里是本项目自己的 OAuth 应用。照原样分发会让所有使用者共用同一份配额，一方被限流会牵连所有人；请在 Bangumi 开发者页面注册自己的应用后替换（回调地址见「开始开发」）。
3. **衍生数据库仍需 ODbL**——若修改或再分发 `crates/bangumi-sync/data/titles.tsv`，衍生数据同样要以 ODbL v1.0 提供，详见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。
4. **不要借名义背书**——MIT 不授予商标权。修改版请改名或注明「基于 LAGM 修改」，避免让人误以为是原作者发布的官方版本。
5. **标明本项目的仓库地址**

只是自己改着用的话，除第 2 条外都不必操心。

## 免责声明

本项目仅供个人整理本地已有资源使用，不提供、不托管、不分发任何影视或游戏内容。

- 本项目是第三方工具，与 Bangumi 官方无隶属或合作关系；账号的注册、登录、状态变更（如限流、封禁）均由其平台规则决定。
- **通过非本项目渠道获取的版本**（第三方修改版、他人重新打包的分发件）若造成账号异常、数据丢失或隐私泄露，本项目概不负责。
- 因使用本工具违反 Bangumi 服务条款或当地法律法规而产生的后果，由使用者自行承担。

软件按「现状」提供，不对因使用或无法使用本软件造成的直接或间接损失承担责任，完整条款见 [LICENSE](LICENSE)。
