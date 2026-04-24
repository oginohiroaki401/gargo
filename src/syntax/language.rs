use tree_sitter::Language;
use tree_sitter_language::LanguageFn;

use crate::syntax::indent;

const MARKDOWN_TAGS_QUERY: &str = r#"
(atx_heading
  (inline) @name) @definition.section

(setext_heading
  (paragraph) @name) @definition.section
"#;

pub struct LanguageDef {
    pub name: &'static str,
    pub language_fn: LanguageFn,
    pub highlight_query: &'static str,
    pub indent_query: Option<&'static str>,
    pub tags_query: Option<&'static str>,
    pub extensions: &'static [&'static str],
}

pub struct LanguageRegistry {
    languages: Vec<LanguageDef>,
}

impl Default for LanguageRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LanguageRegistry {
    pub fn new() -> Self {
        let languages = vec![
            LanguageDef {
                name: "Rust",
                language_fn: tree_sitter_rust::LANGUAGE,
                highlight_query: tree_sitter_rust::HIGHLIGHTS_QUERY,
                indent_query: Some(indent::RUST_INDENT_QUERY),
                tags_query: Some(tree_sitter_rust::TAGS_QUERY),
                extensions: &["rs"],
            },
            LanguageDef {
                name: "JavaScript",
                language_fn: tree_sitter_javascript::LANGUAGE,
                highlight_query: tree_sitter_javascript::HIGHLIGHT_QUERY,
                indent_query: Some(indent::JAVASCRIPT_INDENT_QUERY),
                tags_query: Some(tree_sitter_javascript::TAGS_QUERY),
                extensions: &["js", "mjs", "cjs", "jsx"],
            },
            LanguageDef {
                name: "TypeScript",
                language_fn: tree_sitter_typescript::LANGUAGE_TYPESCRIPT,
                highlight_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
                indent_query: Some(indent::TYPESCRIPT_INDENT_QUERY),
                tags_query: Some(tree_sitter_typescript::TAGS_QUERY),
                extensions: &["ts", "mts", "cts"],
            },
            LanguageDef {
                name: "TSX",
                language_fn: tree_sitter_typescript::LANGUAGE_TSX,
                highlight_query: tree_sitter_typescript::HIGHLIGHTS_QUERY,
                indent_query: Some(indent::TYPESCRIPT_INDENT_QUERY),
                tags_query: Some(tree_sitter_typescript::TAGS_QUERY),
                extensions: &["tsx"],
            },
            LanguageDef {
                name: "Python",
                language_fn: tree_sitter_python::LANGUAGE,
                highlight_query: tree_sitter_python::HIGHLIGHTS_QUERY,
                indent_query: None,
                tags_query: Some(tree_sitter_python::TAGS_QUERY),
                extensions: &["py", "pyi"],
            },
            LanguageDef {
                name: "Go",
                language_fn: tree_sitter_go::LANGUAGE,
                highlight_query: tree_sitter_go::HIGHLIGHTS_QUERY,
                indent_query: Some(indent::GO_INDENT_QUERY),
                tags_query: Some(tree_sitter_go::TAGS_QUERY),
                extensions: &["go"],
            },
            LanguageDef {
                name: "C",
                language_fn: tree_sitter_c::LANGUAGE,
                highlight_query: tree_sitter_c::HIGHLIGHT_QUERY,
                indent_query: Some(indent::C_INDENT_QUERY),
                tags_query: Some(tree_sitter_c::TAGS_QUERY),
                extensions: &["c", "h"],
            },
            LanguageDef {
                name: "JSON",
                language_fn: tree_sitter_json::LANGUAGE,
                highlight_query: tree_sitter_json::HIGHLIGHTS_QUERY,
                indent_query: Some(indent::JSON_INDENT_QUERY),
                tags_query: None,
                extensions: &["json"],
            },
            LanguageDef {
                name: "TOML",
                language_fn: tree_sitter_toml_ng::LANGUAGE,
                highlight_query: tree_sitter_toml_ng::HIGHLIGHTS_QUERY,
                indent_query: None,
                tags_query: None,
                extensions: &["toml"],
            },
            LanguageDef {
                name: "Diff",
                language_fn: tree_sitter_diff::LANGUAGE,
                highlight_query: tree_sitter_diff::HIGHLIGHTS_QUERY,
                indent_query: None,
                tags_query: None,
                extensions: &["diff", "patch"],
            },
            LanguageDef {
                name: "Java",
                language_fn: tree_sitter_java::LANGUAGE,
                highlight_query: tree_sitter_java::HIGHLIGHTS_QUERY,
                indent_query: None,
                tags_query: Some(tree_sitter_java::TAGS_QUERY),
                extensions: &["java"],
            },
            LanguageDef {
                name: "PHP",
                language_fn: tree_sitter_php::LANGUAGE_PHP,
                highlight_query: tree_sitter_php::HIGHLIGHTS_QUERY,
                indent_query: None,
                tags_query: Some(tree_sitter_php::TAGS_QUERY),
                extensions: &["php", "phtml"],
            },
            LanguageDef {
                name: "HTML",
                language_fn: tree_sitter_html::LANGUAGE,
                highlight_query: tree_sitter_html::HIGHLIGHTS_QUERY,
                indent_query: None,
                tags_query: None,
                extensions: &["html", "htm"],
            },
            LanguageDef {
                name: "Markdown",
                language_fn: tree_sitter_md::LANGUAGE,
                highlight_query: tree_sitter_md::HIGHLIGHT_QUERY_BLOCK,
                indent_query: None,
                tags_query: Some(MARKDOWN_TAGS_QUERY),
                extensions: &["md", "markdown"],
            },
        ];
        Self { languages }
    }

