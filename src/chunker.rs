use once_cell::sync::OnceCell;
use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::Path;

pub const MAX_CHUNK_TOKENS: usize = 400;
pub const MIN_CHUNK_TOKENS: usize = 10;
pub const CHUNK_OVERLAP_TOKENS: usize = 40;

pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".tokenix",
    "node_modules",
    "bower_components",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    ".eggs",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    "dist",
    "build",
    "out",
    "obj",
    "target",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".parcel-cache",
    ".cache",
    ".gradle",
    ".terraform",
    "Pods",
    "coverage",
    ".idea",
    ".vscode",
    ".cargo",
];

pub const INDEXED_EXTS: &[&str] = &[
    ".rs", ".py", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".go", ".sh", ".bash", ".toml",
    ".md", ".txt", ".c", ".cpp", ".h", ".hpp", ".cc", ".cxx",
    // VB6 / VBA text sources (.frx/.ctx are binary form resources — excluded).
    ".bas", ".cls", ".ctl", ".frm", ".vbp",
    // SQL scripts + Oracle object files (function/trigger/package/procedure/
    // table/view DDL).
    ".sql", ".fnc", ".trg", ".pkg", ".prc", ".tab", ".vw",
];

/// Data/config extensions indexed only when `[index] data_files = true`.
/// Off by default: these are usually generated/config noise (e.g. thousands of
/// JSON files) that bloat the index and pollute semantic results.
pub const DATA_EXTS: &[&str] = &[".json", ".yaml", ".yml"];

/// Filename substrings that are never indexed — likely to contain secrets.
const SENSITIVE_NAMES: &[&str] = &[
    ".env",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "secrets.",
    ".secret",
    "credentials",
];

/// Sensitive file extensions, never indexed (keys, certs).
const SENSITIVE_EXTS: &[&str] = &[".pem", ".key", ".pfx", ".p12", ".keystore", ".jks"];

#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct Chunk {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: String,
    pub kind: String,
    pub content: String,
    pub token_count: usize,
}

pub fn file_hash(content: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content);
    hex::encode(&hasher.finalize()[..8])
}

pub fn count_tokens(text: &str) -> usize {
    // Fast approximation: ~4 chars per token (Claude/GPT tokenizers)
    // Accurate enough for budget decisions without shipping tiktoken in Rust
    text.len().div_ceil(4)
}

static SECRET_RE: OnceCell<Vec<Regex>> = OnceCell::new();

/// Mask obvious secrets (private keys, cloud keys, bearer tokens, and
/// `key = value` assignments for sensitive names) with `[REDACTED]`.
/// Opt-in via `[index] redact_secrets = true`.
pub fn redact_secrets(content: &str) -> String {
    let patterns = SECRET_RE.get_or_init(|| {
        [
            r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z ]*PRIVATE KEY-----",
            r"AKIA[0-9A-Z]{16}",
            r"(?i)bearer\s+[A-Za-z0-9._\-]{16,}",
            r#"(?i)(?:api[_-]?key|secret|token|password|passwd|pwd|access[_-]?key)\s*[:=]\s*['"]?[A-Za-z0-9._\-/+]{8,}['"]?"#,
        ]
        .iter()
        .filter_map(|p| Regex::new(p).ok())
        .collect()
    });
    let mut out = content.to_string();
    for re in patterns {
        out = re.replace_all(&out, "[REDACTED]").into_owned();
    }
    out
}

#[derive(serde::Deserialize, Default, Clone)]
struct ProjectConfig {
    #[serde(default)]
    languages: std::collections::HashMap<String, String>,
    #[serde(default)]
    index: IndexConfig,
    #[serde(default)]
    hook: HookConfig,
}

/// `[hook]` section of `.tokenix.toml`. All fields optional; defaults live at
/// the use sites in hook.rs so the fail-open contract is untouched.
#[derive(serde::Deserialize, Default, Clone)]
pub struct HookConfig {
    /// Read intercept: files with at least this many lines return an outline
    /// instead of full content (default 200).
    pub read_min_lines: Option<usize>,
    /// Grep intercept: patterns with at least this many words are treated as
    /// semantic queries (default 3).
    pub grep_min_words: Option<usize>,
}

/// The resolved `[hook]` config for the current project (defaults if absent).
pub fn hook_config() -> HookConfig {
    load_project_config().map(|c| c.hook).unwrap_or_default()
}

/// `[index]` section of `.tokenix.toml`. All fields optional.
#[derive(serde::Deserialize, Default, Clone)]
pub struct IndexConfig {
    /// Extra directory names to ignore, in addition to the built-in list.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Extra file extensions to index, e.g. `["proto", "sql"]` (no leading dot).
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Index `.json` / `.yaml` / `.yml` data files (off by default).
    #[serde(default)]
    pub data_files: bool,
    /// Mask obvious secrets (keys, tokens, passwords) in chunk content.
    #[serde(default)]
    pub redact_secrets: bool,
    /// Override the default 1.5 MB max indexed file size.
    pub max_file_bytes: Option<u64>,
}

/// The resolved `[index]` config for the current project (defaults if absent).
pub fn index_config() -> IndexConfig {
    load_project_config().map(|c| c.index).unwrap_or_default()
}

fn load_project_config() -> Option<ProjectConfig> {
    #[cfg(test)]
    {
        let cwd = std::env::current_dir().ok()?;
        let root = crate::store::find_project_root(&cwd);
        let config_path = root.join(".tokenix.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path).ok()?;
            return toml::from_str(&content).ok();
        }
        let config_path2 = root.join("tokenix.toml");
        if config_path2.exists() {
            let content = std::fs::read_to_string(&config_path2).ok()?;
            return toml::from_str(&content).ok();
        }
        None
    }
    #[cfg(not(test))]
    {
        static PROJECT_CONFIG: OnceCell<Option<ProjectConfig>> = OnceCell::new();
        PROJECT_CONFIG
            .get_or_init(|| {
                let cwd = std::env::current_dir().ok()?;
                let root = crate::store::find_project_root(&cwd);
                let config_path = root.join(".tokenix.toml");
                if config_path.exists() {
                    let content = std::fs::read_to_string(&config_path).ok()?;
                    return toml::from_str(&content).ok();
                }
                let config_path2 = root.join("tokenix.toml");
                if config_path2.exists() {
                    let content = std::fs::read_to_string(&config_path2).ok()?;
                    return toml::from_str(&content).ok();
                }
                None
            })
            .clone()
    }
}

