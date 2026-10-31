//! 把 `data/titles.tsv` 编译成 FST + 字符串块。
//!
//! 表数据是「罗马音 → 日文原名」，来源见 `THIRD-PARTY-NOTICES.md`。
//! 放在 build.rs 而不是运行期解析 TSV，是为了让查询零开销、零堆分配：
//! FST 是有序前缀压缩结构，`include_bytes!` 直接静态映射，查一次是 O(长度)。
//!
//! 输出两个文件到 OUT_DIR：
//! - `titles.fst`：key → 值在 blob 中的偏移
//! - `titles.bin`：每条值是「u32 长度 + UTF-8 字节」

use std::io::BufWriter;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=data/titles.tsv");

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let source = manifest.join("data/titles.tsv");
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));

    let text = std::fs::read_to_string(&source).unwrap_or_else(|err| {
        panic!(
            "读取 {} 失败：{err}\n该文件由 `node scripts/build-title-table.mjs` 生成，不能缺失。",
            source.display()
        )
    });

    // FST 要求按 key 字节序插入；TSV 已是升序，但这里再排一次，
    // 避免将来换工具生成时因排序规则差异（UTF-16 vs 字节序）导致构建失败。
    let mut entries: Vec<(&str, &str)> = text
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .filter(|(key, _)| !key.is_empty())
        .collect();
    entries.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));

    let mut blob: Vec<u8> = Vec::with_capacity(entries.len() * 8);
    // 多个罗马音写法（不同站的别名）常指向同一部番，重复存字符串会白白涨 2 MB 量级，
    // 这里按值去重，FST 里只记偏移。
    let mut value_offsets: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    let mut builder = fst::MapBuilder::new(BufWriter::new(
        std::fs::File::create(out_dir.join("titles.fst")).expect("创建 titles.fst"),
    ))
    .expect("初始化 FST 构建器");

    for (key, value) in &entries {
        let offset = match value_offsets.get(value) {
            Some(offset) => *offset,
            None => {
                let offset = blob.len() as u64;
                let bytes = value.as_bytes();
                blob.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                blob.extend_from_slice(bytes);
                value_offsets.insert(value, offset);
                offset
            }
        };
        builder.insert(key, offset).expect("写入 FST 条目");
    }
    builder.finish().expect("完成 FST 构建");

    std::fs::write(out_dir.join("titles.bin"), &blob).expect("写入 titles.bin");
}
