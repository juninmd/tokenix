//! Vector search: int8 quantization, cosine scoring, and the brute-force
//! top-k scan over `embeddings`.

use anyhow::Result;
use rusqlite::{params, Connection, OptionalExtension};

use super::embeddings_have_scale;

/// Symmetric int8 quantization: `scale = max|x| / 127`, each element stored as
/// one signed byte. 4x smaller than f32 with near-lossless cosine similarity
/// (the per-vector scale cancels out of the cosine entirely).
pub fn quantize_q8(v: &[f32]) -> (Vec<u8>, f32) {
    let max_abs = v.iter().fold(0.0f32, |m, x| m.max(x.abs()));
    let scale = if max_abs == 0.0 { 1.0 } else { max_abs / 127.0 };
    let data = v
        .iter()
        .map(|x| (x / scale).round().clamp(-127.0, 127.0) as i8 as u8)
        .collect();
    (data, scale)
}

/// Cosine similarity between an f32 query and an i8-quantized document vector.
/// The document's quantization scale cancels in the cosine, so only the raw
/// i8 bytes are needed.
pub fn cosine_similarity_to_q8(query_vec: &[f32], query_norm: f32, bytes: &[u8]) -> f32 {
    let mut dot = 0.0f32;
    let mut nb = 0.0f32;
    for (i, &b) in bytes.iter().enumerate() {
        if i >= query_vec.len() {
            break;
        }
        let y = b as i8 as f32;
        dot += query_vec[i] * y;
        nb += y * y;
    }
    let nb_sqrt = nb.sqrt();
    if query_norm == 0.0 || nb_sqrt == 0.0 {
        0.0
    } else {
        dot / (query_norm * nb_sqrt)
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[allow(dead_code)]
pub struct SearchResult {
    pub id: i64,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: String,
    pub kind: String,
    pub content: String,
    pub token_count: usize,
    pub distance: f32,
}

#[allow(dead_code)]
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

pub fn cosine_similarity_to_bytes(query_vec: &[f32], query_norm: f32, bytes: &[u8]) -> f32 {
    let mut dot = 0.0f32;
    let mut nb = 0.0f32;
    for (i, chunk) in bytes.as_chunks::<4>().0.iter().enumerate() {
        if i >= query_vec.len() {
            break;
        }
        let y = f32::from_le_bytes(*chunk);
        dot += query_vec[i] * y;
        nb += y * y;
    }
    let nb_sqrt = nb.sqrt();
    if query_norm == 0.0 || nb_sqrt == 0.0 {
        0.0
    } else {
        dot / (query_norm * nb_sqrt)
    }
}

/// Brute-force cosine in two passes: score every vector without touching chunk
/// text, then load content for the `k` winners only. Reading `c.content` for
/// every row made each query copy the whole indexed corpus out of SQLite.
pub fn search_similar(
    conn: &Connection,
    query_vec: &[f32],
    k: usize,
    file_filter: Option<&str>,
) -> Result<Vec<SearchResult>> {
    // Pre-migration DBs have no `scale` column; select NULL so every row takes
    // the legacy f32 path until `tokenix index` migrates the file.
    let scale_expr = if embeddings_have_scale(conn) {
        "e.scale"
    } else {
        "NULL"
    };
    let vector_row = |row: &rusqlite::Row<'_>| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Vec<u8>>(1)?,
            row.get::<_, Option<f64>>(2)?,
        ))
    };
    let vectors: Vec<(i64, Vec<u8>, Option<f64>)> = if let Some(filter) = file_filter {
        conn.prepare(&format!(
            "SELECT e.chunk_id, e.embedding, {scale_expr}
             FROM embeddings e JOIN chunks c ON c.id = e.chunk_id
             WHERE instr(c.path, ?1) > 0"
        ))?
        .query_map(params![filter], vector_row)?
        .collect::<rusqlite::Result<_>>()?
    } else {
        conn.prepare(&format!(
            "SELECT e.chunk_id, e.embedding, {scale_expr} FROM embeddings e"
        ))?
        .query_map([], vector_row)?
        .collect::<rusqlite::Result<_>>()?
    };

    let query_norm: f32 = query_vec.iter().map(|x| x * x).sum::<f32>().sqrt();

    use rayon::prelude::*;
    let mut scored: Vec<(f32, i64)> = vectors
        .par_iter()
        .map(|(id, blob, scale)| {
            // scale set → int8-quantized row; NULL → legacy f32 blob.
            let sim = if scale.is_some() {
                cosine_similarity_to_q8(query_vec, query_norm, blob)
            } else {
                cosine_similarity_to_bytes(query_vec, query_norm, blob)
            };
            (sim, *id)
        })
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    let mut stmt = conn.prepare_cached(
        "SELECT path, start_line, end_line, symbol, kind, content, token_count
         FROM chunks WHERE id = ?1",
    )?;
    let mut results = Vec::with_capacity(k.min(scored.len()));
    for (sim, id) in scored {
        if results.len() == k {
            break;
        }
        // An embedding whose chunk is gone is skipped, not counted toward k.
        let row = stmt
            .query_row(params![id], |r| {
                Ok(SearchResult {
                    id,
                    path: r.get(0)?,
                    start_line: r.get::<_, i64>(1)? as usize,
                    end_line: r.get::<_, i64>(2)? as usize,
                    symbol: r.get(3)?,
                    kind: r.get(4)?,
                    content: r.get(5)?,
                    token_count: r.get::<_, i64>(6)? as usize,
                    distance: 1.0 - sim,
                })
            })
            .optional()?;
        results.extend(row);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        init_schema, insert_chunk, insert_embedding, serialize_vec, upsert_file, NewChunk,
    };

    #[test]
    fn test_cosine_similarity_to_bytes() {
        let q = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![0.5, -1.0, 2.0, 1.5];
        let q_norm = q.iter().map(|x| x * x).sum::<f32>().sqrt();

        let sim1 = cosine_similarity(&q, &b);
        let bytes = serialize_vec(&b);
        let sim2 = cosine_similarity_to_bytes(&q, q_norm, &bytes);

        assert!((sim1 - sim2).abs() < 1e-6);
    }

    #[test]
    fn test_q8_cosine_matches_f32() {
        // Pseudo-embedding with mixed signs/magnitudes; q8 cosine must track f32.
        let doc: Vec<f32> = (0..768)
            .map(|i| ((i as f32 * 0.37).sin() * 0.04) - 0.01)
            .collect();
        let query: Vec<f32> = (0..768)
            .map(|i| ((i as f32 * 0.29).cos() * 0.05) + 0.005)
            .collect();
        let q_norm = query.iter().map(|x| x * x).sum::<f32>().sqrt();

        let exact = cosine_similarity(&query, &doc);
        let (q8, scale) = quantize_q8(&doc);
        assert!(scale > 0.0);
        assert_eq!(q8.len(), doc.len());
        let approx = cosine_similarity_to_q8(&query, q_norm, &q8);

        assert!(
            (exact - approx).abs() < 0.01,
            "q8 cosine drifted: exact={exact} approx={approx}"
        );
    }

    fn seed_chunk(conn: &Connection, path: &str, content: &str, v: &[f32]) -> i64 {
        let file_id = upsert_file(conn, path, 1.0, "h").unwrap();
        let id = insert_chunk(
            conn,
            NewChunk {
                file_id,
                path,
                start: 1,
                end: 2,
                symbol: "s",
                kind: "function",
                content,
                token_count: 3,
            },
        )
        .unwrap();
        insert_embedding(conn, id, v).unwrap();
        id
    }

    #[test]
    fn search_similar_returns_top_k_by_similarity_with_content() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn, 3).unwrap();
        let near = seed_chunk(&conn, "src/near.rs", "near body", &[1.0, 0.0, 0.0]);
        let mid = seed_chunk(&conn, "src/mid.rs", "mid body", &[1.0, 1.0, 0.0]);
        seed_chunk(&conn, "lib/far.rs", "far body", &[0.0, 0.0, 1.0]);

        let top = search_similar(&conn, &[1.0, 0.0, 0.0], 2, None).unwrap();
        let ids: Vec<i64> = top.iter().map(|r| r.id).collect();
        assert_eq!(ids, vec![near, mid], "ranked by cosine, cut at k");
        assert_eq!(top[0].content, "near body");
        assert_eq!(top[1].path, "src/mid.rs");

        let filtered = search_similar(&conn, &[0.0, 0.0, 1.0], 5, Some("src/")).unwrap();
        assert!(
            filtered.iter().all(|r| r.path.starts_with("src/")),
            "file filter must exclude lib/far.rs"
        );
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    #[ignore = "benchmark: cargo test --release --bin tokenix bench_search_similar -- --ignored --nocapture"]
    fn bench_search_similar() {
        let path = std::env::temp_dir().join(format!("tokenix-bench-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut conn = Connection::open(&path).unwrap();
        init_schema(&conn, 768).unwrap();
        let (n, dim) = (30_000usize, 768usize);
        let content = "x".repeat(1600);
        let tx = conn.transaction().unwrap();
        let file_id = upsert_file(&tx, "src/a.rs", 1.0, "h").unwrap();
        for i in 0..n {
            let id = insert_chunk(
                &tx,
                NewChunk {
                    file_id,
                    path: "src/a.rs",
                    start: i,
                    end: i + 1,
                    symbol: "s",
                    kind: "function",
                    content: &content,
                    token_count: 400,
                },
            )
            .unwrap();
            let v: Vec<f32> = (0..dim)
                .map(|j| ((i * 31 + j * 17) % 97) as f32 / 48.0 - 1.0)
                .collect();
            insert_embedding(&tx, id, &v).unwrap();
        }
        tx.commit().unwrap();

        let q: Vec<f32> = (0..dim).map(|j| (j % 13) as f32 / 6.0 - 1.0).collect();
        // Warm the page cache so runs measure the query, not the first disk read.
        for _ in 0..3 {
            search_similar(&conn, &q, 10, None).unwrap();
        }
        let runs = 5;
        let t = std::time::Instant::now();
        for _ in 0..runs {
            assert_eq!(search_similar(&conn, &q, 10, None).unwrap().len(), 10);
        }
        eprintln!(
            "search_similar n={n} dim={dim}: {:.1} ms/query",
            t.elapsed().as_secs_f64() * 1000.0 / runs as f64
        );
        drop(conn);
        let _ = std::fs::remove_file(&path);
    }
}
