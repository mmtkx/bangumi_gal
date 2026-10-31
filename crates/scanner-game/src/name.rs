use library_core::{CoreError, Result};
use regex::Regex;

/// exe 常被放在这些目录里，它们不是游戏名（`Hollow Knight/bin/Game.exe`）。
/// 命中时标题向上回溯到第一个非容器目录名。
const GENERIC_DIR_NAMES: &[&str] = &[
    "bin",
    "binaries",
    "binary",
    "x64",
    "x86",
    "win64",
    "win32",
    "game",
    "gamefiles",
    "build",
    "release",
    "debug",
    "app",
    "application",
    "data",
    "launcher",
    "support",
];

pub fn is_generic_dir(name: &str) -> bool {
    GENERIC_DIR_NAMES
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// 游戏文件夹名清洗规则。
///
/// 不复用 scanner-anime 的实现：功能 crate 之间禁止相互依赖（plan.md 3.2），
/// 且游戏目录的噪声词（破解组、版本标记）与动画完全不同。
pub struct NameCleaner {
    bracket: Regex,
    junk: Regex,
    separators: Regex,
}

impl NameCleaner {
    pub fn new() -> Result<Self> {
        Ok(Self {
            bracket: Regex::new(r"[\[【（(][^\]】）)]*[\]】）)]")
                .map_err(|err| CoreError::Regex(err.to_string()))?,
            junk: Regex::new(
                r"(?i)(?:\b(?:repack|fitgirl|codex|plaza|skidrow|gog|rune|tenoke|goldberg|elamigos|multi\d*|repack-?v?\d*)\b|免安装|绿色版|破解版|汉化版|官方中文|完整版|简体中文版|繁体中文版|纯净版)",
            )
            .map_err(|err| CoreError::Regex(err.to_string()))?,
            // 不把 `.` 当分隔符：版本号 `v1.5` 会被拆成 `v1 5`，损失信息。
            separators: Regex::new(r"[_\[\]【】（）()/\\]+")
                .map_err(|err| CoreError::Regex(err.to_string()))?,
        })
    }

    pub fn clean(&self, folder_name: &str) -> String {
        let mut text = folder_name.to_string();
        text = self.bracket.replace_all(&text, " ").to_string();
        text = self.junk.replace_all(&text, " ").to_string();
        text = self.separators.replace_all(&text, " ").to_string();

        let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
        let trimmed = collapsed
            .trim_matches(|c: char| c.is_whitespace() || "-–—_·:：|".contains(c))
            .to_string();

        if trimmed.is_empty() {
            folder_name.trim().to_string()
        } else {
            trimmed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_brackets_and_release_tags() {
        let cleaner = NameCleaner::new().expect("cleaner");
        assert_eq!(
            cleaner.clean("[FitGirl Repack] Hollow Knight"),
            "Hollow Knight"
        );
        assert_eq!(cleaner.clean("Hollow_Knight_v1.5"), "Hollow Knight v1.5");
        assert_eq!(cleaner.clean("塞尔达传说 汉化版"), "塞尔达传说");
    }

    #[test]
    fn keeps_numbers_in_titles() {
        let cleaner = NameCleaner::new().expect("cleaner");
        assert_eq!(cleaner.clean("FIFA 23"), "FIFA 23");
    }

    #[test]
    fn detects_generic_container_dirs() {
        assert!(is_generic_dir("bin"));
        assert!(is_generic_dir("Binaries"));
        assert!(!is_generic_dir("Hollow Knight"));
    }
}