    pub fn detect_by_extension(&self, path: &str) -> Option<&LanguageDef> {
        let ext = path.rsplit('.').next()?;
        self.languages
            .iter()
            .find(|lang| lang.extensions.contains(&ext))
    }

    pub fn ts_language(lang_fn: LanguageFn) -> Language {
        Language::new(lang_fn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_rust() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("src/main.rs").unwrap();
        assert_eq!(lang.name, "Rust");
    }

    #[test]
    fn detect_typescript() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("app.tsx").unwrap();
        assert_eq!(lang.name, "TSX");
        let lang = reg.detect_by_extension("index.ts").unwrap();
        assert_eq!(lang.name, "TypeScript");
    }

    #[test]
    fn detect_unknown() {
        let reg = LanguageRegistry::new();
        assert!(reg.detect_by_extension("README.txt").is_none());
    }

    #[test]
    fn detect_toml() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("Cargo.toml").unwrap();
        assert_eq!(lang.name, "TOML");
    }

    #[test]
    fn detect_java() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("Main.java").unwrap();
        assert_eq!(lang.name, "Java");
    }

    #[test]
    fn detect_php() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("index.php").unwrap();
        assert_eq!(lang.name, "PHP");
        let lang = reg.detect_by_extension("page.phtml").unwrap();
        assert_eq!(lang.name, "PHP");
    }

    #[test]
    fn detect_html() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("index.html").unwrap();
        assert_eq!(lang.name, "HTML");
        let lang = reg.detect_by_extension("page.htm").unwrap();
        assert_eq!(lang.name, "HTML");
    }

    #[test]
    fn detect_javascript() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("app.js").unwrap();
        assert_eq!(lang.name, "JavaScript");
    }

    #[test]
    fn detect_diff() {
        let reg = LanguageRegistry::new();
        let lang = reg.detect_by_extension("changes.diff").unwrap();
        assert_eq!(lang.name, "Diff");
        let lang = reg.detect_by_extension("changes.patch").unwrap();
        assert_eq!(lang.name, "Diff");
    }
}
