#![no_main]

use libfuzzer_sys::fuzz_target;

mod store {
    use std::path::{Path, PathBuf};

    pub fn find_project_root(start: &Path) -> PathBuf {
        start.to_path_buf()
    }
}

#[path = "../../src/chunker.rs"]
mod chunker;

fn path_for(selector: u8) -> &'static str {
    match selector % 8 {
        0 => "src/main.rs",
        1 => "src/tool.ts",
        2 => "scripts/task.py",
        3 => "internal/worker.go",
        4 => "native/parser.cpp",
        5 => "schema/query.sql",
        6 => "docs/notes.md",
        _ => "config/tokenix.toml",
    }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }

    let path = path_for(data[0]);
    let end = data.len().min(64 * 1024);
    let content = String::from_utf8_lossy(&data[1..end]);

    let _ = chunker::redact_secrets(&content);
    let _ = chunker::clean_generic_text(&content);
    let chunks = chunker::chunk_file(path, &content);
    for chunk in &chunks {
        assert!(chunk.token_count <= chunker::MAX_CHUNK_TOKENS);
        assert!(!chunk.content.is_empty());
    }
    let _ = chunker::generate_outline(&content, path);
});