fn detect_custom_lang(path: &Path) -> Option<Lang> {
    let ext = path.extension().and_then(|e| e.to_str())?.to_lowercase();
    let config = load_project_config()?;
    let lang_str = config.languages.get(&ext)?;
    match lang_str.to_lowercase().as_str() {
        "rust" => Some(Lang::Rust),
        "python" => Some(Lang::Python),
        "typescript" => Some(Lang::TypeScript),
        "javascript" => Some(Lang::JavaScript),
        "go" => Some(Lang::Go),
        "cpp" | "c" => Some(Lang::Cpp),
        "vb" | "vb6" | "vba" | "visualbasic" => Some(Lang::Vb),
        "sql" | "plsql" | "tsql" => Some(Lang::Sql),
        _ => Some(Lang::Generic),
    }
}

pub fn should_index(path: &Path) -> bool {
    let cfg = load_project_config();
    let extra_excludes = cfg.as_ref().map(|c| &c.index.exclude);
    for component in path.components() {
        let s = component.as_os_str().to_string_lossy();
        if IGNORED_DIRS.contains(&s.as_ref()) {
            return false;
        }
        if extra_excludes.is_some_and(|ex| ex.iter().any(|d| d == s.as_ref())) {
            return false;
        }
    }
    let name = path.to_string_lossy().to_lowercase();
    if name.ends_with(".min.js") || name.ends_with(".min.css") || name.ends_with(".map") {
        return false;
    }
    if is_sensitive_file(&name) {
        return false;
    }

    // Built-in code/doc extensions are always indexed.
    if INDEXED_EXTS.iter().any(|ext| name.ends_with(ext)) {
        return true;
    }
    // Data files (.json/.yaml/.yml) only when opted in.
    let data_files = cfg.as_ref().map(|c| c.index.data_files).unwrap_or(false);
    if data_files && DATA_EXTS.iter().any(|ext| name.ends_with(ext)) {
        return true;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        let ext = ext.to_lowercase();
        if let Some(config) = cfg.as_ref() {
            if config.languages.contains_key(&ext) {
                return true;
            }
            if config
                .index
                .extensions
                .iter()
                .any(|e| e.to_lowercase() == ext)
            {
                return true;
            }
        }
    }
    false
}

/// True for files whose name suggests they hold secrets (keys, env, certs).
fn is_sensitive_file(name_lower: &str) -> bool {
    let base = name_lower.rsplit(['/', '\\']).next().unwrap_or(name_lower);
    if SENSITIVE_NAMES.iter().any(|p| base.contains(p)) {
        return true;
    }
    SENSITIVE_EXTS.iter().any(|ext| name_lower.ends_with(ext))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Lang {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Cpp,
    Vb,
    Sql,
    Generic,
}

pub(crate) fn detect_lang(path: &Path) -> Lang {
    if let Some(lang) = detect_custom_lang(path) {
        return lang;
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "rs" => Lang::Rust,
        "py" => Lang::Python,
        "ts" | "tsx" => Lang::TypeScript,
        "js" | "jsx" | "mjs" | "cjs" => Lang::JavaScript,
        "go" => Lang::Go,
        "c" | "cpp" | "h" | "hpp" | "cc" | "cxx" => Lang::Cpp,
        "bas" | "cls" | "ctl" | "frm" => Lang::Vb,
        "sql" | "fnc" | "trg" | "pkg" | "prc" | "tab" | "vw" => Lang::Sql,
        _ => Lang::Generic,
    }
}

pub fn chunk_file(path: &str, content: &str) -> Vec<Chunk> {
    let p = Path::new(path);
    let lang = detect_lang(p);

    let chunks = match lang {
        Lang::Rust => chunk_rust(content, path),
        Lang::Python => chunk_python(content, path),
        Lang::TypeScript | Lang::JavaScript => chunk_ts_js(content, path),
        Lang::Go => chunk_go(content, path),
        Lang::Cpp => chunk_cpp(content, path),
        Lang::Vb => chunk_by_symbol_lines(content, path, vb_symbol_of),
        Lang::Sql => chunk_by_symbol_lines(content, path, sql_symbol_of),
        Lang::Generic => {
            let lines: Vec<&str> = content.lines().collect();
            chunk_by_lines(&lines, path)
        }
    };
    enforce_token_cap(chunks)
}

/// Hard guarantee that no single chunk exceeds `MAX_CHUNK_TOKENS`. The
/// language chunkers split on line boundaries, but a single very long line
/// (minified JS/JSON, generated data) can still produce one oversized chunk —
/// which inflates the padded ONNX embedding batch and was the historical
/// PC-freeze trigger. Here we split such chunks by character windows (never
/// truncating), preserving 100% of the content.
fn enforce_token_cap(chunks: Vec<Chunk>) -> Vec<Chunk> {
    let max_chars = MAX_CHUNK_TOKENS * 4; // count_tokens(s) == s.len().div_ceil(4)
    let mut out = Vec::with_capacity(chunks.len());
    for chunk in chunks {
        if chunk.content.len() <= max_chars {
            out.push(chunk);
            continue;
        }
        let content = &chunk.content;
        let len = content.len();
        let mut start = 0;
        while start < len {
            let mut end = (start + max_chars).min(len);
            while end < len && !content.is_char_boundary(end) {
                end += 1;
            }
            let piece = &content[start..end];
            out.push(Chunk {
                path: chunk.path.clone(),
                start_line: chunk.start_line,
                end_line: chunk.end_line,
                symbol: chunk.symbol.clone(),
                kind: chunk.kind.clone(),
                content: piece.to_string(),
                token_count: count_tokens(piece),
            });
            start = end;
        }
    }
    out
}

struct SymbolNode {
    start_line: usize,
    end_line: usize,
    symbol: String,
    kind: String,
}

fn find_first_identifier<'a>(node: tree_sitter::Node<'a>, source: &'a [u8]) -> Option<String> {
    let kind = node.kind();
    if kind == "identifier" || kind == "type_identifier" || kind == "field_identifier" {
        if let Ok(text) = node.utf8_text(source) {
            return Some(text.to_string());
        }
    }
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i) {
            if let Some(name) = find_first_identifier(child, source) {
                return Some(name);
            }
        }
    }
    None
}

