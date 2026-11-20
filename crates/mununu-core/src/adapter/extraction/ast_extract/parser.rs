//! Tree-sitter parser initialization and language dispatch.
//!
//! Provides a unified interface for parsing TypeScript, Python, and Rust
//! source files into tree-sitter syntax trees. Language is detected from
//! file extension or explicit config.

use tree_sitter::{Language, Parser, Tree};

/// Supported source languages for AST extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceLanguage {
    TypeScript,
    Python,
    Rust,
}

impl SourceLanguage {
    /// Detect language from file extension.
    pub fn from_extension(path: &str) -> Option<Self> {
        let ext = path.rsplit('.').next()?;
        match ext {
            "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => Some(Self::TypeScript),
            "py" | "pyi" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            _ => None,
        }
    }

    /// Every name [`Self::from_name`] accepts — the candidates an "Unknown language" refusal
    /// lists (mununu#1121). Kept beside the match; the test holds the two equal.
    pub const NAMES: &'static [&'static str] = &[
        "typescript",
        "ts",
        "javascript",
        "js",
        "python",
        "py",
        "rust",
        "rs",
    ];

    /// Detect language from explicit name string.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "typescript" | "ts" | "javascript" | "js" => Some(Self::TypeScript),
            "python" | "py" => Some(Self::Python),
            "rust" | "rs" => Some(Self::Rust),
            _ => None,
        }
    }
}

/// Parsed source file with tree-sitter AST.
pub struct ParsedSource {
    /// The source code text.
    pub source: String,
    /// The tree-sitter syntax tree.
    pub tree: Tree,
    /// Detected language.
    pub language: SourceLanguage,
}

impl ParsedSource {
    /// Get the source text for a tree-sitter node.
    pub fn node_text(&self, node: &tree_sitter::Node) -> &str {
        node.utf8_text(self.source.as_bytes()).unwrap_or("")
    }

    /// Get the source line (1-indexed) for a tree-sitter node.
    pub fn node_line(&self, node: &tree_sitter::Node) -> u32 {
        node.start_position().row as u32 + 1
    }

    /// Get the end line (1-indexed) for a tree-sitter node.
    pub fn node_end_line(&self, node: &tree_sitter::Node) -> u32 {
        node.end_position().row as u32 + 1
    }
}

/// Get the tree-sitter Language for a source language.
fn get_ts_language(lang: SourceLanguage) -> Language {
    match lang {
        SourceLanguage::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        SourceLanguage::Python => tree_sitter_python::LANGUAGE.into(),
        SourceLanguage::Rust => tree_sitter_rust::LANGUAGE.into(),
    }
}

/// Parse a source file into a tree-sitter syntax tree.
pub fn parse_source(source: &str, language: SourceLanguage) -> Result<ParsedSource, String> {
    let ts_language = get_ts_language(language);
    let mut parser = Parser::new();
    parser
        .set_language(&ts_language)
        .map_err(|e| format!("Failed to set tree-sitter language: {e}"))?;

    let tree = parser
        .parse(source, None)
        .ok_or("Tree-sitter parse returned None")?;

    let root = tree.root_node();
    if root.has_error() {
        // Count ERROR nodes to assess severity
        let error_count = count_error_nodes(&root);
        let total_count = count_all_nodes(&root);
        let error_pct = if total_count > 0 {
            (error_count as f64 / total_count as f64) * 100.0
        } else {
            0.0
        };

        if error_pct > 30.0 {
            return Err(format!(
                "Tree-sitter parse has too many errors ({} of {} nodes, {:.0}%). \
                 The source may not be valid {} syntax.",
                error_count,
                total_count,
                error_pct,
                match language {
                    SourceLanguage::TypeScript => "TypeScript",
                    SourceLanguage::Python => "Python",
                    SourceLanguage::Rust => "Rust",
                }
            ));
        }
        // Warn but continue — tree-sitter is error-tolerant
        eprintln!(
            "Warning: tree-sitter parse contains {} error node(s) out of {} ({:.0}%); extraction may be incomplete",
            error_count, total_count, error_pct
        );
    }

    Ok(ParsedSource {
        source: source.to_string(),
        tree,
        language,
    })
}

