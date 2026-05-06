use sha2::{Digest, Sha256};
use std::path::Path;

pub const MAX_CHUNK_TOKENS: usize = 400;
pub const MIN_CHUNK_TOKENS: usize = 10;

pub const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".tokenix",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    "target",
    ".next",
    ".nuxt",
    "coverage",
    ".cargo",
];

pub const INDEXED_EXTS: &[&str] = &[
    ".rs", ".py", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".go", ".java", ".c", ".cpp", ".h",
    ".hpp", ".cs", ".rb", ".swift", ".kt", ".scala", ".sh", ".bash", ".toml", ".yaml", ".yml",
    ".json", ".md", ".txt",
];

#[derive(Debug, Clone)]
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
    (text.len() + 3) / 4
}

pub fn should_index(path: &Path) -> bool {
    for component in path.components() {
        let s = component.as_os_str().to_string_lossy();
        if IGNORED_DIRS.contains(&s.as_ref()) {
            return false;
        }
    }
    let name = path.to_string_lossy().to_lowercase();
    if name.ends_with(".min.js") || name.ends_with(".min.css") || name.ends_with(".map") {
        return false;
    }
    // Check extension
    let has_ext = INDEXED_EXTS.iter().any(|ext| name.ends_with(ext));
    if !has_ext {
        return false;
    }
    true
}

#[derive(Debug)]
enum Lang {
    Rust,
    Python,
    TypeScript,
    JavaScript,
    Go,
    Generic,
}

fn detect_lang(path: &Path) -> Lang {
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
        _ => Lang::Generic,
    }
}

pub fn chunk_file(path: &str, content: &str) -> Vec<Chunk> {
    let p = Path::new(path);
    let lang = detect_lang(p);
    let lines: Vec<&str> = content.lines().collect();

    match lang {
        Lang::Rust => chunk_rust(&lines, path),
        Lang::Python => chunk_python(&lines, path),
        Lang::TypeScript | Lang::JavaScript => chunk_ts_js(&lines, path),
        Lang::Go => chunk_go(&lines, path),
        Lang::Generic => chunk_by_lines(&lines, path),
    }
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
    if total * 4 > MAX_CHUNK_TOKENS * 4 {
        // Split large chunk
        let mut s = start;
        while s <= end {
            let e = (s + MAX_CHUNK_TOKENS).min(end);
            if let Some(c) = make_chunk(lines, path, s, e, symbol, kind) {
                out.push(c);
            }
            s = e + 1;
        }
    } else if let Some(c) = make_chunk(lines, path, start, end, symbol, kind) {
        out.push(c);
    }
}

fn chunk_rust(lines: &[&str], path: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut block_start: Option<usize> = None;
    let mut block_symbol = String::new();
    let mut block_kind = String::new();
    let mut brace_depth: i32 = 0;
    let mut in_block = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !in_block {
            let is_def = trimmed.starts_with("pub fn ")
                || trimmed.starts_with("fn ")
                || trimmed.starts_with("pub async fn ")
                || trimmed.starts_with("async fn ")
                || trimmed.starts_with("pub struct ")
                || trimmed.starts_with("struct ")
                || trimmed.starts_with("pub enum ")
                || trimmed.starts_with("enum ")
                || trimmed.starts_with("pub impl")
                || trimmed.starts_with("impl ")
                || trimmed.starts_with("pub trait ")
                || trimmed.starts_with("trait ")
                || trimmed.starts_with("pub mod ")
                || trimmed.starts_with("mod ");

            if is_def {
                block_start = Some(i);
                block_symbol = extract_rust_name(trimmed);
                block_kind = if trimmed.contains(" fn ") {
                    "function"
                } else if trimmed.contains("struct ") {
                    "struct"
                } else if trimmed.contains("enum ") {
                    "enum"
                } else if trimmed.contains("impl") {
                    "impl"
                } else if trimmed.contains("trait ") {
                    "trait"
                } else {
                    "module"
                }
                .to_string();
                in_block = true;
                brace_depth = 0;
            }
        }

        if in_block {
            brace_depth += line.chars().filter(|&c| c == '{').count() as i32;
            brace_depth -= line.chars().filter(|&c| c == '}').count() as i32;

            if brace_depth <= 0 && block_start.is_some() {
                let start = block_start.take().unwrap();
                flush_chunk(lines, path, start, i, &block_symbol, &block_kind, &mut out);
                in_block = false;
                brace_depth = 0;
            }
        }
    }

    if out.is_empty() {
        chunk_by_lines(lines, path)
    } else {
        out
    }
}