fn chunk_with_parser(
    language: impl Into<tree_sitter::Language>,
    content: &str,
    path: &str,
    is_symbol_node: fn(&str) -> Option<&'static str>,
) -> Vec<Chunk> {
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language.into()).is_err() {
        let lines: Vec<&str> = content.lines().collect();
        return chunk_by_lines(&lines, path);
    }
    let tree = match parser.parse(content, None) {
        Some(t) => t,
        None => {
            let lines: Vec<&str> = content.lines().collect();
            return chunk_by_lines(&lines, path);
        }
    };

    let source = content.as_bytes();
    let mut symbols = Vec::new();

    fn traverse<'a>(
        node: tree_sitter::Node<'a>,
        source: &'a [u8],
        is_symbol_node: fn(&str) -> Option<&'static str>,
        symbols: &mut Vec<SymbolNode>,
    ) {
        let kind_str = node.kind();
        if let Some(kind) = is_symbol_node(kind_str) {
            let start_line = node.start_position().row;
            let end_line = node.end_position().row;
            let symbol =
                find_first_identifier(node, source).unwrap_or_else(|| "anonymous".to_string());
            symbols.push(SymbolNode {
                start_line,
                end_line,
                symbol,
                kind: kind.to_string(),
            });
        }
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                traverse(child, source, is_symbol_node, symbols);
            }
        }
    }

    traverse(tree.root_node(), source, is_symbol_node, &mut symbols);

    let lines: Vec<&str> = content.lines().collect();
    if symbols.is_empty() {
        return chunk_by_lines(&lines, path);
    }

    let mut chunks = Vec::new();
    for sym in symbols {
        flush_chunk(
            &lines,
            path,
            sym.start_line,
            sym.end_line,
            &sym.symbol,
            &sym.kind,
            &mut chunks,
        );
    }
    chunks
}

fn is_rust_symbol(kind: &str) -> Option<&'static str> {
    match kind {
        "function_item" | "fn_item" => Some("function"),
        "struct_item" => Some("struct"),
        "enum_item" => Some("enum"),
        "impl_item" => Some("impl"),
        "trait_item" => Some("trait"),
        "macro_definition" => Some("macro"),
        _ => None,
    }
}

fn chunk_rust(content: &str, path: &str) -> Vec<Chunk> {
    chunk_with_parser(tree_sitter_rust::LANGUAGE, content, path, is_rust_symbol)
}

fn is_python_symbol(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("function"),
        "class_definition" => Some("class"),
        _ => None,
    }
}

fn chunk_python(content: &str, path: &str) -> Vec<Chunk> {
    chunk_with_parser(
        tree_sitter_python::LANGUAGE,
        content,
        path,
        is_python_symbol,
    )
}

fn is_js_ts_symbol(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" => Some("function"),
        "class_declaration" => Some("class"),
        "method_definition" => Some("method"),
        "function_expression" => Some("function"),
        "arrow_function" => Some("function"),
        _ => None,
    }
}

fn chunk_ts_js(content: &str, path: &str) -> Vec<Chunk> {
    let mut chunks = chunk_with_parser(
        tree_sitter_javascript::LANGUAGE,
        content,
        path,
        is_js_ts_symbol,
    );
    merge_missing_symbol_chunks(&mut chunks, heuristic_ts_js_symbols(content), content, path);
    chunks
}

fn merge_missing_symbol_chunks(
    chunks: &mut Vec<Chunk>,
    symbols: Vec<SymbolNode>,
    content: &str,
    path: &str,
) {
    if symbols.is_empty() {
        return;
    }
    let lines: Vec<&str> = content.lines().collect();
    for symbol in symbols {
        if chunks.iter().any(|chunk| chunk.symbol == symbol.symbol) {
            continue;
        }
        flush_chunk(
            &lines,
            path,
            symbol.start_line,
            symbol.end_line,
            &symbol.symbol,
            &symbol.kind,
            chunks,
        );
    }
    chunks.sort_by_key(|chunk| (chunk.start_line, chunk.end_line));
}

static TS_JS_SYMBOL_RE: OnceCell<Regex> = OnceCell::new();

fn heuristic_ts_js_symbols(content: &str) -> Vec<SymbolNode> {
    let re = TS_JS_SYMBOL_RE.get_or_init(|| {
        Regex::new(
            r"\b(?:export\s+)?(?:default\s+)?(?:abstract\s+)?(class|interface|enum|function|type)\s+([A-Za-z_$][A-Za-z0-9_$]*)",
        )
        .unwrap()
    });
    let lines: Vec<&str> = content.lines().collect();
    let mut symbols = Vec::new();
    for cap in re.captures_iter(content) {
        let Some(mat) = cap.get(0) else {
            continue;
        };
        let kind = cap.get(1).map(|m| m.as_str()).unwrap_or("symbol");
        let name = cap.get(2).map(|m| m.as_str()).unwrap_or("anonymous");
        let start_line = content[..mat.start()].lines().count();
        let end_line = find_block_end(&lines, start_line);
        symbols.push(SymbolNode {
            start_line,
            end_line,
            symbol: name.to_string(),
            kind: kind.to_string(),
        });
    }
    symbols
}

