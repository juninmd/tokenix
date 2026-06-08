use anyhow::{anyhow, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use once_cell::sync::OnceCell;
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

/// Friendly id of the model used when nothing is configured. Existing indexes were
/// built with this, so it stays the default to avoid forcing a re-index.
pub const DEFAULT_MODEL_ID: &str = "nomic-v1.5";

/// A selectable embedding model. `doc_prefix`/`query_prefix` are model-specific:
/// nomic uses `search_document:`/`search_query:`, e5 uses `passage:`/`query:`,
/// bge/minilm use none. Using the wrong prefix silently degrades retrieval.
pub struct ModelSpec {
    pub id: &'static str,
    pub model: EmbeddingModel,
    pub doc_prefix: &'static str,
    pub query_prefix: &'static str,
    pub note: &'static str,
}

/// Built-in ONNX models (run on the current fastembed/ORT backend, no extra deps).
pub const MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "nomic-v1.5",
        model: EmbeddingModel::NomicEmbedTextV15Q,
        doc_prefix: "search_document: ",
        query_prefix: "search_query: ",
        note: "768d · English · quantized · default",
    },
    // Non-quantized ONNX: the Qdrant-quantized bge/minilm graphs use a fused
    // SkipLayerNormalization op that the pinned ORT build cannot run (missing
    // input). The fp32 graphs load cleanly; they are larger to download but still
    // far fewer params than nomic, so indexing is faster.
    ModelSpec {
        id: "bge-small",
        model: EmbeddingModel::BGESmallENV15,
        doc_prefix: "",
        query_prefix: "",
        note: "384d · English · ~33M params, fast",
    },
    ModelSpec {
        id: "bge-base",
        model: EmbeddingModel::BGEBaseENV15,
        doc_prefix: "",
        query_prefix: "",
        note: "768d · English",
    },
    ModelSpec {
        id: "minilm-l6",
        model: EmbeddingModel::AllMiniLML6V2,
        doc_prefix: "",
        query_prefix: "",
        note: "384d · English · ~22M params, fastest, lower quality",
    },
    ModelSpec {
        id: "e5-small",
        model: EmbeddingModel::MultilingualE5Small,
        doc_prefix: "passage: ",
        query_prefix: "query: ",
        note: "384d · multilingual",
    },
];

/// Look up a model by id, falling back to the default for unknown ids so a bad
/// `TOKENIX_EMBED_MODEL` never breaks indexing/search (doctor surfaces the warning).
pub fn spec_for(id: &str) -> &'static ModelSpec {
    MODELS.iter().find(|m| m.id == id).unwrap_or(&MODELS[0])
}

pub fn is_known_model(id: &str) -> bool {
    MODELS.iter().any(|m| m.id == id)
}

thread_local! {
    /// Per-thread active model id. Set explicitly by the index path (from the
    /// chosen model) and by the query/hook path (from the index's stamped model),
    /// so the daemon's handler threads can serve different projects/models without
    /// racing on a shared global.
    static ACTIVE_MODEL: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Set the active embedding model for the current thread. Pass a friendly id from
/// [`MODELS`]; unknown ids resolve to the default.
pub fn set_active_model(id: &str) {
    let id = spec_for(id).id.to_string();
    ACTIVE_MODEL.with(|m| *m.borrow_mut() = Some(id));
}

/// Resolve the active model id: thread-local override, else `TOKENIX_EMBED_MODEL`,
/// else the default.
pub fn active_model_id() -> String {
    if let Some(id) = ACTIVE_MODEL.with(|m| m.borrow().clone()) {
        return id;
    }
    match std::env::var("TOKENIX_EMBED_MODEL") {
        Ok(id) if !id.trim().is_empty() => spec_for(id.trim()).id.to_string(),
        _ => DEFAULT_MODEL_ID.to_string(),
    }
}

/// Loaded models keyed by id. Leaked to `'static` so callers get a stable
/// reference; the process loads each model at most once.
static MODELS_CACHE: OnceCell<Mutex<HashMap<String, &'static TextEmbedding>>> = OnceCell::new();

/// When true, the GPU execution provider is skipped even on a GPU-enabled build.
/// Set by `main()` from the `--only-cpu` flag before the model is first used.
static FORCE_CPU: AtomicBool = AtomicBool::new(false);

/// Force CPU-only embedding. Must be called before the first embed call.
/// No-op on CPU-only builds (no GPU provider is compiled in).
pub fn set_force_cpu(force: bool) {
    FORCE_CPU.store(force, Ordering::Relaxed);
}

#[allow(dead_code)]
fn force_cpu() -> bool {
    FORCE_CPU.load(Ordering::Relaxed)
}

pub fn model_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("tokenix")
        .join("models")
}

