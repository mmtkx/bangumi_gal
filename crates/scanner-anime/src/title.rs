use library_core::{CoreError, Result};
use regex::Regex;

/// 这些数字极少是集号，出现时更可能是分辨率，解析集号时排除。
const AMBIGUOUS_NUMBERS: &[u32] = &[480, 720];

fn build(pattern: &str) -> Result<Regex> {
    Regex::new(pattern).map_err(|err| CoreError::Regex(err.to_string()))
}

/// 文件名/文件夹名解析器（ADR-004：正则规则引擎，语义解析留给未来插件）。
pub struct TitleParser {
    bracket: Regex,
    season_marks: Regex,
    season_cn: Regex,
    season_latin: Regex,
    season_cn_numbered: Regex,
    junk: Regex,
    year: Regex,
    /// 版本标记（`Lv.2`、`Lv2`、`Ver 2`、`v2`）：里面的数字不是集号。
    version_mark: Regex,
    /// 带捕获组的方括号，用于「纯括号名」的标题兜底。
    bracket_inner: Regex,
    separators: Regex,
    episodes: Vec<Regex>,
}

impl TitleParser {
    pub fn new() -> Result<Self> {
        Ok(Self {
            bracket: build(r"[\[【（(][^\]】）)]*[\]】）)]")?,
            // Rust regex 不支持前瞻，故把可选集号一并吃掉；否则 `Show S02E05` 会残留集号。
            season_marks: build(
                r"(?i)(?:^|[\s._\-])(?:s|season)\s*\d{1,2}(?:\s*e\s*\d{1,4})?(?:[\s._\-]|$)",
            )?,
            season_cn: build(r"第\s*[一二三四五六七八九十百零两\d]{1,3}\s*[季期部]")?,
            // 带捕获组的同款规则：清理标题用的是不带组的版本，这里只为拿季号。
            season_latin: build(
                r"(?i)(?:^|[\s._\-\[\(【])(?:s|season)\s*0?(\d{1,2})(?:[\s._\-e\]\)】]|$)",
            )?,
            season_cn_numbered: build(r"第\s*([一二三四五六七八九十百零两\d]{1,3})\s*[季期部]")?,
            junk: build(
                r"(?i)(?:\b(?:1080p|720p|2160p|480p|4k|8k|8bit|10bit|hi10p|hevc|avc|x264|x265|h264|h265|av1|vp9|aac|flac|opus|ac3|eac3|dts|bdrip|bluray|bd|web-?dl|webrip|hdtv|dvdrip|remux|hdr10|hdr|sdr|chs|cht|gb|big5|jpn|eng|dual|multi|sub|mkv|mp4|avi)\b|简繁|简体|繁体|中字|内嵌|外挂|字幕|合集|无修|完结|招募|生肉|熟肉|[★☆♪])",
            )?,
            year: build(r"(?i)(?:^|[\s._\-])(?:19|20)\d{2}(?:[\s._\-]|$)")?,
            // 不要求左侧边界：`学生会的一己之见Lv.2` 的版本标记紧跟在汉字后面。
            // 尾部必须是非字母数字或结束，避免吃掉 `Dev2` 之外的普通单词。
            version_mark: build(r"(?i)(?:lv|ver|version|v)\s*\.?\s*\d{1,3}(?:[^0-9A-Za-z]|$)")?,
            bracket_inner: build(r"[\[【（(]([^\]】）)]*)[\]】）)]")?,
            separators: build(r"[._\[\]【】（）()/\\]+")?,
            episodes: vec![
                build(r"(?i)s\d{1,2}\s*e(\d{1,4})")?,
                build(r"(?i)(?:^|[\s\[\(【._\-])(?:ep|e)\s*\.?\s*(\d{1,4})(?:[\s\]\)】._\-]|$)")?,
                build(r"第\s*(\d{1,4})\s*[话話集]")?,
                build(r"(?:^|[\s\[\(【._\-]+)(\d{1,4})\s*(?:[vV]\d)?\s*(?:[\s\]\)】._\-]|$)")?,
            ],
        })
    }