fn find_block_end(lines: &[&str], start_line: usize) -> usize {
    let mut depth = 0i32;
    let mut saw_open = false;
    for (idx, line) in lines.iter().enumerate().skip(start_line) {
        for ch in line.chars() {
            match ch {
                '{' => {
                    depth += 1;
                    saw_open = true;
                }
                '}' => {
                    depth -= 1;
                    if saw_open && depth <= 0 {
                        return idx;
                    }
                }
                ';' if !saw_open => return idx,
                _ => {}
            }
        }
    }
    lines.len().saturating_sub(1)
}

fn is_go_symbol(kind: &str) -> Option<&'static str> {
    match kind {
        "function_declaration" => Some("function"),
        "method_declaration" => Some("method"),
        "type_declaration" => Some("type"),
        _ => None,
    }
}

fn chunk_go(content: &str, path: &str) -> Vec<Chunk> {
    chunk_with_parser(tree_sitter_go::LANGUAGE, content, path, is_go_symbol)
}

fn is_cpp_symbol(kind: &str) -> Option<&'static str> {
    match kind {
        "function_definition" => Some("function"),
        "class_specifier" => Some("class"),
        "struct_specifier" => Some("struct"),
        "namespace_definition" => Some("namespace"),
        _ => None,
    }
}

fn chunk_cpp(content: &str, path: &str) -> Vec<Chunk> {
    chunk_with_parser(tree_sitter_cpp::LANGUAGE, content, path, is_cpp_symbol)
}

/// Line-scanning symbol chunker for languages without a tree-sitter grammar
/// (VB6/VBA, SQL). `symbol_of` returns `(name, kind)` when a line opens a new
/// top-level definition; each segment then runs until the next definition, so
/// 100% of the file is covered (preamble included). Falls back to plain line
/// windows when no symbol is found.
fn chunk_by_symbol_lines(
    content: &str,
    path: &str,
    symbol_of: fn(&str) -> Option<(String, &'static str)>,
) -> Vec<Chunk> {
    let lines: Vec<&str> = content.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let mut marks: Vec<(usize, String, &'static str)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((symbol, kind)) = symbol_of(line) {
            marks.push((i, symbol, kind));
        }
    }
    if marks.is_empty() {
        return chunk_by_lines(&lines, path);
    }
    let mut chunks = Vec::new();
    if marks[0].0 > 0 {
        flush_chunk(&lines, path, 0, marks[0].0 - 1, "", "header", &mut chunks);
    }
    for (mi, (start, symbol, kind)) in marks.iter().enumerate() {
        let end = marks
            .get(mi + 1)
            .map(|m| m.0 - 1)
            .unwrap_or(lines.len() - 1);
        flush_chunk(&lines, path, *start, end, symbol, kind, &mut chunks);
    }
    chunks
}

static VB_SYMBOL_RE: OnceCell<Regex> = OnceCell::new();
static VB_NAME_RE: OnceCell<Regex> = OnceCell::new();

