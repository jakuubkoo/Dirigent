//! Syntax highlighting with tree-sitter for the diff view and the file preview: language detection, whole-file
//! highlighting into per-line spans (with injections), a small content-hash cache that also keeps the parse trees.
//! No UI dependencies; everything here is blocking and runs off the UI thread. Grammars and their shipped queries
//! are MIT (license texts in `licenses/`).

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::ops::{ControlFlow, Range};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use tree_sitter::{Language, ParseOptions, ParseState, Parser, Tree};
use tree_sitter_highlight::{HighlightConfiguration, HighlightEvent, Highlighter};

pub mod structural;

/// Bigger files stay plain.
pub const MAX_BYTES: usize = 4 << 20;
pub const MAX_LINES: usize = 100_000;
const PARSE_DEADLINE: Duration = Duration::from_millis(400);
const HIGHLIGHT_DEADLINE: Duration = Duration::from_millis(500);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    Rust,
    Php,
    TypeScript,
    Tsx,
    JavaScript,
    Python,
    Json,
    Toml,
    Yaml,
    Css,
    Scss,
    Html,
    Markdown,
    MarkdownInline,
    Bash,
    Sql,
    Dockerfile,
    Cpp,
    Twig,
}

impl Lang {
    pub const ALL: [Lang; 19] = [
        Lang::Rust,
        Lang::Php,
        Lang::TypeScript,
        Lang::Tsx,
        Lang::JavaScript,
        Lang::Python,
        Lang::Json,
        Lang::Toml,
        Lang::Yaml,
        Lang::Css,
        Lang::Scss,
        Lang::Html,
        Lang::Markdown,
        Lang::MarkdownInline,
        Lang::Bash,
        Lang::Sql,
        Lang::Dockerfile,
        Lang::Cpp,
        Lang::Twig,
    ];

    fn index(self) -> usize {
        Self::ALL.iter().position(|l| *l == self).unwrap()
    }

    /// The language an injection names (`<script>`, a Markdown fence's info string, a heredoc's end marker).
    fn from_injection_name(name: &str) -> Option<Lang> {
        Some(match name.to_ascii_lowercase().as_str() {
            "rust" | "rs" => Lang::Rust,
            "php" => Lang::Php,
            "typescript" | "ts" => Lang::TypeScript,
            "tsx" => Lang::Tsx,
            "javascript" | "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
            "python" | "py" => Lang::Python,
            "json" | "jsonc" => Lang::Json,
            "toml" => Lang::Toml,
            "yaml" | "yml" => Lang::Yaml,
            "css" => Lang::Css,
            "scss" => Lang::Scss,
            "html" => Lang::Html,
            "markdown" | "md" => Lang::Markdown,
            "markdown_inline" => Lang::MarkdownInline,
            "bash" | "sh" | "shell" | "zsh" => Lang::Bash,
            "sql" => Lang::Sql,
            "dockerfile" | "containerfile" => Lang::Dockerfile,
            "cpp" | "c++" | "c" | "mql4" | "mql5" | "mql" => Lang::Cpp,
            "twig" => Lang::Twig,
            _ => return None,
        })
    }
}

/// Language by file name / extension; None = plain text.
pub fn detect(path: &Path) -> Option<Lang> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    match name.as_str() {
        "dockerfile" | "containerfile" => return Some(Lang::Dockerfile),
        "cargo.lock" | "poetry.lock" | "uv.lock" | "pipfile" => return Some(Lang::Toml),
        "composer.lock" | ".prettierrc" | ".eslintrc" => return Some(Lang::Json),
        ".bashrc" | ".bash_profile" | ".profile" | ".zshrc" | ".zprofile" | ".zshenv" | "pkgbuild" => {
            return Some(Lang::Bash);
        }
        _ => {}
    }
    if name.starts_with("dockerfile.") || name.starts_with("containerfile.") {
        return Some(Lang::Dockerfile);
    }
    let ext = name.rsplit_once('.')?.1;
    Some(match ext {
        "rs" => Lang::Rust,
        "php" | "phtml" | "php8" => Lang::Php,
        "ts" | "mts" | "cts" => Lang::TypeScript,
        "tsx" => Lang::Tsx,
        "js" | "mjs" | "cjs" | "jsx" => Lang::JavaScript,
        "py" | "pyi" => Lang::Python,
        "json" | "jsonc" | "json5" => Lang::Json,
        "toml" => Lang::Toml,
        "yaml" | "yml" => Lang::Yaml,
        "css" => Lang::Css,
        "scss" => Lang::Scss,
        "html" | "htm" => Lang::Html,
        "md" | "markdown" => Lang::Markdown,
        "sh" | "bash" | "zsh" => Lang::Bash,
        "sql" => Lang::Sql,
        "dockerfile" | "containerfile" => Lang::Dockerfile,
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "h" | "c" | "mq4" | "mq5" | "mqh" => Lang::Cpp,
        "twig" => Lang::Twig,
        _ => return None,
    })
}