static TOKENIZER: OnceCell<Option<tokenizers::Tokenizer>> = OnceCell::new();

/// Accurate token count using the model's own tokenizer (loaded from the
/// cached `tokenizer.json`). Falls back to the fast `count_tokens` heuristic if
/// the model has not been downloaded yet. Use this for budget decisions where
/// precision matters; the hot chunker path keeps the cheap approximation.
pub fn count_tokens_accurate(text: &str) -> usize {
    let tok = TOKENIZER.get_or_init(|| {
        let dir = model_cache_dir();
        let path = find_tokenizer_json(&dir)?;
        tokenizers::Tokenizer::from_file(path).ok()
    });
    match tok {
        Some(t) => t
            .encode(text, false)
            .map(|e| e.len())
            .unwrap_or_else(|_| crate::chunker::count_tokens(text)),
        None => crate::chunker::count_tokens(text),
    }
}

fn find_tokenizer_json(dir: &std::path::Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_tokenizer_json(&p) {
                return Some(found);
            }
        } else if p.file_name().is_some_and(|n| n == "tokenizer.json") {
            return Some(p);
        }
    }
    None
}

/// The GPU execution provider compiled into this binary, if any.
/// `None` means a CPU-only build (no GPU code is present).
pub fn gpu_backend() -> Option<&'static str> {
    #[cfg(feature = "cuda")]
    {
        Some("CUDA")
    }
    #[cfg(all(not(feature = "cuda"), feature = "directml", target_os = "windows"))]
    {
        Some("DirectML")
    }
    #[cfg(not(any(feature = "cuda", all(feature = "directml", target_os = "windows"))))]
    {
        None
    }
}

fn open_query_cache_db() -> Option<Connection> {
    let dir = dirs::home_dir()?.join(".tokenix");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("query_cache.db");
    let conn = Connection::open(&path).ok()?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .ok()?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS query_cache (
            query_text TEXT PRIMARY KEY,
            embedding BLOB NOT NULL
        )",
        [],
    )
    .ok()?;
    Some(conn)
}

fn serialize_vec(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|f| f.to_le_bytes()).collect()
}

fn deserialize_vec(bytes: &[u8]) -> Vec<f32> {
    bytes
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

/// Get (loading once) the model for a friendly id. The lock is held across the
/// load so two threads never load the same model twice; after first load it is a
/// brief map lookup.
fn model_for(id: &str) -> Result<&'static TextEmbedding> {
    let cache = MODELS_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = cache
        .lock()
        .map_err(|_| anyhow!("embedding model cache poisoned"))?;
    if let Some(m) = map.get(id) {
        return Ok(*m);
    }
    let te = build_text_embedding(spec_for(id).model.clone())
        .map_err(|e| anyhow!("Embedding model '{id}' init failed: {e}"))?;
    // Leak once: the model lives for the rest of the process anyway.
    let leaked: &'static TextEmbedding = Box::leak(Box::new(te));
    map.insert(id.to_string(), leaked);
    Ok(leaked)
}

