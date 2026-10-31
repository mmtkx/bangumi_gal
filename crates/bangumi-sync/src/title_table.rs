//! 内置的「罗马音 → 日文原名」查询。
//!
//! 数据由 `scripts/build-title-table.mjs` 从 anime-offline-database 生成，
//! 再由 `build.rs` 编译成 FST，这里只做零拷贝查询。
//!
//! 用途：下载资源的文件名常常是罗马音（`Shingeki no Kyojin - 01.mkv`），
//! 而 Bangumi 条目标题是日文原名，直接搜命中率为 0。先查表拿到日文名再搜，
//! 才能让自动匹配真正生效。

use std::sync::OnceLock;

static FST_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/titles.fst"));
static BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/titles.bin"));

fn map() -> &'static fst::Map<&'static [u8]> {
    static MAP: OnceLock<fst::Map<&'static [u8]>> = OnceLock::new();
    MAP.get_or_init(|| fst::Map::new(FST_BYTES).expect("内置标题对照表损坏"))
}

/// 按罗马音查日文原名；查不到返回 `None`。
///
/// 查询前会用与建表时一致的规则归一化（小写、只保留字母数字），
/// 因此 `Kaguya-sama wa Kokurasetai` 与 `kaguya_sama wa kokurasetai` 等价。
pub fn native_title_for(romaji: &str) -> Option<&'static str> {
    let key = crate::matching::normalize_key(romaji);
    if key.is_empty() {
        return None;
    }
    let offset = map().get(key.as_bytes())? as usize;
    let len_bytes = BLOB.get(offset..offset + 4)?;
    let len = u32::from_le_bytes(len_bytes.try_into().ok()?) as usize;
    let start = offset + 4;
    std::str::from_utf8(BLOB.get(start..start + len)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_up_common_romaji_titles() {
        assert_eq!(native_title_for("Shingeki no Kyojin"), Some("進撃の巨人"));
        assert_eq!(native_title_for("shingeki_no_kyojin"), Some("進撃の巨人"));
        assert_eq!(
            native_title_for("Bocchi the Rock!"),
            Some("ぼっち・ざ・ろっく!")
        );
        // 查不到时必须是 None，不能返回别的条目。
        assert_eq!(native_title_for("Definitely Not An Anime Title"), None);
        assert_eq!(native_title_for(""), None);
    }
}