    /// 从文件夹名提取用于展示的标题。
    ///
    /// 清洗后为空时（名字整体由方括号段构成，例如 `[组][作品][1080P]…`），
    /// 改为逐个清洗括号内的文本、挑最像作品名的一段——否则条目会直接显示成
    /// 一整串 release 名。仍然挑不出来才退回原始名称。
    pub fn parse_title(&self, folder_name: &str) -> String {
        let cleaned = self.clean_title(folder_name);
        if !cleaned.is_empty() {
            return cleaned;
        }
        let mut best: Option<(usize, String)> = None;
        for inner in self.bracket_inner.captures_iter(folder_name) {
            let Some(text) = inner.get(1) else {
                continue;
            };
            let candidate = self.clean_title(text.as_str());
            if candidate.is_empty() {
                continue;
            }
            let score = title_score(&candidate);
            if best.as_ref().is_none_or(|(current, _)| score > *current) {
                best = Some((score, candidate));
            }
        }
        match best {
            Some((_, value)) => value,
            None => folder_name.trim().to_string(),
        }
    }

    /// 标题清洗管线本身，不做任何兜底。
    fn clean_title(&self, raw: &str) -> String {
        let mut text = raw.to_string();
        text = self.bracket.replace_all(&text, " ").to_string();
        text = self.season_marks.replace_all(&text, " ").to_string();
        text = self.season_cn.replace_all(&text, " ").to_string();
        text = self.version_mark.replace_all(&text, " ").to_string();
        text = self.junk.replace_all(&text, " ").to_string();
        text = self.year.replace_all(&text, " ").to_string();
        text = self.separators.replace_all(&text, " ").to_string();

        collapse_whitespace(&text)
            .trim_matches(|c: char| c.is_whitespace() || "-–—_·:：|".contains(c))
            .to_string()
    }

    /// 从视频文件名解析「系列标题」。
    ///
    /// 与 `parse_title` 的区别是额外去掉末尾集号，使 `Show - 01` 与 `Show - 02`
    /// 归到同一组；根目录松散文件就是按这个结果分组的。
    pub fn parse_series_title(&self, file_name: &str) -> String {
        strip_trailing_episode(&self.parse_title(file_name))
    }

    /// 从视频文件名解析集号。解析不到返回 None，UI 退化为展示文件名。
    pub fn parse_episode(&self, file_name: &str) -> Option<u32> {
        // `[DBD-Raws][Seitokai no Ichizon Lv.2][03]…` 里 `Lv.2` 的点会被当成集号分隔符，
        // 而它比真正的 `[03]` 更靠左，所以必须先抹掉版本标记再匹配，否则整部番都会取到 2。
        let cleaned = self.version_mark.replace_all(file_name, " ");
        for pattern in &self.episodes {
            let Some(captures) = pattern.captures(&cleaned) else {
                continue;
            };
            let Some(raw) = captures.get(1) else {
                continue;
            };
            let Ok(value) = raw.as_str().parse::<u32>() else {
                continue;
            };
            // 0 是合法的：不少压制组从 `[00]` 开始编号（由探测阶段整体 +1）。
            if (0..=999).contains(&value) && !AMBIGUOUS_NUMBERS.contains(&value) {
                return Some(value);
            }
        }
        None
    }

    /// 从文本里解析季号（`S02E05`、`Season 2`、`第二季`）。解析不到返回 None。
    pub fn parse_season(&self, text: &str) -> Option<u32> {
        if let Some(captures) = self.season_latin.captures(text) {
            if let Some(value) = captures
                .get(1)
                .and_then(|raw| raw.as_str().parse::<u32>().ok())
                .filter(|value| (1..=99).contains(value))
            {
                return Some(value);
            }
        }
        let captures = self.season_cn_numbered.captures(text)?;
        parse_cn_number(captures.get(1)?.as_str()).filter(|value| (1..=99).contains(value))
    }

    /// 相对路径里的季号：文件名优先，其次从最深的目录往上找。
    ///
    /// `Show/Season 2/05.mkv` 的季号只在目录上，`Show/S02E05.mkv` 只在文件名上，
    /// 所以必须两层都看；文件名更具体，优先采信。
    pub fn parse_season_in_path(&self, relative: &str) -> Option<u32> {
        let mut parts: Vec<&str> = relative.split('/').collect();
        let file_name = parts.pop()?;
        if let Some(season) = self.parse_season(file_name) {
            return Some(season);
        }
        parts.iter().rev().find_map(|part| self.parse_season(part))
    }
}

