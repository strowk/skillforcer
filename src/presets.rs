pub struct Preset {
    pub name: &'static str,
    pub content: Option<&'static str>,
    pub path: &'static [&'static str],
}

// Heuristic comment detector: line/block comment leaders and trailing `#`.
const CODE_COMMENTS: &str = r"(//|/\*|\*/|<!--|(^|\s)#|(^|\s);;)";
const MD_HEADINGS: &str = r"(?m)^#{1,6}\s";

static PRESETS: &[Preset] = &[
    Preset {
        name: "code-comments",
        content: Some(CODE_COMMENTS),
        path: &[
            "**/*.rs",
            "**/*.ts",
            "**/*.js",
            "**/*.go",
            "**/*.py",
            "**/*.c",
            "**/*.h",
            "**/*.cpp",
            "**/*.java",
        ],
    },
    Preset {
        name: "markdown-headings",
        content: Some(MD_HEADINGS),
        path: &["**/*.md"],
    },
];

pub fn builtin() -> &'static [Preset] {
    PRESETS
}

pub fn get(name: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;

    #[test]
    fn code_comments_preset_exists_and_matches() {
        let p = get("code-comments").unwrap();
        let re = Regex::new(p.content.unwrap()).unwrap();
        assert!(re.is_match("// hello"));
        assert!(re.is_match("/* block */"));
        assert!(re.is_match("value = 1  # trailing"));
        assert!(!re.is_match("let x = 1;"));
    }

    #[test]
    fn markdown_headings_preset_exists() {
        let p = get("markdown-headings").unwrap();
        let re = Regex::new(p.content.unwrap()).unwrap();
        assert!(re.is_match("# Title"));
        assert!(!re.is_match("no heading here"));
    }
}