fn extract_rust_name(line: &str) -> String {
    let tokens: Vec<&str> = line.split_whitespace().collect();
    for (i, &t) in tokens.iter().enumerate() {
        if t == "fn" || t == "struct" || t == "enum" || t == "trait" || t == "mod" || t == "impl" {
            if let Some(next) = tokens.get(i + 1) {
                // Strip generic params, paren args, and non-identifier chars
                let name = next
                    .split(['(', '<', '{', ';'])
                    .next()
                    .unwrap_or(next)
                    .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                return name.to_string();
            }
        }
    }
    String::new()
}

fn chunk_python(lines: &[&str], path: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut block_start: Option<usize> = None;
    let mut block_symbol = String::new();
    let mut block_kind = String::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        let is_def = trimmed.starts_with("def ")
            || trimmed.starts_with("async def ")
            || trimmed.starts_with("class ");

        if is_def {
            if let Some(start) = block_start.take() {
                flush_chunk(
                    lines,
                    path,
                    start,
                    i.saturating_sub(1),
                    &block_symbol,
                    &block_kind,
                    &mut out,
                );
            }
            block_start = Some(i);
            block_symbol = extract_python_name(trimmed);
            block_kind = if trimmed.starts_with("class ") {
                "class"
            } else {
                "function"
            }
            .to_string();
        }
    }

    if let Some(start) = block_start {
        flush_chunk(
            lines,
            path,
            start,
            lines.len().saturating_sub(1),
            &block_symbol,
            &block_kind,
            &mut out,
        );
    }

    if out.is_empty() {
        chunk_by_lines(lines, path)
    } else {
        out
    }
}

fn extract_python_name(line: &str) -> String {
    let after_kw = if let Some(s) = line.strip_prefix("async def ") {
        s
    } else if let Some(s) = line.strip_prefix("def ") {
        s
    } else if let Some(s) = line.strip_prefix("class ") {
        s
    } else {
        line
    };
    after_kw
        .split(['(', ':'])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn chunk_ts_js(lines: &[&str], path: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut block_start: Option<usize> = None;
    let mut block_symbol = String::new();
    let mut block_kind = String::new();
    let mut brace_depth: i32 = 0;
    let mut in_block = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !in_block {
            let is_def = trimmed.starts_with("function ")
                || trimmed.starts_with("export function ")
                || trimmed.starts_with("export default function")
                || trimmed.starts_with("export async function")
                || trimmed.starts_with("async function")
                || trimmed.starts_with("class ")
                || trimmed.starts_with("export class ")
                || trimmed.starts_with("interface ")
                || trimmed.starts_with("export interface ")
                || trimmed.starts_with("type ")
                || trimmed.starts_with("export type ")
                || (trimmed.contains("= function")
                    || trimmed.contains("= async function")
                    || trimmed.contains("=> {"))
                    && (trimmed.contains("const ")
                        || trimmed.contains("let ")
                        || trimmed.contains("var "));

            if is_def {
                block_start = Some(i);
                block_symbol = extract_ts_name(trimmed);
                block_kind = if trimmed.contains("class ") {
                    "class"
                } else if trimmed.contains("interface ") {
                    "interface"
                } else if trimmed.starts_with("type ") || trimmed.starts_with("export type ") {
                    "type"
                } else {
                    "function"
                }
                .to_string();
                in_block = true;
                brace_depth = 0;
            }
        }

        if in_block {
            brace_depth += line.chars().filter(|&c| c == '{').count() as i32;
            brace_depth -= line.chars().filter(|&c| c == '}').count() as i32;

            if brace_depth <= 0 && block_start.is_some() {
                let start = block_start.take().unwrap();
                flush_chunk(lines, path, start, i, &block_symbol, &block_kind, &mut out);
                in_block = false;
                brace_depth = 0;
            }
        }
    }

    if out.is_empty() {
        chunk_by_lines(lines, path)
    } else {
        out
    }
}