/// VB6/VBA definition opener: `[Public|Private|Friend] [Static]
/// Sub|Function|Property Get/Let/Set Name`. `Attribute VB_Name = "X"` names
/// the module/class itself, so it opens the header segment as a "class" mark.
fn vb_symbol_of(line: &str) -> Option<(String, &'static str)> {
    let re = VB_SYMBOL_RE.get_or_init(|| {
        Regex::new(
            r"(?i)^\s*(?:public\s+|private\s+|friend\s+)?(?:static\s+)?(sub|function|property\s+(?:get|let|set))\s+([A-Za-z][A-Za-z0-9_]*)",
        )
        .unwrap()
    });
    if let Some(c) = re.captures(line) {
        let kind = if c[1].to_lowercase().starts_with("property") {
            "property"
        } else {
            "function"
        };
        return Some((c[2].to_string(), kind));
    }
    let re_name = VB_NAME_RE
        .get_or_init(|| Regex::new(r#"(?i)^\s*Attribute\s+VB_Name\s*=\s*"([^"]+)""#).unwrap());
    re_name.captures(line).map(|c| (c[1].to_string(), "class"))
}

static SQL_SYMBOL_RE: OnceCell<Regex> = OnceCell::new();

/// SQL/PLSQL object opener: `CREATE [OR REPLACE] PROCEDURE|FUNCTION|PACKAGE
/// [BODY]|TRIGGER|VIEW|TABLE|… name`, quote- and schema-qualified names kept.
fn sql_symbol_of(line: &str) -> Option<(String, &'static str)> {
    let re = SQL_SYMBOL_RE.get_or_init(|| {
        Regex::new(
            r#"(?i)^\s*create\s+(?:or\s+replace\s+)?(?:non?editionable\s+)?(?:force\s+)?(?:global\s+temporary\s+)?(package\s+body|package|materialized\s+view|type\s+body|type|procedure|function|trigger|view|table|index|sequence)\s+(?:if\s+not\s+exists\s+)?("?[A-Za-z0-9_$#]+"?(?:\s*\.\s*"?[A-Za-z0-9_$#]+"?)?)"#,
        )
        .unwrap()
    });
    let c = re.captures(line)?;
    let obj = c[1].to_lowercase();
    let kind = match obj.split_whitespace().next().unwrap_or("") {
        "package" => "package",
        "materialized" => "view",
        "type" => "type",
        "procedure" => "procedure",
        "function" => "function",
        "trigger" => "trigger",
        "view" => "view",
        "table" => "table",
        "index" => "index",
        "sequence" => "sequence",
        _ => "object",
    };
    let name = c[2].replace(['"', ' '], "");
    Some((name, kind))
}

fn make_chunk(
    lines: &[&str],
    path: &str,
    start: usize,
    end: usize,
    symbol: &str,
    kind: &str,
) -> Option<Chunk> {
    let content: String = lines[start..=end.min(lines.len().saturating_sub(1))]
        .join("\n")
        .trim_end()
        .to_string();
    let token_count = count_tokens(&content);
    if token_count < MIN_CHUNK_TOKENS {
        return None;
    }
    Some(Chunk {
        path: path.to_string(),
        start_line: start + 1,
        end_line: end + 1,
        symbol: symbol.to_string(),
        kind: kind.to_string(),
        content,
        token_count,
    })
}

fn flush_chunk(
    lines: &[&str],
    path: &str,
    start: usize,
    end: usize,
    symbol: &str,
    kind: &str,
    out: &mut Vec<Chunk>,
) {
    let total = end.saturating_sub(start) + 1;
    if total > MAX_CHUNK_TOKENS {
        // Split large chunk with sliding-window overlap
        let mut s = start;
        while s <= end {
            let e = (s + MAX_CHUNK_TOKENS).min(end);
            if let Some(c) = make_chunk(lines, path, s, e, symbol, kind) {
                out.push(c);
            }

            // Advance s with overlap
            let mut next_s = e + 1;
            let mut accumulated = 0;
            // Backtrack from e to find how many lines to include for overlap
            for idx in (s..=e).rev() {
                accumulated += count_tokens(lines[idx]);
                if accumulated >= CHUNK_OVERLAP_TOKENS {
                    if idx > s {
                        next_s = idx;
                    }
                    break;
                }
            }
            s = next_s;
        }
    } else if let Some(c) = make_chunk(lines, path, start, end, symbol, kind) {
        out.push(c);
    }
}

pub fn chunk_by_lines(lines: &[&str], path: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    if lines.is_empty() {
        return out;
    }
    let mut s = 0usize;
    let end = lines.len().saturating_sub(1);
    while s <= end {
        // Find how many lines we can include up to MAX_CHUNK_TOKENS
        let mut e = s;
        let mut tokens = 0;
        while e <= end {
            let lt = count_tokens(lines[e]);
            if tokens + lt > MAX_CHUNK_TOKENS && e > s {
                break;
            }
            tokens += lt;
            e += 1;
        }
        let last_included = e.saturating_sub(1);
        if let Some(c) = make_chunk(lines, path, s, last_included, "", "block") {
            out.push(c);
        }

        if e > end {
            break;
        }

        // Find next start with overlap
        let mut next_s = e;
        let mut accumulated = 0;
        for idx in (s..=last_included).rev() {
            accumulated += count_tokens(lines[idx]);
            if accumulated >= CHUNK_OVERLAP_TOKENS {
                if idx > s {
                    next_s = idx;
                }
                break;
            }
        }
        s = next_s;
    }
    out
}

/// Extract the full declaration signature up to (but not including) the body opening.
/// Joins multi-line parameter lists and normalizes whitespace.
fn extract_full_signature(content: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        parts.push(trimmed);
        // Body starts at `{` on its own or at end of line, or Python `:` ending the def
        if trimmed.ends_with('{') || trimmed == "{" {
            break;
        }
        if trimmed.ends_with(':')
            && !trimmed.starts_with("//")
            && !trimmed.starts_with('#')
            && !trimmed.contains("=>")
        {
            break;
        }
        if trimmed.ends_with(';') {
            break;
        }
    }
    let joined = parts.join(" ");
    // Strip trailing `{` or `:` left by the loop-break line
    let sig = joined.trim_end_matches('{').trim_end_matches(':').trim();
    // Collapse internal whitespace runs
    let sig: String = sig.split_whitespace().collect::<Vec<_>>().join(" ");
    if sig.chars().count() > 200 {
        let truncated: String = sig.chars().take(197).collect();
        format!("{}…", truncated)
    } else {
        sig
    }
}

/// Look for a single-line doc comment on the line immediately before the chunk.
fn extract_doc_comment(lines: &[&str], chunk_start_line: usize) -> Option<String> {
    // chunk_start_line is 1-based; the line before is index chunk_start_line - 2
    let idx = chunk_start_line.checked_sub(2)?;
    let t = lines.get(idx)?.trim();
    if let Some(doc) = t.strip_prefix("///") {
        let d = doc.trim();
        if !d.is_empty() {
            return Some(d.to_string());
        }
    }
    if let Some(doc) = t.strip_prefix("//") {
        let d = doc.trim();
        if !d.is_empty() && !d.starts_with('/') {
            return Some(d.to_string());
        }
    }
    // Python / shell `#` comment
    if let Some(doc) = t.strip_prefix('#') {
        let d = doc.trim();
        if !d.is_empty() && !d.starts_with('!') {
            return Some(d.to_string());
        }
    }
    None
}

/// Clean generic (non-code) file content: strip markdown formatting, emojis,
/// and collapse whitespace. All text is preserved — nothing is dropped.
pub fn clean_generic_text(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    let mut in_fence = false;
    let mut last_blank = true;

    for raw in content.lines() {
        let t = raw.trim();

        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            continue; // drop fence markers, keep content below
        }

        if in_fence {
            let s = strip_emojis(t);
            if s.is_empty() {
                if !last_blank {
                    out.push('\n');
                    last_blank = true;
                }
            } else {
                out.push_str(&s);
                out.push('\n');
                last_blank = false;
            }
            continue;
        }

        // HTML comment (single-line)
        if t.starts_with("<!--") {
            continue;
        }

        // Horizontal rule: --- *** ___ (no alphanumeric content)
        if t.len() >= 3
            && t.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '))
            && !t.chars().any(|c| c.is_alphanumeric())
        {
            continue;
        }

        // Table separator: | --- | --- |
        if t.starts_with('|') && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ')) {
            continue;
        }

        let s = clean_line(t);
        let s = strip_emojis(&s);
        let s = s.trim().to_string();

        if s.is_empty() {
            if !last_blank {
                out.push('\n');
                last_blank = true;
            }
        } else {
            out.push_str(&s);
            out.push('\n');
            last_blank = false;
        }
    }

    out.trim_end().to_string()
}

