//! 标题归一化与 Bangumi 搜索结果匹配。
//!
//! 扫描得到的文件夹名通常带发布组、分辨率、季数等噪声（如
//! `[VCB-Studio] 夏日重现 [01][1080p]`）。这里先归一化成可比对的核心标题，
//! 再用「完全相等 → 包含 → 字符二元组 Dice」打分，并按季数加权，
//! 只有足够自信且不歧义时才自动绑定。

use std::collections::HashMap;
use std::sync::OnceLock;

use regex::Regex;

use crate::types::SubjectSearchHit;

/// 自动绑定阈值：低于该分数留给用户手动绑定。
pub const AUTO_BIND_THRESHOLD: f64 = 0.72;
/// 与次优的差距小于该值时视为歧义，不自动绑定。
const AMBIGUITY_MARGIN: f64 = 0.05;

fn bracket_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\[【（(][^\]】）)]*[\]】）)]").expect("bracket regex"))
}

fn noise_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"(?i)\b(1080p|720p|2160p|4k|8k|x264|x265|h\.?264|h\.?265|hevc|avc|aac|flac|bluray|bdrip|webrip|web-dl|hdtv|remux|10bit|8bit|hi10p|ma10p|v2|v3|rev|gb|big5|19\d{2}|20\d{2})\b|步兵版|骑兵版|无修|无码|有码|汉化版|汉化|精翻|机翻|生肉|熟肉|整合版|整合|破解版|破解|免安装|绿色版|dl版|dvdpg|简繁|简日|繁日|内封|内嵌|字幕|合集|全集|完结|全\d+话",
        )
        .expect("noise regex")
    })
}

/// 中文季数：第二季 / 第2期 / 第二部。
fn season_cn_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"第([一二三四五六七八九十百零0-9]+)[季期部]").expect("season cn"))
}

/// 英文/简写季数：Season 2 / S2 / Part 2。
fn season_en_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)\b(?:season|part)\s*(\d{1,2})\b|\bs(\d{1,2})\b").expect("season en")
    })
}

/// 只保留小写字母、数字、汉字、平假名、片假名。
fn keep_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"[^0-9a-z\u{4e00}-\u{9fff}\u{3040}-\u{30ff}]").expect("keep regex")
    })
}

fn parse_cn_number(raw: &str) -> Option<u32> {
    if let Ok(value) = raw.parse::<u32>() {
        return Some(value);
    }
    const DIGITS: [char; 10] = ['零', '一', '二', '三', '四', '五', '六', '七', '八', '九'];
    let mut total = 0u32;
    let mut current = 0u32;
    let mut seen = false;
    for ch in raw.chars() {
        if ch == '十' {
            let base = if current == 0 { 1 } else { current };
            total += base * 10;
            current = 0;
            seen = true;
        } else {
            let index = DIGITS.iter().position(|digit| *digit == ch)?;
            current = index as u32;
            seen = true;
        }
    }
    if !seen {
        return None;
    }
    Some(total + current)
}

/// 提取季数（第二季 / Season 2 / S2 / Part 2）。没有季数信息返回 None。
pub fn season_of(raw: &str) -> Option<u32> {
    let lowered = raw.to_lowercase();
    if let Some(captures) = season_cn_re().captures(&lowered) {
        if let Some(value) = captures.get(1).and_then(|m| parse_cn_number(m.as_str())) {
            return Some(value);
        }
    }
    if let Some(captures) = season_en_re().captures(&lowered) {
        for group in 1..=2 {
            if let Some(value) = captures
                .get(group)
                .and_then(|m| m.as_str().parse::<u32>().ok())
            {
                return Some(value);
            }
        }
    }
    None
}

fn strip_season(value: &str) -> String {
    let without_cn = season_cn_re().replace_all(value, " ");
    season_en_re().replace_all(&without_cn, " ").into_owned()
}

/// 归一化标题：繁转简、去括号内容、发布噪声、季数，只留核心字符。
pub fn normalize_title(raw: &str) -> String {
    // 先统一成简体，避免「無限煉姦」与「无限炼奸」被当成两个标题。
    let simplified = fast2s::convert(raw);
    let lowered = simplified.to_lowercase();
    let no_brackets = bracket_re().replace_all(&lowered, " ");
    let no_noise = noise_re().replace_all(&no_brackets, " ");
    let no_season = strip_season(&no_noise);
    keep_re().replace_all(&no_season, "").into_owned()
}

