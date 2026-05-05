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
                let name = next.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
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

pub fn generate_outline(content: &str, path: &str) -> String {
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
        let sig = c
            .content
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect::<String>();
        let label = if c.symbol.is_empty() {
            format!("  L{}-{} [{}]: {}", c.start_line, c.end_line, c.kind, sig)
        } else {
            format!(
                "  L{}-{} [{}] {}: {}",
                c.start_line, c.end_line, c.kind, c.symbol, sig
            )
        };
        parts.push(label);
    }

    parts.join("\n")
}