/// 给候选标题打分：汉字最有辨识度，拉丁字母次之，纯数字/符号最低。
///
/// `[组][学生会的一己之见Lv.2][00-10TV全集+特典映像][1080P]…` 这类名字里，
/// 「学生会的一己之见」应当压过「00-10TV全集+特典映像」与「DBD-Raws」。
fn title_score(text: &str) -> usize {
    let mut score = 0;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            // 数字不计分：`00-10TV全集+特典映像` 里的集数范围会把分值顶上去。
            continue;
        }
        if ch.is_ascii_alphabetic() {
            score += 1;
        } else if ch.is_alphanumeric() {
            // 汉字/假名等：权重更高，否则长串英文压制组名会赢。
            score += 3;
        }
    }
    score
}

/// 把「一 / 十二 / 二十三」这类不超过两位的中文数字转成数值。
fn parse_cn_number(text: &str) -> Option<u32> {
    if let Ok(value) = text.parse::<u32>() {
        return Some(value);
    }
    let mut total = 0_u32;
    let mut pending = 0_u32;
    for ch in text.chars() {
        match ch {
            '零' => pending = 0,
            '一' => pending = 1,
            '二' | '两' => pending = 2,
            '三' => pending = 3,
            '四' => pending = 4,
            '五' => pending = 5,
            '六' => pending = 6,
            '七' => pending = 7,
            '八' => pending = 8,
            '九' => pending = 9,
            '十' => {
                total += if pending == 0 { 10 } else { pending * 10 };
                pending = 0;
            }
            _ => return None,
        }
    }
    Some(total + pending)
}

fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 去掉末尾的集号片段。
///
/// 只处理「最后一个分隔片段是纯数字或 SxxEyy」的情况，所以 `86`、`5-toubun`
/// 这类标题里的数字不会被误删；去掉后为空时保留原值。
fn strip_trailing_episode(text: &str) -> String {
    let trimmed = text.trim();
    let Some(index) = trimmed.rfind([' ', '-', '_', '.']) else {
        return trimmed.to_string();
    };
    let (head, tail) = trimmed.split_at(index);
    let trim_junk = |value: &str| {
        value
            .trim_matches(|c: char| c.is_whitespace() || "-_.".contains(c))
            .to_string()
    };
    let token = trim_junk(tail);
    let head_clean = trim_junk(head);
    if head_clean.is_empty() || token.is_empty() {
        return trimmed.to_string();
    }

    let all_digits = token.chars().all(|c| c.is_ascii_digit());
    let is_episode = (all_digits && (1..=4).contains(&token.len())) || is_season_episode(&token);
    if is_episode {
        head_clean
    } else {
        trimmed.to_string()
    }
}