fn bigrams(value: &str) -> HashMap<(char, char), usize> {
    let chars: Vec<char> = value.chars().collect();
    let mut map = HashMap::new();
    if chars.len() < 2 {
        return map;
    }
    for pair in chars.windows(2) {
        *map.entry((pair[0], pair[1])).or_insert(0) += 1;
    }
    map
}

/// 字符二元组 Dice 系数（0..=1），对中文标题较稳健。
fn dice(a: &str, b: &str) -> f64 {
    let left = bigrams(a);
    let right = bigrams(b);
    if left.is_empty() || right.is_empty() {
        return if a == b { 1.0 } else { 0.0 };
    }
    let mut overlap = 0usize;
    for (gram, count) in &left {
        if let Some(other) = right.get(gram) {
            overlap += (*count).min(*other);
        }
    }
    let total = left.values().sum::<usize>() + right.values().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    (2.0 * overlap as f64) / total as f64
}

/// 两个已归一化标题的相似度。
pub fn similarity(a: &str, b: &str) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    if a == b {
        return 1.0;
    }
    let (short, long) = if a.chars().count() <= b.chars().count() {
        (a, b)
    } else {
        (b, a)
    };
    if short.chars().count() >= 3 && long.contains(short) {
        return 0.85;
    }
    dice(a, b)
}

/// 按季数调整得分：无季数视为第 1 季，季数不一致时降权。
fn season_adjust(base: f64, query_season: Option<u32>, candidate_season: Option<u32>) -> f64 {
    let query = query_season.unwrap_or(1);
    let candidate = candidate_season.unwrap_or(1);
    if query == candidate {
        base
    } else {
        base * 0.4
    }
}

/// 生成搜索关键词候选（去重后最多 3 个）。
///
/// Bangumi 搜索对杂质很敏感，文件夹名带 `[组名]`、`- 第01话` 时容易搜不到，
/// 这里额外给出「去括号主标题」与「分隔符前缀」两个变体。
pub fn search_keywords(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return out;
    }
    let no_bracket = bracket_re().replace_all(trimmed, " ").trim().to_string();
    let latin = looks_latin(trimmed);

    // 罗马音标题优先换成本地对照表里的日文原名：命中率远高于原词与假名。
    let native = if latin {
        crate::title_table::native_title_for(&no_bracket)
    } else {
        None
    };
    if let Some(value) = native {
        push_unique(&mut out, value.to_owned());
    }

    push_unique(&mut out, trimmed.to_owned());
    push_unique(&mut out, no_bracket.clone());

    for separator in [" - ", " – ", "_", "：", ":"] {
        if let Some((head, _)) = no_bracket.split_once(separator) {
            let head = head.trim();
            if head.chars().count() >= 2 {
                push_unique(&mut out, head.to_owned());
            }
        }
    }

    // 表里没有时才退回假名音译，聊胜于无。
    if latin && native.is_none() {
        if let Some(kana) = romaji_to_hiragana(&no_bracket) {
            push_unique(&mut out, kana);
        }
    }

    out.truncate(4);
    out
}

/// 归一化查询键：小写，只保留字母、数字与假名/汉字。
///
/// 建表脚本与运行时必须使用完全相同的规则，否则查不到。
pub fn normalize_key(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
        }
    }
    out
}

/// 是否基本是拉丁字母（罗马音/英文标题）。
///
/// 只排除极少量的非拉丁字符，保证「中日文混排」不会被误判成罗马音。
fn looks_latin(text: &str) -> bool {
    let mut letters = 0usize;
    let mut others = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_alphabetic() {
            letters += 1;
        } else if !ch.is_whitespace()
            && !matches!(ch, '-' | '_' | '.' | ':' | '&' | '\'' | '!' | '?')
        {
            others += 1;
        }
    }
    letters >= 4 && others * 4 <= letters
}

