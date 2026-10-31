# 第三方数据与许可证声明

本项目代码使用 MIT 许可证（见各 `Cargo.toml` 的 `license` 字段）。
但**随程序分发的数据文件**另有其许可证，必须一并遵守。

## 动画标题对照表

- 文件：`crates/bangumi-sync/data/titles.tsv`
- 用途：用罗马音文件名查 Bangumi 的日文原名，提高自动匹配命中率；构建时由
  `crates/bangumi-sync/build.rs` 编译成 FST 并嵌入可执行文件。
- 来源：anime-offline-database（manami-project）
  <https://github.com/manami-project/anime-offline-database>
- 数据快照：`lastUpdate = 2026-07-04`，41,537 条目（压缩后 93,105 条罗马音映射）。
- 生成方式：`node scripts/build-title-table.mjs`，只做「抽取标题与同义词、
  按脚本筛选日文名、归一化后去重」，未修改原始语义。
- 许可证：**Open Data Commons Open Database License (ODbL) v1.0**
  \+ Database Contents License (DbCL) v1.0
  <https://opendatacommons.org/licenses/odbl/1-0/>
- 义务说明：`titles.tsv` 属于该数据集的衍生数据库，因此**该文件本身也以 ODbL v1.0
  提供**。它随源代码一起保存在仓库中，任何获得本软件的人都可以取到、修改并以同样
  许可证再分发。代码（Rust / TypeScript 部分）不受 ODbL 影响，仍为 MIT。
- 署名：数据由 manami-project 及其上游元数据提供方（MyAnimeList、AniList、Kitsu、
  AniDB、Anime-Planet、AniSearch、SIMKL、AnimeCountdown、Anime News Network、
  LiveChart.me）汇集。

## 更新方式

数据快照会过期（新番不会出现在表里）。更新步骤：

```bash
# 1. 从 https://github.com/manami-project/anime-offline-database/releases 下载
#    anime-offline-database-minified.json
# 2. 重新生成对照表
node scripts/build-title-table.mjs <下载到的 json 路径>
# 3. 更新本文件里的「数据快照」日期，然后重新构建
pwsh -File scripts/verify.ps1
```

`titles.tsv` 变化后 build.rs 会自动重编 FST，不需要手动清理 target。