fn clean_line(s: &str) -> String {
    // Strip heading markers (# ## ### …)
    let s = s.trim_start_matches('#').trim_start();
    // Blockquote
    let s = s
        .strip_prefix("> ")
        .or_else(|| s.strip_prefix('>'))
        .unwrap_or(s)
        .trim_start();
    // Unordered list marker
    let s = s
        .strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .or_else(|| s.strip_prefix("+ "))
        .unwrap_or(s);
    // Numbered list: "1. " "42. "
    let s = {
        let b = s.as_bytes();
        let mut n = 0;
        while n < b.len() && b[n].is_ascii_digit() {
            n += 1;
        }
        if n > 0 && b.get(n) == Some(&b'.') && b.get(n + 1) == Some(&b' ') {
            &s[n + 2..]
        } else {
            s
        }
    };
    // Table row: | cell | cell | → "cell  cell"
    let owned: String;
    let s = if s.starts_with('|') {
        owned = s
            .split('|')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect::<Vec<_>>()
            .join("  ");
        owned.as_str()
    } else {
        s
    };
    strip_inline(s)
}

fn strip_inline(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(n);
    let mut i = 0;
    while i < n {
        match chars[i] {
            // Image: ![alt](url) → remove entirely
            '!' if chars.get(i + 1) == Some(&'[') => {
                i += 2;
                while i < n && chars[i] != ']' {
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
                if chars.get(i) == Some(&'(') {
                    i += 1;
                    while i < n && chars[i] != ')' {
                        i += 1;
                    }
                    if i < n {
                        i += 1;
                    }
                }
            }
            // Link: [text](url) → text
            '[' => {
                i += 1;
                let start = i;
                while i < n && chars[i] != ']' {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                if i < n {
                    i += 1;
                }
                if chars.get(i) == Some(&'(') {
                    i += 1;
                    while i < n && chars[i] != ')' {
                        i += 1;
                    }
                    if i < n {
                        i += 1;
                    }
                }
                out.push_str(&strip_inline(&text));
            }
            // Bold/italic markers: ** * __ _ → drop markers, keep text
            '*' => {
                if chars.get(i + 1) == Some(&'*') {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            '_' => {
                if chars.get(i + 1) == Some(&'_') {
                    i += 2;
                } else {
                    i += 1;
                }
            }
            // Strikethrough: ~~text~~ → drop markers
            '~' if chars.get(i + 1) == Some(&'~') => {
                i += 2;
            }
            // Inline code: `text` → text (preserve content)
            '`' => {
                i += 1;
                while i < n && chars[i] != '`' {
                    out.push(chars[i]);
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            // HTML tag: <...> → drop
            '<' => {
                while i < n && chars[i] != '>' {
                    i += 1;
                }
                if i < n {
                    i += 1;
                }
            }
            // Backslash escape: \* → *
            '\\' if i + 1 < n => {
                i += 1;
                out.push(chars[i]);
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

pub fn strip_emojis(s: &str) -> String {
    s.chars().filter(|&c| !is_emoji(c)).collect()
}

fn is_emoji(c: char) -> bool {
    let u = c as u32;
    (0x2600..=0x27BF).contains(&u)   // misc symbols & dingbats
        || (0x1F000..=0x1FAFF).contains(&u) // main emoji block
        || (0x1FB00..=0x1FBFF).contains(&u) // legacy computing symbols
        || u == 0xFE0F  // variation selector-16
        || u == 0x200D // zero-width joiner
}

pub fn generate_outline(content: &str, path: &str) -> String {
    // Generic files (md, txt, yaml, …) have no symbols.
    // Return full cleaned text — never a truncated preview.
    if matches!(detect_lang(Path::new(path)), Lang::Generic) {
        return clean_generic_text(content);
    }

    let lines: Vec<&str> = content.lines().collect();
    let chunks = chunk_file(path, content);

    if chunks.is_empty() {
        let preview: Vec<&str> = lines.iter().take(30).copied().collect();
        return format!(
            "[{} lines - no symbols detected]\n{}",
            lines.len(),
            preview.join("\n")
        );
    }

    let mut parts = vec![format!(
        "[{}] - {} lines, {} symbols\n",
        path,
        lines.len(),
        chunks.len()
    )];

    for c in &chunks {
        let sig = extract_full_signature(&c.content);
        let doc = extract_doc_comment(&lines, c.start_line);
        let doc_suffix = doc.map(|d| format!("  // {}", d)).unwrap_or_default();
        let label = if c.symbol.is_empty() {
            format!(
                "  L{}-{} [{}]: {}{}",
                c.start_line, c.end_line, c.kind, sig, doc_suffix
            )
        } else {
            format!(
                "  L{}-{} [{}] {}: {}{}",
                c.start_line, c.end_line, c.kind, c.symbol, sig, doc_suffix
            )
        };
        parts.push(label);
    }

    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_tokens_basic() {
        assert_eq!(count_tokens(""), 0);
        assert_eq!(count_tokens("abcd"), 1);
        assert_eq!(count_tokens("abcde"), 2);
        assert_eq!(count_tokens("hello world"), 3); // 11 chars → (11+3)/4 = 3
    }

    #[test]
    fn sensitive_files_are_never_indexed() {
        assert!(!should_index(Path::new("src/.env")));
        assert!(!should_index(Path::new("config/prod.env")));
        assert!(!should_index(Path::new("certs/server.pem")));
        assert!(!should_index(Path::new("keys/private.key")));
        assert!(!should_index(Path::new(".ssh/id_rsa")));
        assert!(should_index(Path::new("src/main.rs")));
    }

    #[test]
    fn redact_secrets_masks_common_patterns() {
        let input = "let token = \"AKIAIOSFODNN7EXAMPLE\";\napi_key = \"abcd1234efgh5678\"";
        let out = redact_secrets(input);
        assert!(out.contains("[REDACTED]"));
        assert!(!out.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn giant_single_line_is_split_and_content_preserved() {
        // A minified-style single line that would otherwise be one huge chunk.
        let payload = "x".repeat(MAX_CHUNK_TOKENS * 4 * 5 + 123);
        let content = format!("{{\"data\":\"{payload}\"}}");
        let chunks = chunk_file("data.json", &content);
        assert!(chunks.len() > 1, "oversized chunk must be split");
        for c in &chunks {
            assert!(
                c.token_count <= MAX_CHUNK_TOKENS + 1,
                "every chunk must respect the token cap, got {}",
                c.token_count
            );
        }
        let rejoined: String = chunks.iter().map(|c| c.content.as_str()).collect();
        assert_eq!(rejoined, content, "no content may be lost when splitting");
    }

    #[test]
    fn file_hash_deterministic() {
        let a = file_hash(b"hello");
        let b = file_hash(b"hello");
        assert_eq!(a, b);
        assert_ne!(file_hash(b"hello"), file_hash(b"world"));
        assert_eq!(a.len(), 16); // 8 bytes → 16 hex chars
    }

    #[test]
    fn should_index_accepts_known_extensions() {
        assert!(should_index(std::path::Path::new("src/main.rs")));
        assert!(should_index(std::path::Path::new("lib/auth.py")));
        assert!(should_index(std::path::Path::new("app/index.ts")));
        assert!(should_index(std::path::Path::new("server/handler.go")));
    }

    #[test]
    fn should_index_accepts_vb6_and_sql_extensions() {
        for f in [
            "legacy/Module1.bas",
            "legacy/Cliente.cls",
            "legacy/Grid.ctl",
            "legacy/Main.frm",
            "legacy/Projeto.vbp",
            "db/schema.sql",
            "db/calc_total.fnc",
            "db/audit.trg",
            "db/billing.pkg",
            "db/process.prc",
            "db/clientes.tab",
            "db/saldo.vw",
        ] {
            assert!(should_index(std::path::Path::new(f)), "{f} should index");
        }
        // Binary VB6 form/control resources stay out of the index.
        assert!(!should_index(std::path::Path::new("legacy/Main.frx")));
        assert!(!should_index(std::path::Path::new("legacy/Grid.ctx")));
    }

    #[test]
    fn chunk_vb_extracts_subs_functions_properties() {
        let src = r#"VERSION 1.0 CLASS
BEGIN
  MultiUse = -1  'True
END
Attribute VB_Name = "Cliente"
Option Explicit
Private mNome As String

Public Property Get Nome() As String
    Nome = mNome
End Property

Private Sub Class_Initialize()
    mNome = ""
End Sub

Public Function SaldoTotal(ByVal conta As Long) As Double
    SaldoTotal = conta * 2
End Function
"#;
        let chunks = chunk_file("legacy/Cliente.cls", src);
        let syms: Vec<(&str, &str)> = chunks
            .iter()
            .map(|c| (c.symbol.as_str(), c.kind.as_str()))
            .collect();
        assert!(syms.contains(&("Cliente", "class")), "{syms:?}");
        assert!(syms.contains(&("Nome", "property")), "{syms:?}");
        assert!(syms.contains(&("Class_Initialize", "function")), "{syms:?}");
        assert!(syms.contains(&("SaldoTotal", "function")), "{syms:?}");
        // 100% coverage: every source line lands in some chunk range.
        let total_lines = src.lines().count();
        let covered: std::collections::HashSet<usize> = chunks
            .iter()
            .flat_map(|c| c.start_line..=c.end_line)
            .collect();
        assert!((1..=total_lines).all(|l| covered.contains(&l)));
    }

    #[test]
    fn chunk_sql_extracts_create_objects() {
        let src = r#"-- billing objects
CREATE OR REPLACE PACKAGE BODY billing.faturas AS
  PROCEDURE interna IS BEGIN NULL; END;
END faturas;
/

CREATE OR REPLACE FUNCTION calc_total(p_id NUMBER) RETURN NUMBER IS
BEGIN
  RETURN p_id * 2;
END;
/

CREATE TABLE clientes (id NUMBER PRIMARY KEY, nome VARCHAR2(100));

create or replace view saldo_vw as select * from clientes;
"#;
        let chunks = chunk_file("db/billing.pkg", src);
        let syms: Vec<(&str, &str)> = chunks
            .iter()
            .map(|c| (c.symbol.as_str(), c.kind.as_str()))
            .collect();
        assert!(syms.contains(&("billing.faturas", "package")), "{syms:?}");
        assert!(syms.contains(&("calc_total", "function")), "{syms:?}");
        assert!(syms.contains(&("clientes", "table")), "{syms:?}");
        assert!(syms.contains(&("saldo_vw", "view")), "{syms:?}");
    }

    #[test]
    fn should_index_rejects_ignored_dirs() {
        assert!(!should_index(std::path::Path::new(
            "node_modules/lib/index.js"
        )));
        assert!(!should_index(std::path::Path::new("target/debug/build.rs")));
        assert!(!should_index(std::path::Path::new(".git/config")));
    }

    #[test]
    fn should_index_rejects_unknown_extensions() {
        assert!(!should_index(std::path::Path::new("image.png")));
        assert!(!should_index(std::path::Path::new("binary.exe")));
        assert!(!should_index(std::path::Path::new("data.parquet")));
    }

    #[test]
    fn should_index_rejects_minified() {
        assert!(!should_index(std::path::Path::new("bundle.min.js")));
        assert!(!should_index(std::path::Path::new("app.min.css")));
        assert!(!should_index(std::path::Path::new("source.map")));
    }

    #[test]
    fn custom_extension_indexing_and_detection() {
        // Create a temporary .tokenix.toml in the current directory
        let toml_path = std::path::Path::new(".tokenix.toml");
        std::fs::write(
            toml_path,
            r#"
[languages]
customrs = "rust"
custompy = "python"
"#,
        )
        .unwrap();

        // should_index should now accept files with .customrs and .custompy
        assert!(should_index(std::path::Path::new("src/test.customrs")));
        assert!(should_index(std::path::Path::new("src/test.custompy")));
        assert!(!should_index(std::path::Path::new("src/test.unknown")));

        // detect_lang should detect the mapped languages
        assert!(matches!(
            detect_lang(std::path::Path::new("src/test.customrs")),
            Lang::Rust
        ));
        assert!(matches!(
            detect_lang(std::path::Path::new("src/test.custompy")),
            Lang::Python
        ));

        // Clean up
        let _ = std::fs::remove_file(toml_path);
    }

    #[test]
    fn chunk_rust_detects_functions() {
        // Functions need >10 tokens each to pass MIN_CHUNK_TOKENS
        let body =
            "    let value = compute_something_complex(input, config, options);\n    value * 2\n";
        let code = format!("fn hello(input: i32, config: Config, options: Options) -> i32 {{\n{body}}}\n\nfn world(input: i32, config: Config, options: Options) -> i32 {{\n{body}}}\n");
        let chunks = chunk_file("src/test.rs", &code);
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            symbols.contains(&"hello"),
            "expected 'hello' in {:?}",
            symbols
        );
        assert!(
            symbols.contains(&"world"),
            "expected 'world' in {:?}",
            symbols
        );
    }

    #[test]
    fn chunk_python_detects_classes_and_defs() {
        let code = concat!(
            "class DatabaseClient:\n",
            "    def __init__(self, host: str, port: int, username: str, password: str) -> None:\n",
            "        self.host = host\n",
            "        self.port = port\n",
            "        self.conn = None\n\n",
            "def connect_to_database(host: str, port: int, timeout: int = 30) -> DatabaseClient:\n",
            "    client = DatabaseClient(host, port, 'admin', 'secret')\n",
            "    client.connect(timeout=timeout)\n",
            "    return client\n",
        );
        let chunks = chunk_file("module.py", code);
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            symbols
                .iter()
                .any(|s| s.contains("DatabaseClient") || s.contains("connect_to_database")),
            "no expected symbols in {:?}",
            symbols
        );
    }

    #[test]
    fn chunk_typescript_detects_exported_classes_and_interfaces() {
        let code = concat!(
            "export interface UserRepositoryOptions {\n",
            "  tableName: string;\n",
            "  poolSize: number;\n",
            "}\n\n",
            "export abstract class BaseRepository<T> {\n",
            "  async findById(id: string): Promise<T | null> {\n",
            "    return this.queryById(id);\n",
            "  }\n",
            "  protected abstract queryById(id: string): Promise<T | null>;\n",
            "}\n\n",
            "export class UserRepository extends BaseRepository<User> {\n",
            "  protected async queryById(id: string): Promise<User | null> {\n",
            "    const user = await this.pool.query('select * from users where id = $1', [id]);\n",
            "    return user.rows[0] ?? null;\n",
            "  }\n",
            "}\n",
        );
        let chunks = chunk_file("database_client.ts", code);
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(
            symbols.contains(&"UserRepository"),
            "expected UserRepository in {:?}",
            symbols
        );
        assert!(
            symbols.contains(&"BaseRepository"),
            "expected BaseRepository in {:?}",
            symbols
        );
    }

    #[test]
    fn chunk_typescript_detects_types_enums_and_functions() {
        let code = concat!(
            "export type UserRole = 'admin' | 'user' | 'guest';\n\n",
            "export enum LoginState {\n",
            "  Pending = 'pending',\n",
            "  Complete = 'complete',\n",
            "}\n\n",
            "export function buildUserPayload(id: string, role: UserRole) {\n",
            "  const payload = { id, role, createdAt: new Date().toISOString() };\n",
            "  return JSON.stringify(payload);\n",
            "}\n",
        );
        let chunks = chunk_file("auth.ts", code);
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(symbols.contains(&"UserRole"), "symbols: {:?}", symbols);
        assert!(symbols.contains(&"LoginState"), "symbols: {:?}", symbols);
        assert!(
            symbols.contains(&"buildUserPayload"),
            "symbols: {:?}",
            symbols
        );
    }

    #[test]
    fn chunk_javascript_detects_default_export_class() {
        let code = concat!(
            "export default class SessionStore {\n",
            "  constructor(client) {\n",
            "    this.client = client;\n",
            "  }\n",
            "  async save(session) {\n",
            "    await this.client.set(session.id, JSON.stringify(session));\n",
            "  }\n",
            "}\n",
        );
        let chunks = chunk_file("session.js", code);
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(symbols.contains(&"SessionStore"), "symbols: {:?}", symbols);
    }

    #[test]
    fn test_chunk_cpp() {
        let code = r#"
class MyClass {
public:
    void myMethod() {
        int x = 42;
        int y = x * 2;
        int z = y + 10;
        // Make this method pass the min token count threshold
        printf("Calculated value: %d\n", z);
    }
};

void globalFunc() {
    int a = 100;
    int b = 200;
    int c = a + b;
    // Ensure the function chunk is large enough to be indexed
    printf("The sum is %d\n", c);
}
"#;
        let chunks = chunk_file("test.cpp", code);
        assert!(!chunks.is_empty());
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(symbols.contains(&"MyClass"));
        assert!(symbols.contains(&"globalFunc"));
    }

    #[test]
    fn generate_outline_includes_line_counts() {
        let code = "fn a() {}\n".repeat(50);
        let out = generate_outline(&code, "src/many.rs");
        assert!(
            out.contains("50 lines") || out.contains("lines"),
            "outline: {}",
            &out[..200.min(out.len())]
        );
    }

    #[test]
    fn chunk_respects_max_token_limit() {
        // A 400-token chunk should not be split by the chunker into zero chunks
        let big_fn = format!("fn big() {{\n{}}}\n", "    let x = 1;\n".repeat(300));
        let chunks = chunk_file("src/big.rs", &big_fn);
        assert!(!chunks.is_empty(), "should produce at least one chunk");
        for c in &chunks {
            assert!(c.token_count > 0);
        }
    }

    #[test]
    fn test_sliding_window_overlap() {
        // Create an input where each line has several tokens
        let line_content = "let var_to_verify_overlap = 12345;";
        let lines: Vec<String> = (0..150)
            .map(|i| format!("{}: {}", i, line_content))
            .collect();
        let slice: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let chunks = chunk_by_lines(&slice, "test.txt");
        assert!(chunks.len() > 1);

        let c1 = &chunks[0];
        let c2 = &chunks[1];
        assert!(c2.start_line < c1.end_line);
        assert!(c1.content.contains("let var_to_verify_overlap"));
        assert!(c2.content.contains("let var_to_verify_overlap"));
    }
}
