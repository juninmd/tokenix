use anyhow::{anyhow, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use once_cell::sync::OnceCell;
use rusqlite::{params, Connection};
use std::path::PathBuf;

static MODEL: OnceCell<TextEmbedding> = OnceCell::new();

fn model_cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")))
        .join("tokenix")
        .join("models")
}

fn open_query_cache_db() -> Option<Connection> {
    let dir = dirs::home_dir()?.join(".tokenix");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("query_cache.db");
    let conn = Connection::open(&path).ok()?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;").ok()?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS query_cache (
            query_text TEXT PRIMARY KEY,
            embedding BLOB NOT NULL
        )",
        [],
    ).ok()?;
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

fn model() -> Result<&'static TextEmbedding> {
    MODEL
        .get_or_try_init(|| {
            // OMP_NUM_THREADS=1: limits OpenMP thread count for non-Windows ORT builds.
            // On Windows, ORT prebuilt uses OpenMP; this var is set in main() before hook runs.
            // Set here too as belt-and-suspenders for indexer and query paths.
            #[allow(unused_unsafe)]
            if std::env::var("OMP_NUM_THREADS").is_err() {
                unsafe { std::env::set_var("OMP_NUM_THREADS", "1") };
            }
            let cache_dir = model_cache_dir();
            std::fs::create_dir_all(&cache_dir).ok();
            TextEmbedding::try_new(
                InitOptions::new(EmbeddingModel::NomicEmbedTextV15Q).with_cache_dir(cache_dir),
            )
        })
        .map_err(|e| anyhow!("Embedding model init failed: {e}"))
}

/// Embed a batch of document texts for indexing.
/// Applies the "search_document:" prefix required by nomic-embed-text-v1.5.
pub fn embed_documents(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(vec![]);
    }
    let prefixed: Vec<String> = texts
        .iter()
        .map(|t| format!("search_document: {t}"))
        .collect();
    model()?.embed(prefixed, None).map_err(|e| anyhow!("{e}"))
}

/// Embed a single query string for semantic search.
/// Applies the "search_query:" prefix required by nomic-embed-text-v1.5.
pub fn embed_query(text: &str) -> Result<Vec<f32>> {
    // 1. Try checking the persistent query cache
    if let Some(conn) = open_query_cache_db() {
        if let Ok(mut stmt) = conn.prepare("SELECT embedding FROM query_cache WHERE query_text = ?1") {
            if let Ok(mut rows) = stmt.query(params![text]) {
                if let Ok(Some(row)) = rows.next() {
                    if let Ok(blob) = row.get::<_, Vec<u8>>(0) {
                        return Ok(deserialize_vec(&blob));
                    }
                }
            }
        }
    }

    // 2. Generate embedding if not cached
    let prefixed = format!("search_query: {text}");
    let vec = model()?
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
            params![text, blob],
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