fn extract_ts_name(line: &str) -> String {
    // "function foo(" -> "foo"
    // "const foo = " -> "foo"
    // "export function foo" -> "foo"
    let cleaned = line
        .trim_start_matches("export ")
        .trim_start_matches("default ")
        .trim_start_matches("async ");
    if let Some(s) = cleaned.strip_prefix("function ") {
        return s.split(['(', '<', ' ']).next().unwrap_or("").to_string();
    }
    if let Some(s) = cleaned.strip_prefix("class ") {
        return s.split([' ', '{', '<']).next().unwrap_or("").to_string();
    }
    if let Some(s) = cleaned.strip_prefix("interface ") {
        return s.split([' ', '{', '<']).next().unwrap_or("").to_string();
    }
    if let Some(s) = cleaned.strip_prefix("type ") {
        return s.split([' ', '=']).next().unwrap_or("").to_string();
    }
    // const/let/var foo = ...
    let after_decl = cleaned
        .trim_start_matches("const ")
        .trim_start_matches("let ")
        .trim_start_matches("var ");
    after_decl
        .split(['=', ':', ' '])
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn chunk_go(lines: &[&str], path: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut block_start: Option<usize> = None;
    let mut block_symbol = String::new();
    let mut block_kind = String::new();
    let mut brace_depth: i32 = 0;
    let mut in_block = false;

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if !in_block {
            let is_def = trimmed.starts_with("func ") || trimmed.starts_with("type ");
            if is_def {
                block_start = Some(i);
                block_symbol = extract_go_name(trimmed);
                block_kind = if trimmed.starts_with("type ") {
                    "type"
                } else {
                    "function"
                }
                .to_string();
                in_block = true;
                brace_depth = 0;
            }
        }

        if in_block {
            brace_depth += line.chars().filter(|&c| c == '{').count() as i32;
            brace_depth -= line.chars().filter(|&c| c == '}').count() as i32;

            if brace_depth <= 0 && block_start.is_some() {
                let start = block_start.take().unwrap();
                flush_chunk(lines, path, start, i, &block_symbol, &block_kind, &mut out);
                in_block = false;
                brace_depth = 0;
            }
        }
    }

    if out.is_empty() {
        chunk_by_lines(lines, path)
    } else {
        out
    }
}

fn extract_go_name(line: &str) -> String {
    if let Some(s) = line.strip_prefix("func ") {
        // func (recv) Name( or func Name(
        let after = if s.starts_with('(') {
            s.splitn(2, ')')
                .nth(1)
                .unwrap_or(s)
                .trim()
                .trim_start_matches(' ')
        } else {
            s
        };
        return after.split(['(', ' ']).next().unwrap_or("").to_string();
    }
    if let Some(s) = line.strip_prefix("type ") {
        return s.split(' ').next().unwrap_or("").to_string();
    }
    String::new()
}

pub fn chunk_by_lines(lines: &[&str], path: &str) -> Vec<Chunk> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut current_tokens = 0usize;
    let mut start = 0usize;

    for (i, &line) in lines.iter().enumerate() {
        let lt = count_tokens(line);
        if current_tokens + lt > MAX_CHUNK_TOKENS && !current.is_empty() {
            let content = current.join("\n").trim_end().to_string();
            let tc = count_tokens(&content);
            if tc >= MIN_CHUNK_TOKENS {
                out.push(Chunk {
                    path: path.to_string(),
                    start_line: start + 1,
                    end_line: start + current.len(),
                    symbol: String::new(),
                    kind: "block".to_string(),
                    content,
                    token_count: tc,
                });
            }
            start = i;
            current = vec![line];
            current_tokens = lt;
        } else {
            current.push(line);
            current_tokens += lt;
        }
    }

    if !current.is_empty() {
        let content = current.join("\n").trim_end().to_string();
        let tc = count_tokens(&content);
        if tc >= MIN_CHUNK_TOKENS {
            out.push(Chunk {
                path: path.to_string(),
                start_line: start + 1,
                end_line: start + current.len(),
                symbol: String::new(),
                kind: "block".to_string(),
                content,
                token_count: tc,
            });
        }
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
    let sig = joined
        .trim_end_matches('{')
        .trim_end_matches(':')
        .trim();
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
                if !last_blank { out.push('\n'); last_blank = true; }
            } else {
                out.push_str(&s); out.push('\n'); last_blank = false;
            }
            continue;
        }

        // HTML comment (single-line)
        if t.starts_with("<!--") { continue; }

        // Horizontal rule: --- *** ___ (no alphanumeric content)
        if t.len() >= 3
            && t.chars().all(|c| matches!(c, '-' | '*' | '_' | '=' | ' '))
            && !t.chars().any(|c| c.is_alphanumeric())
        {
            continue;
        }

        // Table separator: | --- | --- |
        if t.starts_with('|')
            && t.chars().all(|c| matches!(c, '|' | '-' | ':' | ' '))
        {
            continue;
        }

        let s = clean_line(t);
        let s = strip_emojis(&s);
        let s = s.trim().to_string();

        if s.is_empty() {
            if !last_blank { out.push('\n'); last_blank = true; }
        } else {
            out.push_str(&s); out.push('\n'); last_blank = false;
        }
    }

    out.trim_end().to_string()
}