/// 罗马音转平假名，用于检索日文原名。
///
/// 这是「够用就好」的音节表：覆盖五十音与常见拗音，长音、促音、拨音按通用规则处理。
/// 无法识别的字母（如 `l`、`x` 开头的外来音）会被跳过，宁可少几个字也不要整串失效。
pub fn romaji_to_hiragana(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut out = String::new();
    let mut index = 0;
    let mut converted = false;

    while index < chars.len() {
        let ch = chars[index];
        if !ch.is_ascii_alphabetic() {
            // 分隔符直接跳过：`Kaguya-sama` 与 `Kaguya sama` 应转出同样的结果。
            index += 1;
            continue;
        }

        // 促音：后接相同辅音（`n` 除外，它是拨音）。
        if ch != 'n' && chars.get(index + 1) == Some(&ch) {
            out.push('っ');
            index += 1;
            converted = true;
            continue;
        }

        // 拨音：`n` 后面不是元音或 y 时读作 ん。
        if ch == 'n' {
            let next = chars.get(index + 1).copied();
            let vowel_follows = matches!(next, Some('a' | 'i' | 'u' | 'e' | 'o' | 'y'));
            if !vowel_follows {
                out.push('ん');
                index += 1;
                converted = true;
                continue;
            }
        }

        // 最长匹配：先试三字母（拗音），再两字母，最后一字母。
        let mut consumed = 0usize;
        for len in (1..=3).rev() {
            if index + len > chars.len() {
                continue;
            }
            let slice: String = chars[index..index + len].iter().collect();
            if let Some(kana) = lookup_syllable(&slice) {
                out.push_str(kana);
                consumed = len;
                converted = true;
                break;
            }
        }
        index += if consumed == 0 { 1 } else { consumed };
    }

    if converted && !out.is_empty() {
        Some(out)
    } else {
        None
    }
}

fn lookup_syllable(slice: &str) -> Option<&'static str> {
    SYLLABLES
        .iter()
        .find(|(roman, _)| *roman == slice)
        .map(|(_, kana)| *kana)
}

/// 罗马音音节表（平文式）。顺序无关，匹配时按长度从长到短尝试。
const SYLLABLES: &[(&str, &str)] = &[
    ("kya", "きゃ"),
    ("kyu", "きゅ"),
    ("kyo", "きょ"),
    ("sha", "しゃ"),
    ("shu", "しゅ"),
    ("sho", "しょ"),
    ("shi", "し"),
    ("cha", "ちゃ"),
    ("chu", "ちゅ"),
    ("cho", "ちょ"),
    ("chi", "ち"),
    ("tsu", "つ"),
    ("nya", "にゃ"),
    ("nyu", "にゅ"),
    ("nyo", "にょ"),
    ("hya", "ひゃ"),
    ("hyu", "ひゅ"),
    ("hyo", "ひょ"),
    ("mya", "みゃ"),
    ("myu", "みゅ"),
    ("myo", "みょ"),
    ("rya", "りゃ"),
    ("ryu", "りゅ"),
    ("ryo", "りょ"),
    ("gya", "ぎゃ"),
    ("gyu", "ぎゅ"),
    ("gyo", "ぎょ"),
    ("ja", "じゃ"),
    ("ju", "じゅ"),
    ("jo", "じょ"),
    ("bya", "びゃ"),
    ("byu", "びゅ"),
    ("byo", "びょ"),
    ("pya", "ぴゃ"),
    ("pyu", "ぴゅ"),
    ("pyo", "ぴょ"),
    ("ka", "か"),
    ("ki", "き"),
    ("ku", "く"),
    ("ke", "け"),
    ("ko", "こ"),
    ("sa", "さ"),
    ("si", "し"),
    ("su", "す"),
    ("se", "せ"),
    ("so", "そ"),
    ("ta", "た"),
    ("ti", "ち"),
    ("tu", "つ"),
    ("te", "て"),
    ("to", "と"),
    ("na", "な"),
    ("ni", "に"),
    ("nu", "ぬ"),
    ("ne", "ね"),
    ("no", "の"),
    ("ha", "は"),
    ("hi", "ひ"),
    ("fu", "ふ"),
    ("hu", "ふ"),
    ("he", "へ"),
    ("ho", "ほ"),
    ("ma", "ま"),
    ("mi", "み"),
    ("mu", "む"),
    ("me", "め"),
    ("mo", "も"),
    ("ya", "や"),
    ("yu", "ゆ"),
    ("yo", "よ"),
    ("ra", "ら"),
    ("ri", "り"),
    ("ru", "る"),
    ("re", "れ"),
    ("ro", "ろ"),
    ("wa", "わ"),
    ("wo", "を"),
    ("ga", "が"),
    ("gi", "ぎ"),
    ("gu", "ぐ"),
    ("ge", "げ"),
    ("go", "ご"),
    ("za", "ざ"),
    ("zi", "じ"),
    ("zu", "ず"),
    ("ze", "ぜ"),
    ("zo", "ぞ"),
    ("ji", "じ"),
    ("da", "だ"),
    ("di", "ぢ"),
    ("du", "づ"),
    ("de", "で"),
    ("do", "ど"),
    ("ba", "ば"),
    ("bi", "び"),
    ("bu", "ぶ"),
    ("be", "べ"),
    ("bo", "ぼ"),
    ("pa", "ぱ"),
    ("pi", "ぴ"),
    ("pu", "ぷ"),
    ("pe", "ぺ"),
    ("po", "ぽ"),
    ("a", "あ"),
    ("i", "い"),
    ("u", "う"),
    ("e", "え"),
    ("o", "お"),
];