/// What a span is colored as (the theme maps each to a color).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Class {
    Keyword,
    String,
    Number,
    Comment,
    Function,
    Type,
    Variable,
    Constant,
    Property,
    Punctuation,
    Operator,
    Tag,
    Attribute,
}

impl Class {
    pub const ALL: [Class; 13] = [
        Class::Keyword,
        Class::String,
        Class::Number,
        Class::Comment,
        Class::Function,
        Class::Type,
        Class::Variable,
        Class::Constant,
        Class::Property,
        Class::Punctuation,
        Class::Operator,
        Class::Tag,
        Class::Attribute,
    ];

    pub fn from_u8(v: u8) -> Option<Class> {
        Self::ALL.get(v as usize).copied()
    }
}

impl From<Class> for u8 {
    fn from(c: Class) -> u8 {
        c as u8
    }
}

/// Capture names the highlighter recognizes (it picks the longest dotted prefix match) and their class.
/// `none`/`embedded` are deliberately unrecognized (no events): tree-sitter-highlight nests a parent layer's capture
/// that starts where an injected one starts *inside* it, so its end pops the wrong span.
const NAMES: &[(&str, Class)] = &[
    ("attribute", Class::Attribute),
    ("boolean", Class::Constant),
    ("character", Class::String),
    ("comment", Class::Comment),
    ("constant", Class::Constant),
    ("constructor", Class::Type),
    ("escape", Class::String),
    ("field", Class::Property),
    ("float", Class::Number),
    ("function", Class::Function),
    ("keyword", Class::Keyword),
    ("label", Class::Constant),
    ("method", Class::Function),
    ("module", Class::Type),
    ("namespace", Class::Type),
    ("number", Class::Number),
    ("operator", Class::Operator),
    ("property", Class::Property),
    ("punctuation", Class::Punctuation),
    ("string", Class::String),
    ("tag", Class::Tag),
    ("text.literal", Class::String),
    ("text.reference", Class::Function),
    ("text.title", Class::Keyword),
    ("text.uri", Class::Property),
    ("type", Class::Type),
    ("variable", Class::Variable),
    ("variable.builtin", Class::Constant),
    ("variable.parameter", Class::Variable),
];

// Small injection queries that the grammar crates ship as files but don't export (MIT, same repos).
/// tree-sitter-php `queries/injections-text.scm`: the HTML around `<?php ?>`.
const PHP_TEXT_INJECTION: &str =
    "((text) @injection.content (#set! injection.language \"html\") (#set! injection.combined))";
/// gbprod/tree-sitter-twig `queries/injections.scm`.
const TWIG_INJECTION: &str =
    "((content) @injection.content (#set! injection.language \"html\") (#set! injection.combined))";

fn language(lang: Lang) -> Language {
    match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Php => tree_sitter_php::LANGUAGE_PHP.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::Json => tree_sitter_json::LANGUAGE.into(),
        Lang::Toml => tree_sitter_toml_ng::LANGUAGE.into(),
        Lang::Yaml => tree_sitter_yaml::LANGUAGE.into(),
        Lang::Css => tree_sitter_css::LANGUAGE.into(),
        Lang::Scss => tree_sitter_scss::language(),
        Lang::Html => tree_sitter_html::LANGUAGE.into(),
        Lang::Markdown => tree_sitter_md::LANGUAGE.into(),
        Lang::MarkdownInline => tree_sitter_md::INLINE_LANGUAGE.into(),
        Lang::Bash => tree_sitter_bash::LANGUAGE.into(),
        Lang::Sql => tree_sitter_sequel::LANGUAGE.into(),
        Lang::Dockerfile => tree_sitter_containerfile::LANGUAGE.into(),
        Lang::Cpp => tree_sitter_cpp::LANGUAGE.into(),
        Lang::Twig => tree_sitter_twig::LANGUAGE.into(),
    }
}