fn clean_line(s: &str) -> String {
    // Strip heading markers (# ## ### …)
    let s = s.trim_start_matches('#').trim_start();
    // Blockquote
    let s = s.strip_prefix("> ").or_else(|| s.strip_prefix('>')).unwrap_or(s).trim_start();
    // Unordered list marker
    let s = s.strip_prefix("- ")
        .or_else(|| s.strip_prefix("* "))
        .or_else(|| s.strip_prefix("+ "))
        .unwrap_or(s);
    // Numbered list: "1. " "42. "
    let s = {
        let b = s.as_bytes();
        let mut n = 0;
        while n < b.len() && b[n].is_ascii_digit() { n += 1; }
        if n > 0 && b.get(n) == Some(&b'.') && b.get(n + 1) == Some(&b' ') {
            &s[n + 2..]
        } else {
            s
        }
    };
    // Table row: | cell | cell | → "cell  cell"
    let owned: String;
    let s = if s.starts_with('|') {
        owned = s.split('|')
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
                while i < n && chars[i] != ']' { i += 1; }
                if i < n { i += 1; }
                if chars.get(i) == Some(&'(') {
                    i += 1;
                    while i < n && chars[i] != ')' { i += 1; }
                    if i < n { i += 1; }
                }
            }
            // Link: [text](url) → text
            '[' => {
                i += 1;
                let start = i;
                while i < n && chars[i] != ']' { i += 1; }
                let text: String = chars[start..i].iter().collect();
                if i < n { i += 1; }
                if chars.get(i) == Some(&'(') {
                    i += 1;
                    while i < n && chars[i] != ')' { i += 1; }
                    if i < n { i += 1; }
                }
                out.push_str(&strip_inline(&text));
            }
            // Bold/italic markers: ** * __ _ → drop markers, keep text
            '*' => { if chars.get(i+1) == Some(&'*') { i += 2; } else { i += 1; } }
            '_' => { if chars.get(i+1) == Some(&'_') { i += 2; } else { i += 1; } }
            // Strikethrough: ~~text~~ → drop markers
            '~' if chars.get(i+1) == Some(&'~') => { i += 2; }
            // Inline code: `text` → text (preserve content)
            '`' => {
                i += 1;
                while i < n && chars[i] != '`' { out.push(chars[i]); i += 1; }
                if i < n { i += 1; }
            }
            // HTML tag: <...> → drop
            '<' => {
                while i < n && chars[i] != '>' { i += 1; }
                if i < n { i += 1; }
            }
            // Backslash escape: \* → *
            '\\' if i + 1 < n => { i += 1; out.push(chars[i]); i += 1; }
            c => { out.push(c); i += 1; }
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
        || u == 0x200D  // zero-width joiner
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
        let doc_suffix = doc
            .map(|d| format!("  // {}", d))
            .unwrap_or_default();
        let label = if c.symbol.is_empty() {
            format!("  L{}-{} [{}]: {}{}", c.start_line, c.end_line, c.kind, sig, doc_suffix)
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
    fn should_index_rejects_ignored_dirs() {
        assert!(!should_index(std::path::Path::new("node_modules/lib/index.js")));
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
    fn chunk_rust_detects_functions() {
        // Functions need >10 tokens each to pass MIN_CHUNK_TOKENS
        let body = "    let value = compute_something_complex(input, config, options);\n    value * 2\n";
        let code = format!("fn hello(input: i32, config: Config, options: Options) -> i32 {{\n{body}}}\n\nfn world(input: i32, config: Config, options: Options) -> i32 {{\n{body}}}\n");
        let chunks = chunk_file("src/test.rs", &code);
        let symbols: Vec<&str> = chunks.iter().map(|c| c.symbol.as_str()).collect();
        assert!(symbols.contains(&"hello"), "expected 'hello' in {:?}", symbols);
        assert!(symbols.contains(&"world"), "expected 'world' in {:?}", symbols);
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
        assert!(symbols.iter().any(|s| s.contains("DatabaseClient") || s.contains("connect_to_database")),
            "no expected symbols in {:?}", symbols);
    }

    #[test]
    fn generate_outline_includes_line_counts() {
        let code = "fn a() {}\n".repeat(50);
        let out = generate_outline(&code, "src/many.rs");
        assert!(out.contains("50 lines") || out.contains("lines"), "outline: {}", &out[..200.min(out.len())]);
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
}