/// Count ERROR nodes in the tree-sitter parse tree.
fn count_error_nodes(node: &tree_sitter::Node) -> usize {
    let mut count = if node.is_error() || node.is_missing() {
        1
    } else {
        0
    };
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_error_nodes(&child);
    }
    count
}

/// Count all nodes in the tree-sitter parse tree.
fn count_all_nodes(node: &tree_sitter::Node) -> usize {
    let mut count = 1;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        count += count_all_nodes(&child);
    }
    count
}

/// Parse a source file from a path, detecting language from extension.
pub fn parse_file(
    path: &std::path::Path,
    language_override: Option<SourceLanguage>,
) -> Result<ParsedSource, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read '{}': {e}", path.display()))?;

    let language = language_override
        .or_else(|| path.to_str().and_then(SourceLanguage::from_extension))
        .ok_or_else(|| {
            format!(
                "Cannot detect language for '{}'. Use --language or a recognized extension.",
                path.display()
            )
        })?;

    parse_source(&source, language)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_language_from_extension() {
        assert_eq!(
            SourceLanguage::from_extension("server.ts"),
            Some(SourceLanguage::TypeScript)
        );
        assert_eq!(
            SourceLanguage::from_extension("handler.py"),
            Some(SourceLanguage::Python)
        );
        assert_eq!(
            SourceLanguage::from_extension("connection/mod.rs"),
            Some(SourceLanguage::Rust)
        );
        assert_eq!(SourceLanguage::from_extension("readme.md"), None);
    }

    #[test]
    fn detect_language_from_name() {
        assert_eq!(
            SourceLanguage::from_name("typescript"),
            Some(SourceLanguage::TypeScript)
        );
        assert_eq!(
            SourceLanguage::from_name("Python"),
            Some(SourceLanguage::Python)
        );
        assert_eq!(
            SourceLanguage::from_name("rust"),
            Some(SourceLanguage::Rust)
        );
    }

    #[test]
    fn parse_typescript_source() {
        let source = r#"
class Server {
    private _started: boolean = false;
    private _closed: boolean = false;

    start(): void {
        if (this._started) {
            throw new Error('already started');
        }
        this._started = true;
    }

    close(): void {
        if (this._closed) {
            return;
        }
        this._closed = true;
    }
}
"#;
        let parsed = parse_source(source, SourceLanguage::TypeScript).unwrap();
        assert_eq!(parsed.language, SourceLanguage::TypeScript);
        assert!(!parsed.tree.root_node().has_error());
    }

    #[test]
    fn parse_python_source() {
        let source = r#"
class Handler:
    def __init__(self):
        self._active = False
        self._count = 0

    def activate(self):
        if not self._active:
            self._active = True

    def process(self):
        if self._active:
            self._count += 1
"#;
        let parsed = parse_source(source, SourceLanguage::Python).unwrap();
        assert_eq!(parsed.language, SourceLanguage::Python);
        assert!(!parsed.tree.root_node().has_error());
    }

    #[test]
    fn parse_rust_source() {
        let source = r#"
pub struct Connection {
    started: bool,
    closed: bool,
}

impl Connection {
    pub fn start(&mut self) {
        if !self.started {
            self.started = true;
        }
    }

    pub fn close(&mut self) {
        if !self.closed {
            self.closed = true;
        }
    }
}
"#;
        let parsed = parse_source(source, SourceLanguage::Rust).unwrap();
        assert_eq!(parsed.language, SourceLanguage::Rust);
        assert!(!parsed.tree.root_node().has_error());
    }

    #[test]
    fn node_line_numbers() {
        let source = "class Foo {\n    bar(): void {}\n}\n";
        let parsed = parse_source(source, SourceLanguage::TypeScript).unwrap();
        let root = parsed.tree.root_node();
        // Root starts at line 1
        assert_eq!(parsed.node_line(&root), 1);
    }

    /// mununu#1121 — the listed names are exactly the ones `from_name` accepts.
    #[test]
    fn x1121_every_listed_language_name_is_accepted() {
        for name in SourceLanguage::NAMES {
            assert!(
                SourceLanguage::from_name(name).is_some(),
                "`{name}` is listed but refused"
            );
        }
        assert!(SourceLanguage::from_name("cobol").is_none());
    }
}
