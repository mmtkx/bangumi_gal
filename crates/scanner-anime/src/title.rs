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
    junk: Regex,
    year: Regex,
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
            junk: build(
                r"(?i)(?:\b(?:1080p|720p|2160p|480p|4k|8k|8bit|10bit|hi10p|hevc|avc|x264|x265|h264|h265|av1|vp9|aac|flac|opus|ac3|eac3|dts|bdrip|bluray|bd|web-?dl|webrip|hdtv|dvdrip|remux|hdr10|hdr|sdr|chs|cht|gb|big5|jpn|eng|dual|multi|sub|mkv|mp4|avi)\b|简繁|简体|繁体|中字|内嵌|外挂|字幕|合集|无修|完结|招募|生肉|熟肉|[★☆♪])",
            )?,
            year: build(r"(?i)(?:^|[\s._\-])(?:19|20)\d{2}(?:[\s._\-]|$)")?,
            separators: build(r"[._\[\]【】（）()/\\]+")?,
            episodes: vec![
                build(r"(?i)s\d{1,2}\s*e(\d{1,4})")?,
                build(r"(?i)(?:^|[\s\[\(【._\-])(?:ep|e)\s*\.?\s*(\d{1,4})(?:[\s\]\)】._\-]|$)")?,
                build(r"第\s*(\d{1,4})\s*[话話集]")?,
                build(r"(?:^|[\s\[\(【._\-]+)(\d{1,4})\s*(?:[vV]\d)?\s*(?:[\s\]\)】._\-]|$)")?,
            ],
        })
    }

    /// 从文件夹名提取用于展示的标题。清洗后为空时退回原始名称，避免条目显示为空白。
    pub fn parse_title(&self, folder_name: &str) -> String {
        let mut text = folder_name.to_string();
        text = self.bracket.replace_all(&text, " ").to_string();
        text = self.season_marks.replace_all(&text, " ").to_string();
        text = self.season_cn.replace_all(&text, " ").to_string();
        text = self.junk.replace_all(&text, " ").to_string();
        text = self.year.replace_all(&text, " ").to_string();
        text = self.separators.replace_all(&text, " ").to_string();

        let collapsed = collapse_whitespace(&text);
        let trimmed = collapsed
            .trim_matches(|c: char| c.is_whitespace() || "-–—_·:：|".contains(c))
            .to_string();

        if trimmed.is_empty() {
            folder_name.trim().to_string()
        } else {
            trimmed
        }
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
        for pattern in &self.episodes {
            let Some(captures) = pattern.captures(file_name) else {
                continue;
            };
            let Some(raw) = captures.get(1) else {
                continue;
            };
            let Ok(value) = raw.as_str().parse::<u32>() else {
                continue;
            };
            if (1..=999).contains(&value) && !AMBIGUOUS_NUMBERS.contains(&value) {
                return Some(value);
            }
        }
        None
    }
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