fn push_unique(out: &mut Vec<String>, value: String) {
    let value = value.trim().to_owned();
    if value.is_empty() {
        return;
    }
    if !out.iter().any(|existing| existing == &value) {
        out.push(value);
    }
}

/// 匹配候选。
#[derive(Debug, Clone, PartialEq)]
pub struct MatchCandidate {
    pub hit: SubjectSearchHit,
    pub score: f64,
}

fn score_hit(query: &str, query_season: Option<u32>, hit: &SubjectSearchHit) -> f64 {
    let mut best = 0.0f64;
    for candidate in std::iter::once(Some(hit.name.as_str()))
        .chain(std::iter::once(hit.name_cn.as_deref()))
        .flatten()
    {
        let base = similarity(query, &normalize_title(candidate));
        let adjusted = season_adjust(base, query_season, season_of(candidate));
        if adjusted > best {
            best = adjusted;
        }
    }
    best
}

/// 在搜索结果里选最佳匹配；不达标或存在歧义时返回 None。
pub fn best_match(query: &str, hits: &[SubjectSearchHit]) -> Option<MatchCandidate> {
    let normalized_query = normalize_title(query);
    if normalized_query.is_empty() || hits.is_empty() {
        return None;
    }
    let query_season = season_of(query);

    let mut scored: Vec<MatchCandidate> = hits
        .iter()
        .map(|hit| MatchCandidate {
            hit: hit.clone(),
            score: score_hit(&normalized_query, query_season, hit),
        })
        .collect();
    scored.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let top = &scored[0];
    if top.score < AUTO_BIND_THRESHOLD {
        return None;
    }
    if let Some(second) = scored.get(1) {
        if top.score - second.score < AMBIGUITY_MARGIN {
            return None;
        }
    }
    Some(top.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(id: i64, name: &str, name_cn: Option<&str>) -> SubjectSearchHit {
        SubjectSearchHit {
            id,
            name: name.into(),
            name_cn: name_cn.map(str::to_owned),
            score: None,
        }
    }

    #[test]
    fn normalize_strips_group_noise_and_season() {
        assert_eq!(
            normalize_title("[VCB-Studio] 夏日重现 [01][1080p]"),
            "夏日重现"
        );
        assert_eq!(normalize_title("进击的巨人 第二季"), "进击的巨人");
        assert_eq!(normalize_title("Attack on Titan Season 2"), "attackontitan");
        assert_eq!(
            normalize_title("辉夜大小姐想让我告白（第一季）"),
            "辉夜大小姐想让我告白"
        );
    }

    #[test]
    fn season_is_extracted() {
        assert_eq!(season_of("进击的巨人 第二季"), Some(2));
        assert_eq!(season_of("进击的巨人 第12季"), Some(12));
        assert_eq!(season_of("Attack on Titan Season 3"), Some(3));
        assert_eq!(season_of("Show S2"), Some(2));
        assert_eq!(season_of("夏日重现"), None);
    }

    #[test]
    fn normalize_converts_traditional_and_strips_release_junk() {
        assert_eq!(normalize_title("無限煉姦"), normalize_title("无限炼奸"));
        assert_eq!(normalize_title("无限炼奸 步兵版"), "无限炼奸");
        assert_eq!(normalize_title("真爱百合精翻汉化"), "真爱百合");
        assert_eq!(normalize_title("妹調教日記"), "妹调教日记");
    }

    #[test]
    fn similarity_prefers_exact_then_containment() {
        assert_eq!(similarity("夏日重现", "夏日重现"), 1.0);
        assert_eq!(similarity("夏日重现", "夏日重现summertimerendering"), 0.85);
        assert!(similarity("夏日重现", "进击的巨人") < 0.3);
    }

    #[test]
    fn best_match_prefers_base_entry_without_season() {
        let hits = vec![
            hit(1, "進撃の巨人", Some("进击的巨人")),
            hit(2, "進撃の巨人 Season 2", Some("进击的巨人 第二季")),
        ];
        let found = best_match("进击的巨人", &hits).expect("应命中正传");
        assert_eq!(found.hit.id, 1);
    }

    #[test]
    fn best_match_picks_matching_season() {
        let hits = vec![
            hit(1, "進撃の巨人", Some("进击的巨人")),
            hit(2, "進撃の巨人 Season 2", Some("进击的巨人 第二季")),
        ];
        let found = best_match("进击的巨人 第二季", &hits).expect("应命中第二季");
        assert_eq!(found.hit.id, 2);
    }

    #[test]
    fn season_one_is_equivalent_to_no_season() {
        let hits = vec![hit(1, "進撃の巨人", Some("进击的巨人"))];
        let found = best_match("进击的巨人 第一季", &hits).expect("第一季应等于正传");
        assert_eq!(found.hit.id, 1);
    }

    #[test]
    fn search_keywords_produces_variants() {
        let keys = search_keywords("[VCB-Studio] 夏日重现 - 第01话 [1080p]");
        assert!(keys.iter().any(|key| key == "夏日重现"));
        assert!(keys.len() <= 4);
        assert!(search_keywords("   ").is_empty());
    }

    #[test]
    fn romaji_titles_get_a_kana_keyword() {
        // 内置对照表命中时，日文原名排第一，且不再追加假名（多一次请求没有意义）。
        let keys = search_keywords("Kaguya-sama wa Kokurasetai");
        assert_eq!(
            keys.first().map(String::as_str),
            Some("かぐや様は告らせたい ~天才たちの恋愛頭脳戦~")
        );
        assert!(
            !keys.iter().any(|key| key == "かぐやさまわこくらせたい"),
            "有日文原名时不应再生成假名：{keys:?}"
        );

        // 中文标题不该被误判成罗马音，也不查表。
        let keys = search_keywords("夏日重现");
        assert_eq!(keys.len(), 1);

        // 表里没有的生造词退回假名音译。
        let keys = search_keywords("Zzzquux Vex");
        assert!(
            keys.iter()
                .any(|key| key.contains('ぁ') || key.contains('ず') || key.len() < 12),
            "无对照时应尝试假名：{keys:?}"
        );

        assert_eq!(romaji_to_hiragana("kaguya").as_deref(), Some("かぐや"));
        assert_eq!(romaji_to_hiragana("sama").as_deref(), Some("さま"));
        // 促音与拨音。
        assert_eq!(romaji_to_hiragana("bocchi").as_deref(), Some("ぼっち"));
        assert_eq!(romaji_to_hiragana("shinbun").as_deref(), Some("しんぶん"));
    }

    #[test]
    fn best_match_returns_none_when_ambiguous() {
        let hits = vec![
            hit(1, "夏日重现", Some("夏日重现")),
            hit(2, "夏日重现", Some("夏日重现")),
        ];
        assert!(best_match("夏日重现", &hits).is_none());
    }

    #[test]
    fn best_match_returns_none_when_weak() {
        let hits = vec![hit(1, "完全不相干的作品", Some("别的名字"))];
        assert!(best_match("夏日重现", &hits).is_none());
    }

    #[test]
    fn best_match_handles_empty() {
        assert!(best_match("", &[hit(1, "x", None)]).is_none());
        assert!(best_match("夏日重现", &[]).is_none());
        assert!(best_match("!!!", &[hit(1, "x", None)]).is_none());
    }
}