fn build_text_embedding(model: EmbeddingModel) -> Result<TextEmbedding> {
    // OMP_NUM_THREADS=1: limits OpenMP thread count for non-Windows ORT builds.
    // On Windows, ORT prebuilt uses OpenMP; this var is set in main() before hook runs.
    // Set here too as belt-and-suspenders for indexer and query paths.
    #[allow(unused_unsafe)]
    if std::env::var("OMP_NUM_THREADS").is_err() {
        unsafe { std::env::set_var("OMP_NUM_THREADS", "1") };
    }
    let cache_dir = model_cache_dir();
    std::fs::create_dir_all(&cache_dir).ok();

    #[allow(unused_mut)]
    let mut options = InitOptions::new(model).with_cache_dir(cache_dir);

    // GPU-by-default with automatic CPU fallback. Register the GPU provider
    // first and CPU second, so ORT uses the GPU when available and falls back
    // to CPU otherwise. `--only-cpu` (FORCE_CPU) skips the GPU provider entirely.
    #[cfg(feature = "cuda")]
    if !force_cpu() {
        options = options.with_execution_providers(vec![
            ort::execution_providers::CUDAExecutionProvider::default().build(),
            ort::execution_providers::CPUExecutionProvider::default().build(),
        ]);
    }

    #[cfg(all(not(feature = "cuda"), feature = "directml"))]
    if !force_cpu() {
        options = options.with_execution_providers(vec![
            ort::execution_providers::DirectMLExecutionProvider::default().build(),
            ort::execution_providers::CPUExecutionProvider::default().build(),
        ]);
    }

    TextEmbedding::try_new(options)
}

/// Embed a batch of document texts for indexing, applying the active model's
/// document prefix.
pub fn embed_documents(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(vec![]);
    }
    let id = active_model_id();
    let spec = spec_for(&id);
    let prefixed: Vec<String> = texts
        .iter()
        .map(|t| format!("{}{t}", spec.doc_prefix))
        .collect();
    model_for(&id)?
        .embed(prefixed, None)
        .map_err(|e| anyhow!("{e}"))
}