/// `S02E05` / `s2e5` 形式的集号标记。
fn is_season_episode(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    let Some(rest) = lower.strip_prefix('s') else {
        return false;
    };
    let Some((season, episode)) = rest.split_once('e') else {
        return false;
    };
    !season.is_empty()
        && !episode.is_empty()
        && season.chars().all(|c| c.is_ascii_digit())
        && episode.chars().all(|c| c.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parser() -> TitleParser {
        TitleParser::new().expect("parser")
    }

    #[test]
    fn episode_ignores_version_marker() {
        let parser = parser();
        // `Lv.2` 里的点会被当成集号分隔符，且比真正的 [03] 更靠左：
        // 不先抹掉版本标记，整部番都会取到第 2 集、共用一行进度。
        assert_eq!(
            parser.parse_episode("[DBD-Raws][Seitokai no Ichizon Lv.2][03][1080P].mkv"),
            Some(3)
        );
        assert_eq!(
            parser.parse_episode("[Group][Show Lv.2][10][BDRip].mkv"),
            Some(10)
        );
        // 从 00 起编的压制组：0 要能解析出来，由探测阶段整体 +1 变成第 1 集。
        assert_eq!(
            parser.parse_episode("[DBD-Raws][Seitokai no Ichizon Lv.2][00][1080P].mkv"),
            Some(0)
        );
        // 常规点分隔命名不能被版本标记规则误伤。
        assert_eq!(parser.parse_episode("Show.07.1080p.mkv"), Some(7));
        assert_eq!(parser.parse_episode("Show - 12 [1080p].mkv"), Some(12));
    }

    #[test]
    fn title_falls_back_to_bracket_content() {
        let parser = parser();
        // 名字整体由方括号段构成时，清洗结果为空，须从括号内容里挑作品名，
        // 否则条目会显示成一整串 release 名。
        assert_eq!(
            parser.parse_title(
                "[DBD-Raws][学生会的一己之见Lv.2][00-10TV全集+特典映像][1080P][BDRip][HEVC-10bit][FLAC][MKV]"
            ),
            "学生会的一己之见"
        );
        // 括号外有正常文本时不受影响。
        assert_eq!(
            parser.parse_title("[VCB-Studio] Kimi no Na wa [Ma10p_1080p]"),
            "Kimi no Na wa"
        );
    }

    #[test]
    fn strips_group_bracket_and_release_noise() {
        let parser = parser();
        assert_eq!(
            parser.parse_title("[Nekomoe kissaten] Sousou no Frieren [1080p][CHS]"),
            "Sousou no Frieren"
        );
        assert_eq!(
            parser.parse_title("【喵萌奶茶屋】葬送的芙莉莲[01][1080p][简繁日内嵌]"),
            "葬送的芙莉莲"
        );
    }

    #[test]
    fn strips_season_markers() {
        let parser = parser();
        assert_eq!(
            parser.parse_title("Mushoku Tensei S02 [1080p]"),
            "Mushoku Tensei"
        );
        assert_eq!(parser.parse_title("进击的巨人 第三季"), "进击的巨人");
    }

    #[test]
    fn falls_back_to_raw_name_when_everything_is_cleaned_away() {
        let parser = parser();
        assert_eq!(parser.parse_title("[1080p]"), "[1080p]");
    }

    #[test]
    fn parses_episode_from_common_patterns() {
        let parser = parser();
        let cases = [
            ("Show S02E05 [1080p].mkv", Some(5)),
            ("[Group] Show - 01 [1080p].mkv", Some(1)),
            ("Show EP12.mkv", Some(12)),
            ("Show 第07话.mkv", Some(7)),
            ("Show 08v2.mkv", Some(8)),
        ];
        for (name, expected) in cases {
            assert_eq!(parser.parse_episode(name), expected, "{name}");
        }
    }

    #[test]
    fn parses_season_from_name_and_directory() {
        let parser = parser();
        assert_eq!(parser.parse_season("Show S02E05.mkv"), Some(2));
        assert_eq!(parser.parse_season("Show Season 3 - 01.mkv"), Some(3));
        assert_eq!(parser.parse_season("进击的巨人 第三季"), Some(3));
        assert_eq!(parser.parse_season("Show 第十二季"), Some(12));
        assert_eq!(parser.parse_season("Show 01.mkv"), None);
        // 季号也可能只出现在目录上。
        assert_eq!(parser.parse_season_in_path("Show/Season 2/05.mkv"), Some(2));
        assert_eq!(parser.parse_season_in_path("Show/S02E05.mkv"), Some(2));
        assert_eq!(parser.parse_season_in_path("Show/05.mkv"), None);
    }

    #[test]
    fn rejects_resolution_as_episode() {
        let parser = parser();
        assert_eq!(parser.parse_episode("Show [1080p].mkv"), None);
        assert_eq!(parser.parse_episode("Show 720p.mkv"), None);
    }

    #[test]
    fn series_title_drops_trailing_episode_only() {
        let parser = parser();
        // 同一部番的不同集必须归到同一个系列标题。
        assert_eq!(
            parser.parse_series_title("Kaguya-sama wa Kokurasetai - 01"),
            parser.parse_series_title("Kaguya-sama wa Kokurasetai - 12")
        );
        assert_eq!(
            parser.parse_series_title("Kaguya-sama wa Kokurasetai - 01"),
            "Kaguya-sama wa Kokurasetai"
        );
        // 标题本身是数字时不能被当成集号删掉。
        assert_eq!(parser.parse_series_title("86 - 01"), "86");
        assert_eq!(parser.parse_series_title("86"), "86");
        // 纯数字文件名不能退化成空串。
        assert_eq!(parser.parse_series_title("01"), "01");
    }
}