/// (highlights, injections, locals) as shipped with each grammar.
fn queries(lang: Lang) -> (String, String, &'static str) {
    let (h, i, l) = queries_raw(lang);
    let i = match lang {
        Lang::Php => format!("{i}\n{PHP_TEXT_INJECTION}"),
        // The inline grammar has to see the whole `inline` node, or tree-sitter-highlight gives it no ranges.
        Lang::Markdown => tree_sitter_md::INJECTION_QUERY_BLOCK.replace(
            "(#set! injection.language \"markdown_inline\"))",
            "(#set! injection.language \"markdown_inline\") (#set! injection.include-children))",
        ),
        _ => i.into(),
    };
    (h, i, l)
}

fn queries_raw(lang: Lang) -> (String, &'static str, &'static str) {
    use tree_sitter_javascript as js;
    match lang {
        Lang::Rust => (tree_sitter_rust::HIGHLIGHTS_QUERY.into(), tree_sitter_rust::INJECTIONS_QUERY, ""),
        // Both injection files (phpdoc/heredocs, and the HTML text around the PHP tags): see `queries`.
        Lang::Php => (tree_sitter_php::HIGHLIGHTS_QUERY.into(), tree_sitter_php::INJECTIONS_QUERY, ""),
        // TypeScript's queries only add to JavaScript's.
        Lang::TypeScript => (
            format!("{}\n{}", js::HIGHLIGHT_QUERY, tree_sitter_typescript::HIGHLIGHTS_QUERY),
            js::INJECTIONS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        Lang::Tsx => (
            format!(
                "{}\n{}\n{}",
                js::HIGHLIGHT_QUERY,
                js::JSX_HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
            js::INJECTIONS_QUERY,
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        Lang::JavaScript => {
            (format!("{}\n{}", js::HIGHLIGHT_QUERY, js::JSX_HIGHLIGHT_QUERY), js::INJECTIONS_QUERY, js::LOCALS_QUERY)
        }
        Lang::Python => (tree_sitter_python::HIGHLIGHTS_QUERY.into(), "", ""),
        Lang::Json => (tree_sitter_json::HIGHLIGHTS_QUERY.into(), "", ""),
        Lang::Toml => (tree_sitter_toml_ng::HIGHLIGHTS_QUERY.into(), "", ""),
        Lang::Yaml => (tree_sitter_yaml::HIGHLIGHTS_QUERY.into(), "", ""),
        Lang::Css => (tree_sitter_css::HIGHLIGHTS_QUERY.into(), "", ""),
        // SCSS's query only adds to CSS's.
        Lang::Scss => {
            (format!("{}\n{}", tree_sitter_css::HIGHLIGHTS_QUERY, tree_sitter_scss::HIGHLIGHTS_QUERY), "", "")
        }
        Lang::Html => (tree_sitter_html::HIGHLIGHTS_QUERY.into(), tree_sitter_html::INJECTIONS_QUERY, ""),
        Lang::Markdown => (markdown_block_highlights(), "", ""),
        Lang::MarkdownInline => {
            (tree_sitter_md::HIGHLIGHT_QUERY_INLINE.into(), tree_sitter_md::INJECTION_QUERY_INLINE, "")
        }
        Lang::Bash => (tree_sitter_bash::HIGHLIGHT_QUERY.into(), "", ""),
        Lang::Sql => (tree_sitter_sequel::HIGHLIGHTS_QUERY.into(), "", ""),
        Lang::Dockerfile => {
            (tree_sitter_containerfile::HIGHLIGHTS_QUERY.into(), tree_sitter_containerfile::INJECTIONS_QUERY, "")
        }
        // C++'s query only adds to C's.
        Lang::Cpp => (format!("{}\n{}", tree_sitter_c::HIGHLIGHT_QUERY, tree_sitter_cpp::HIGHLIGHT_QUERY), "", ""),
        Lang::Twig => (tree_sitter_twig::HIGHLIGHT_QUERY.into(), TWIG_INJECTION, ""),
    }
}

/// The shipped block query minus two patterns: the whole fenced block as `@text.literal` (its code is colored by the
/// injected language, the rest stays default) and `(code_fence_content) @none` (see `NAMES`).
fn markdown_block_highlights() -> String {
    let q = tree_sitter_md::HIGHLIGHT_QUERY_BLOCK;
    debug_assert!(q.contains("  (fenced_code_block)\n") && q.contains("(code_fence_content) @none"));
    q.replace("  (fenced_code_block)\n", "").replace("(code_fence_content) @none", "")
}

/// The highlight configuration of a language, built (queries compiled) on first use; None if a query doesn't
/// compile against its grammar (then that language stays plain).
fn config(lang: Lang) -> Option<&'static HighlightConfiguration> {
    static CONFIGS: [OnceLock<Option<HighlightConfiguration>>; 19] = [const { OnceLock::new() }; 19];
    CONFIGS[lang.index()]
        .get_or_init(|| {
            let (highlights, injections, locals) = queries(lang);
            let name = format!("{lang:?}").to_ascii_lowercase();
            let mut c = HighlightConfiguration::new(language(lang), name, &highlights, &injections, locals).ok()?;
            let names: Vec<&str> = NAMES.iter().map(|(n, _)| *n).collect();
            c.configure(&names);
            Some(c)
        })
        .as_ref()
}

/// Spans per line, as byte ranges within the raw line (without its `\n` / `\r\n`), sorted, non-overlapping.
pub type LineSpans = Vec<(Range<u32>, Class)>;

pub struct Highlighted {
    pub lines: Vec<LineSpans>,
    /// The main language's parse tree (kept for the structural diff). Injected languages aren't kept.
    pub tree: Tree,
}

/// Why highlighting gave up: a cancel isn't cached (the next load tries again), the rest is.
#[derive(Debug, PartialEq)]
pub enum Plain {
    Cancelled,
    TooBig,
    TooSlow,
    Failed,
}

/// Highlights a whole file. `cancel` non-zero aborts within one parser progress callback / 100 events.
pub fn highlight(lang: Lang, text: &str, cancel: &AtomicUsize) -> Result<Highlighted, Plain> {
    if text.len() > MAX_BYTES || text.bytes().filter(|&b| b == b'\n').count() >= MAX_LINES {
        return Err(Plain::TooBig);
    }
    let cfg = config(lang).ok_or(Plain::Failed)?;
    let started = Instant::now();
    let tree = parse(lang, text, cancel, started)?;

    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(cfg, text.as_bytes(), None, Some(cancel), |name| Lang::from_injection_name(name).and_then(config))
        .map_err(|_| Plain::Failed)?;
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices('\n').map(|(i, _)| i + 1)).collect();
    let mut lines: Vec<LineSpans> = vec![Vec::new(); starts.len()];
    let mut stack: Vec<Class> = Vec::new();
    for (n, ev) in events.enumerate() {
        if n % 256 == 0 && started.elapsed() > HIGHLIGHT_DEADLINE {
            return Err(Plain::TooSlow);
        }
        match ev {
            Ok(HighlightEvent::HighlightStart(h)) => stack.push(NAMES[h.0].1),
            Ok(HighlightEvent::HighlightEnd) => {
                stack.pop();
            }
            Ok(HighlightEvent::Source { start, end }) => {
                if let Some(class) = stack.last() {
                    push_span(&mut lines, &starts, text, start..end, *class);
                }
            }
            Err(tree_sitter_highlight::Error::Cancelled) => return Err(Plain::Cancelled),
            Err(_) => return Err(Plain::Failed),
        }
    }
    if cancel.load(Ordering::Relaxed) != 0 {
        return Err(Plain::Cancelled);
    }
    Ok(Highlighted { lines, tree })
}

fn parse(lang: Lang, text: &str, cancel: &AtomicUsize, started: Instant) -> Result<Tree, Plain> {
    let mut parser = Parser::new();
    parser.set_language(&language(lang)).map_err(|_| Plain::Failed)?;
    let bytes = text.as_bytes();
    let mut progress = |_: &ParseState| {
        if cancel.load(Ordering::Relaxed) != 0 || started.elapsed() > PARSE_DEADLINE {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let tree = parser.parse_with_options(
        &mut |i, _| if i < bytes.len() { &bytes[i..] } else { &[] },
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    match tree {
        Some(t) => Ok(t),
        None if cancel.load(Ordering::Relaxed) != 0 => Err(Plain::Cancelled),
        None => Err(Plain::TooSlow),
    }
}

/// Splits a whole-text byte range into per-line spans (line breaks excluded).
fn push_span(lines: &mut [LineSpans], starts: &[usize], text: &str, range: Range<usize>, class: Class) {
    let mut line = starts.partition_point(|&s| s <= range.start) - 1;
    let mut pos = range.start;
    while pos < range.end && line < lines.len() {
        let line_start = starts[line];
        let mut line_end = starts.get(line + 1).map_or(text.len(), |&s| s - 1);
        if line_end > line_start && text.as_bytes()[line_end - 1] == b'\r' {
            line_end -= 1;
        }
        let end = range.end.min(line_end);
        if end > pos {
            let r = (pos - line_start) as u32..(end - line_start) as u32;
            let spans = &mut lines[line];
            // Adjacent same-class pieces (e.g. split by an injection boundary) merge.
            match spans.last_mut() {
                Some((last, c)) if *c == class && last.end == r.start => last.end = r.end,
                _ => spans.push((r, class)),
            }
        }
        line += 1;
        pos = starts.get(line).copied().unwrap_or(range.end);
    }
}

/// Recently highlighted files by (language, content hash). Holds the trees too. Use from background tasks.
type Key = (Lang, u64);

fn text_hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// A small LRU map (insertion order; a hit doesn't refresh).
struct Lru<K, V> {
    map: HashMap<K, V>,
    /// Insertion order, oldest first.
    order: VecDeque<K>,
    cap: usize,
}

impl<K: Copy + Eq + std::hash::Hash, V: Clone> Lru<K, V> {
    fn new(cap: usize) -> Self {
        Self { map: HashMap::new(), order: VecDeque::new(), cap }
    }

    fn get(&self, k: &K) -> Option<V> {
        self.map.get(k).cloned()
    }

    fn insert(&mut self, k: K, v: V) {
        if self.map.insert(k, v).is_none() {
            self.order.push_back(k);
            while self.order.len() > self.cap {
                if let Some(old) = self.order.pop_front() {
                    self.map.remove(&old);
                }
            }
        }
    }
}

pub struct Cache {
    /// None: highlighting gave up (too big/slow/failed), so the next load doesn't try again.
    inner: Mutex<Lru<Key, Option<Arc<Highlighted>>>>,
}

impl Default for Cache {
    fn default() -> Self {
        Self { inner: Mutex::new(Lru::new(CACHE_ENTRIES)) }
    }
}

const CACHE_ENTRIES: usize = 32;

impl Cache {
    /// The cached result, or a fresh highlight (cached, failures too, unless cancelled). Err = show plain text.
    pub fn get(&self, lang: Lang, text: &str, cancel: &AtomicUsize) -> Result<Arc<Highlighted>, Plain> {
        let key = (lang, text_hash(text));
        if let Some(hit) = self.inner.lock().unwrap().get(&key) {
            return hit.ok_or(Plain::Failed);
        }
        let result = match highlight(lang, text, cancel) {
            Err(Plain::Cancelled) => return Err(Plain::Cancelled),
            r => r.map(Arc::new),
        };
        self.inner.lock().unwrap().insert(key, result.as_ref().ok().cloned());
        result
    }
}

type StructuralResult = Result<Arc<structural::Structural>, structural::Fail>;

/// Recent structural diffs by (language, old content hash, new content hash), failures included (not cancels).
pub struct StructuralCache {
    inner: Mutex<Lru<(Lang, u64, u64), StructuralResult>>,
}

impl Default for StructuralCache {
    fn default() -> Self {
        Self { inner: Mutex::new(Lru::new(16)) }
    }
}

impl StructuralCache {
    /// `old`/`new`: the texts with their highlights (trees) and the line diff's changed lines.
    pub fn get(
        &self,
        lang: Lang,
        old: structural::Side,
        new: structural::Side,
        cancel: &AtomicUsize,
    ) -> Result<Arc<structural::Structural>, structural::Fail> {
        if matches!(lang, Lang::Markdown | Lang::MarkdownInline) {
            return Err(structural::Fail::Unsupported);
        }
        let key = (lang, text_hash(old.text), text_hash(new.text));
        if let Some(hit) = self.inner.lock().unwrap().get(&key) {
            return hit;
        }
        let result = structural::diff(old, new, cancel, structural::BUDGET).map(Arc::new);
        if result != Err(structural::Fail::Cancelled) {
            self.inner.lock().unwrap().insert(key, result.clone());
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(lang: Lang, text: &str) -> Vec<Vec<(String, Class)>> {
        let h = highlight(lang, text, &AtomicUsize::new(0)).unwrap();
        let lines: Vec<&str> = text.split('\n').collect();
        h.lines
            .iter()
            .zip(lines)
            .map(|(s, l)| s.iter().map(|(r, c)| (l[r.start as usize..r.end as usize].to_string(), *c)).collect())
            .collect()
    }

    fn has(spans: &[Vec<(String, Class)>], text: &str, class: Class) -> bool {
        spans.iter().flatten().any(|(t, c)| t == text && *c == class)
    }

    #[test]
    fn every_language_configures() {
        for lang in Lang::ALL {
            assert!(config(lang).is_some(), "{lang:?} queries compile");
        }
    }

    #[test]
    fn detection() {
        let d = |p: &str| detect(Path::new(p));
        assert_eq!(d("src/main.rs"), Some(Lang::Rust));
        assert_eq!(d("a/Dockerfile"), Some(Lang::Dockerfile));
        assert_eq!(d("Dockerfile.prod"), Some(Lang::Dockerfile));
        assert_eq!(d("Cargo.lock"), Some(Lang::Toml));
        assert_eq!(d("composer.lock"), Some(Lang::Json));
        assert_eq!(d("x/Expert.MQ5"), Some(Lang::Cpp));
        assert_eq!(d("inc/lib.mqh"), Some(Lang::Cpp));
        assert_eq!(d("view.html.twig"), Some(Lang::Twig));
        assert_eq!(d("App.tsx"), Some(Lang::Tsx));
        assert_eq!(d(".zshrc"), Some(Lang::Bash));
        assert_eq!(d("LICENSE"), None);
        assert_eq!(d("notes.xyz"), None);
    }

    #[test]
    fn each_language_colors_something() {
        let samples: &[(Lang, &str, &str, Class)] = &[
            (Lang::Rust, "fn main() { let x = \"hi\"; }", "fn", Class::Keyword),
            (Lang::Php, "<?php\nfunction f() { return 'x'; }", "function", Class::Keyword),
            (Lang::TypeScript, "const x: number = 1;", "const", Class::Keyword),
            (Lang::Tsx, "const a = <div className=\"x\" />;", "const", Class::Keyword),
            (Lang::JavaScript, "function f() { return 1; }", "function", Class::Keyword),
            (Lang::Python, "def f():\n    return 'x'", "def", Class::Keyword),
            (Lang::Json, "{\"a\": 1}", "1", Class::Number),
            (Lang::Toml, "[package]\nname = \"x\"", "\"x\"", Class::String),
            (Lang::Yaml, "key: 12", "12", Class::Number),
            (Lang::Css, "a { color: red; }", "color", Class::Property),
            (Lang::Scss, "$x: 1px;\na { color: $x; }", "color", Class::Property),
            (Lang::Html, "<div class=\"x\">t</div>", "div", Class::Tag),
            (Lang::Markdown, "# Title\n", "Title", Class::Keyword),
            (Lang::Bash, "if true; then echo hi; fi", "if", Class::Keyword),
            (Lang::Sql, "SELECT a FROM t WHERE b = 1;", "SELECT", Class::Keyword),
            (Lang::Dockerfile, "FROM alpine\nRUN echo hi", "FROM", Class::Keyword),
            (Lang::Cpp, "int main() { return 0; }", "return", Class::Keyword),
            (Lang::Twig, "{% if user %}{{ user.name }}{% endif %}", "if", Class::Keyword),
        ];
        for (lang, text, token, class) in samples {
            let s = spans(*lang, text);
            assert!(has(&s, token, *class), "{lang:?}: `{token}` as {class:?} in {s:?}");
        }
    }

    #[test]
    fn multi_line_comments_and_strings_cover_every_line() {
        let s = spans(Lang::Rust, "/* one\ntwo\nthree */\nlet s = \"a\nb\";");
        assert!(has(&s, "/* one", Class::Comment));
        assert!(has(&s, "two", Class::Comment));
        assert!(has(&s, "three */", Class::Comment));
        assert!(has(&s, "\"a", Class::String));
        assert!(has(&s, "b\"", Class::String));
    }

    #[test]
    fn crlf_line_ends_are_not_colored() {
        let h = highlight(Lang::Rust, "// a\r\nfn x() {}\r\n", &AtomicUsize::new(0)).unwrap();
        assert_eq!(h.lines[0], vec![(0..4, Class::Comment)]);
    }

    #[test]
    fn injections() {
        let php = spans(Lang::Php, "<div class=\"a\"><?php echo 1; ?></div>");
        assert!(has(&php, "div", Class::Tag), "HTML around PHP: {php:?}");
        assert!(has(&php, "echo", Class::Keyword));
        let html = spans(Lang::Html, "<script>const a = 1;</script><style>a { color: red; }</style>");
        assert!(has(&html, "const", Class::Keyword), "JS in HTML: {html:?}");
        assert!(has(&html, "color", Class::Property), "CSS in HTML: {html:?}");
        let md = spans(Lang::Markdown, "# T\n\n```rust\nfn main() {}\n```\n\nsome `code` here\n");
        assert!(has(&md, "fn", Class::Keyword), "Rust fence in Markdown: {md:?}");
        assert!(has(&md, "code", Class::String), "inline code: {md:?}");
    }

    #[test]
    fn cancel_and_caps() {
        let big: String = (0..3_000).map(|i| format!("fn f{i}() {{ let x = {i}; }}\n")).collect();
        let t = Instant::now();
        assert!(highlight(Lang::Rust, &big, &AtomicUsize::new(0)).is_ok());
        let full = t.elapsed();
        let t = Instant::now();
        assert!(matches!(highlight(Lang::Rust, &big, &AtomicUsize::new(1)), Err(Plain::Cancelled)));
        assert!(t.elapsed() * 4 < full, "cancel stops early: {:?} vs {full:?}", t.elapsed());
        let huge = "x\n".repeat(MAX_LINES);
        assert!(matches!(highlight(Lang::Rust, &huge, &AtomicUsize::new(0)), Err(Plain::TooBig)));
    }

    #[test]
    fn cache_returns_the_same_result_for_the_same_text() {
        let cache = Cache::default();
        let flag = AtomicUsize::new(0);
        let a = cache.get(Lang::Rust, "fn a() {}", &flag).unwrap();
        let b = cache.get(Lang::Rust, "fn a() {}", &flag).unwrap();
        assert!(Arc::ptr_eq(&a, &b));
        assert_eq!(a.tree.root_node().kind(), "source_file");
        assert!(cache.get(Lang::Rust, "fn a() {}", &AtomicUsize::new(1)).is_ok(), "a hit ignores the cancel flag");
        let big = "x\n".repeat(MAX_LINES);
        assert_eq!(cache.get(Lang::Rust, &big, &flag).err(), Some(Plain::TooBig));
        assert_eq!(cache.get(Lang::Rust, &big, &flag).err(), Some(Plain::Failed), "the failure is cached");
        assert_eq!(cache.get(Lang::Rust, "fn z() {}", &AtomicUsize::new(1)).err(), Some(Plain::Cancelled));
        assert!(cache.get(Lang::Rust, "fn z() {}", &flag).is_ok(), "a cancel isn't cached");
    }

    /// Timing harness (release): `SYNTAX_BENCH=<file> cargo test --release -p syntax bench -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bench() {
        let path = std::env::var("SYNTAX_BENCH").expect("SYNTAX_BENCH=<file>");
        let text = std::fs::read_to_string(&path).unwrap();
        let lang = detect(Path::new(&path)).unwrap();
        let flag = AtomicUsize::new(0);
        let _ = config(lang); // compile queries once, outside the timing
        let _ = highlight(lang, &text, &flag);
        let t = Instant::now();
        parse(lang, &text, &flag, Instant::now()).unwrap();
        let p = t.elapsed();
        let t = Instant::now();
        let h = highlight(lang, &text, &flag).unwrap();
        let all = t.elapsed();
        let spans: usize = h.lines.iter().map(Vec::len).sum();
        println!(
            "{path}: {} lines, parse {:.1} ms, parse + highlight {:.1} ms, {spans} spans",
            h.lines.len(),
            p.as_secs_f64() * 1e3,
            all.as_secs_f64() * 1e3
        );
        let t = Instant::now();
        let _ = config(Lang::Php);
        let _ = config(Lang::Rust);
        println!("query compile (cached now): {:.2} ms", t.elapsed().as_secs_f64() * 1e3);
    }
}