/// Embed a single query string for semantic search, applying the active model's
/// query prefix. The persistent query cache is keyed by `(model_id, text)` so a
/// model switch never returns vectors from a different model.
pub fn embed_query(text: &str) -> Result<Vec<f32>> {
    let id = active_model_id();
    let spec = spec_for(&id);
    // Unit-separator keeps the model id and query text unambiguous; old plain-text
    // cache rows simply never match and are regenerated under the new key.
    let cache_key = format!("{id}\u{1f}{text}");

    // 1. Try checking the persistent query cache
    if let Some(conn) = open_query_cache_db() {
        if let Ok(mut stmt) =
            conn.prepare("SELECT embedding FROM query_cache WHERE query_text = ?1")
        {
            if let Ok(mut rows) = stmt.query(params![cache_key]) {
                if let Ok(Some(row)) = rows.next() {
                    if let Ok(blob) = row.get::<_, Vec<u8>>(0) {
                        return Ok(deserialize_vec(&blob));
                    }
                }
            }
        }
    }

    // 2. Generate embedding if not cached
    let prefixed = format!("{}{text}", spec.query_prefix);
    let vec = model_for(&id)?
        .embed(vec![prefixed], None)
        .map_err(|e| anyhow!("{e}"))?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("Empty embedding response"))?;

    // 3. Try to save to cache
    if let Some(conn) = open_query_cache_db() {
        let blob = serialize_vec(&vec);
        let _ = conn.execute(
            "INSERT OR REPLACE INTO query_cache (query_text, embedding) VALUES (?1, ?2)",
            params![cache_key, blob],
        );
    }

    Ok(vec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies the fastembed model loads and returns 768-dim vectors.
    /// Downloads ~130MB on first run; cached in %LOCALAPPDATA%\tokenix\models.
    #[test]
    #[cfg_attr(
        not(feature = "model-tests"),
        ignore = "needs model download; run with --features model-tests"
    )]
    fn embed_query_returns_768_dims() {
        let vec = embed_query("hello world").expect("embed_query failed");
        assert_eq!(
            vec.len(),
            768,
            "nomic-embed-text-v1.5 produces 768-dim vectors"
        );
    }

    #[test]
    fn embed_documents_empty_returns_empty() {
        let result = embed_documents(&[]).expect("empty embed should succeed");
        assert!(result.is_empty());
    }

    #[test]
    fn registry_defaults_and_lookups() {
        // Default is nomic-v1.5 and it carries nomic's prefixes.
        assert_eq!(active_model_id(), DEFAULT_MODEL_ID);
        let nomic = spec_for("nomic-v1.5");
        assert_eq!(nomic.query_prefix, "search_query: ");
        assert_eq!(nomic.doc_prefix, "search_document: ");
        // e5 needs its own prefixes; bge/minilm use none.
        assert_eq!(spec_for("e5-small").query_prefix, "query: ");
        assert_eq!(spec_for("bge-small").doc_prefix, "");
        // Unknown ids fall back to the default rather than breaking.
        assert!(!is_known_model("does-not-exist"));
        assert_eq!(spec_for("does-not-exist").id, DEFAULT_MODEL_ID);
    }

    #[test]
    fn set_active_model_overrides_default() {
        set_active_model("bge-small");
        assert_eq!(active_model_id(), "bge-small");
        // Unknown id normalizes to default.
        set_active_model("bogus");
        assert_eq!(active_model_id(), DEFAULT_MODEL_ID);
    }

    #[test]
    #[cfg_attr(
        not(feature = "model-tests"),
        ignore = "needs model download; run with --features model-tests"
    )]
    fn embed_documents_returns_correct_count() {
        let texts = vec![
            "fn main() {}".to_string(),
            "struct Foo { x: i32 }".to_string(),
        ];
        let vecs = embed_documents(&texts).expect("embed_documents failed");
        assert_eq!(vecs.len(), 2);
        for v in &vecs {
            assert_eq!(v.len(), 768);
        }
    }

    #[test]
    #[cfg_attr(
        not(feature = "model-tests"),
        ignore = "needs model download; run with --features model-tests"
    )]
    fn similar_texts_have_higher_cosine_similarity() {
        let q = embed_query("database connection pool").unwrap();
        let doc_similar =
            embed_documents(&["database connection pooling strategy".to_string()]).unwrap();
        let doc_different =
            embed_documents(&["sorting algorithms bubble sort".to_string()]).unwrap();

        let dot = |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b).map(|(x, y)| x * y).sum() };
        let norm = |a: &[f32]| -> f32 { a.iter().map(|x| x * x).sum::<f32>().sqrt() };
        let cosine = |a: &[f32], b: &[f32]| dot(a, b) / (norm(a) * norm(b));

        let sim_similar = cosine(&q, &doc_similar[0]);
        let sim_different = cosine(&q, &doc_different[0]);
        assert!(
            sim_similar > sim_different,
            "similar={sim_similar:.3} should > different={sim_different:.3}"
        );
    }

    #[test]
    #[cfg_attr(
        not(feature = "model-tests"),
        ignore = "needs model download; run with --features model-tests"
    )]
    fn test_query_cache_persistence() {
        let query = "test_persistent_cache_query_string_12345";

        // Remove from cache if exists to start clean
        if let Some(conn) = open_query_cache_db() {
            let _ = conn.execute("DELETE FROM query_cache WHERE query_text = ?1", [query]);
        }

        // First call: generates and caches
        let vec1 = embed_query(query).expect("First embed failed");

        // Second call: should retrieve from cache
        let vec2 = embed_query(query).expect("Second embed failed");

        assert_eq!(vec1.len(), 768);
        assert_eq!(vec1, vec2);
    }
}
