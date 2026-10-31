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
- **插件**：UI 插件（Blob 挂载）与能力插件（wasmtime 沙箱）两种

## 环境要求

| 依赖 | 版本 | 说明 |
|---|---|---|
| Windows | 10 / 11 | 仅支持 Windows |
| WebView2 | 系统自带或[手动安装](https://developer.microsoft.com/microsoft-edge/webview2/) | Tauri 运行必需 |
| Rust | 1.95+ | `rustup` 安装 |
| Node.js | 20+ | 建议 22 |
| pnpm | 9+ | `npm i -g pnpm` |

## 开始开发

```powershell
# 1. 准备内置 Bangumi 应用凭据（真实 secret 不入库，需自行复制模板）
copy crates/app-services/src/builtin.rs.example crates/app-services/src/builtin.rs
#    然后在 builtin.rs 里填入你自己的 client_id / client_secret
#    Bangumi 后台回调地址填 http://127.0.0.1:27432/callback

# 2. 安装依赖
pnpm install

# 3. 开发模式
pnpm tauri dev

# 4. 收尾验证（fmt + clippy + test + 前端检查，一次跑完）
pwsh -File scripts/verify.ps1 -Frontend
```

## 打包

```powershell
# 对外分发：抹掉构建机路径 + strip，并生成 NSIS 安装包
pwsh -File scripts/build-release.ps1 -Bundled

# 本机自用（允许带构建机路径）
pnpm tauri build
```

产物：`target/release/bundle/nsis/LAGM_x.y.z_x64-setup.exe`

## 数据存放位置

| 内容 | 路径 |
|---|---|
| 条目库、设置 | `%APPDATA%\com.lagm.desktop\library.db` |
| 日志 | `%LOCALAPPDATA%\LAGM\logs`（滚动，单文件 5 MiB × 10） |
| 封面缓存 | 设置页可配置，默认在用户缓存目录 |
| Bangumi token | Windows 凭据管理器（不落明文文件） |

软件只读媒体文件；「删除资源」是需要二次确认的显式操作，默认移入回收站。

## 关于内置 Bangumi 凭据

本项目以 OAuth 公共客户端形式内置自己的 `client_id` / `client_secret`，因此**这组凭据应当视为公开信息**（任何拿到安装包的人都能从二进制里提取）——这是 OAuth 公共客户端的常态，不构成漏洞。

真正需要保密的是用户的 access token / refresh token，它们只存于各人本机的凭据管理器，不会随安装包分发。

## 第三方数据

自动匹配用到的「罗马音 → 日文原名」对照表来自
[anime-offline-database](https://github.com/manami-project/anime-offline-database)，
以 **ODbL v1.0 + DbCL v1.0** 分发。衍生文件 `crates/bangumi-sync/data/titles.tsv`
同样以 ODbL v1.0 提供，来源、快照日期与更新方式见 [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md)。

## 许可

代码以 [MIT](LICENSE) 许可发布；第三方数据文件的许可见上节。

## 免责声明

本项目仅供个人整理本地已有资源使用，不提供、不托管、不分发任何影视或游戏内容。
使用 Bangumi API 时请遵守其服务条款。
