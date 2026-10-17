//! SQLite-backed persistence for embeddings and text chunks.
//!
//! Stores computed embedding vectors and document chunks so they survive
//! across app launches. The HNSW graph is rebuilt from stored vectors on
//! startup (O(n) insert) instead of recomputing embeddings (~2-5ms each).
//!
//! Schema:
//! - `vectors`: embedding vectors with source linkage and model info
//! - `chunks`: text chunks extracted from PDFs with page/offset metadata
//!
//! # Schema versioning (ADR-0028 D2)
//!
//! The file carries `PRAGMA user_version`. `open` runs whatever migration is
//! needed to reach [`SCHEMA_VERSION`], in one transaction, before returning —
//! every caller (Swift via imbib-core's UniFFI shims, or a headless Rust
//! process linking this crate directly) always sees the current shape.
//! Version 0 is the shape this module shipped with before migrations
//! existed: bare `CREATE TABLE IF NOT EXISTS`, no `owner_type`. Version 1
//! adds `chunks.owner_type` and `idx_vectors_model_type`.
//!
//! `chunks.publication_id` is the **owner id**, despite the name: it holds
//! the id of whatever item owns the chunk, not necessarily a publication.
//! The column keeps its name because it is a `uniffi::Record` field Swift
//! binds against
//! (`apps/imbib/ImbibRustCore/Sources/ImbibRustCore/imbib_core.swift`) —
//! renaming it would churn every FFI call site for zero behavioral gain.
//! `chunks.owner_type` disambiguates instead: `'publication'` is the legacy
//! default, correct for every row written before this migration and by
//! every writer that predates it; `'memory-item'` and `'content-chunk'`
//! arrive with the backfill executor (ADR-0028 D7, `impress.memory.embed`).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// A stored embedding vector with metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredVector {
    /// Unique vector ID (UUID string)
    pub id: String,
    /// Source entity ID (publication_id or chunk_id)
    pub source_id: String,
    /// Source type: "publication" or "chunk"
    pub source_type: String,
    /// The embedding vector
    pub vector: Vec<f32>,
    /// Model identifier, e.g. "apple-nl-384", "fastembed-384"
    pub model: String,
    /// ISO 8601 creation timestamp
    pub created_at: String,
}

/// A text chunk extracted from a publication's PDF.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StoredChunk {
    /// Unique chunk ID (UUID string)
    pub id: String,
    /// Parent publication ID
    pub publication_id: String,
    /// The chunk text
    pub text: String,
    /// Page number in the PDF (0-indexed), if known
    pub page_number: Option<u32>,
    /// Character offset within the full document text
    pub char_offset: u32,
    /// Character length of the chunk
    pub char_length: u32,
    /// Sequential chunk index within the publication (0, 1, 2, ...)
    pub chunk_index: u32,
}

/// Statistics about stored vectors per model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelStats {
    pub model: String,
    pub vector_count: u32,
    pub dimension: u32,
}

/// `vectors.source_type` for a paper's metadata (title/abstract) embedding.
pub const SOURCE_TYPE_PUBLICATION: &str = "publication";
/// `vectors.source_type` for one chunk of a paper's full text.
pub const SOURCE_TYPE_CHUNK: &str = "chunk";

/// Vectors and distinct sources held for one `source_type`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceTypeCount {
    pub source_type: String,
    /// Rows in `vectors` with this source type.
    pub vectors: u32,
    /// Distinct `source_id`s — papers, chunks or memory items.
    pub sources: u32,
}

/// What the sidecar actually holds, tier by tier.
///
/// The sidecar is multi-tenant (a paper's metadata vector, its full-text
/// chunk vectors, and impel's `memory-item` vectors share one file), and the
/// tiers are populated by different pipelines: every paper gets a metadata
/// vector when the index is built, while a chunk vector exists only for a
/// paper whose PDF is downloaded and chunked. A caller that collapses them
/// into one "indexed" number reports a fully embedded library as unindexed,
/// which is exactly what imbib's settings pane did before this type existed.
/// `by_source_type` keeps the same answer available to tenants this crate
/// does not know about.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EmbeddingIndexStatus {
    /// Papers with a metadata (title/abstract) vector.
    pub indexed_publications: u32,
    /// Vectors of source type `publication`.
    pub publication_vectors: u32,
    /// Papers with at least one stored full-text chunk.
    pub chunked_publications: u32,
    /// Vectors of source type `chunk`.
    pub chunk_vectors: u32,
    /// Rows in `chunks`; more than `chunk_vectors` means a partial index.
    pub chunk_count: u32,
    /// Every vector, whatever its tenant.
    pub vector_count: u32,
    /// Every tier present, sorted by name.
    pub by_source_type: Vec<SourceTypeCount>,
}

/// Status of embeddings for a specific publication.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PublicationEmbeddingStatus {
    pub publication_id: String,
    pub has_publication_vector: bool,
    pub chunk_count: u32,
    pub model: String,
}

// ---------------------------------------------------------------------------
// SQLite-backed EmbeddingStore
// ---------------------------------------------------------------------------

pub struct EmbeddingStore {
    db_path: PathBuf,
}

impl EmbeddingStore {
    /// Open or create an embedding store at the given path.
    pub fn open(path: &str) -> Result<Self, String> {
        let db_path = PathBuf::from(path);

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        }

        let mut conn = rusqlite::Connection::open(&db_path)
            .map_err(|e| format!("Failed to open database: {}", e))?;

        // Enable WAL mode for better concurrent read performance. `busy_timeout`
        // is what makes the migration below actually serialize concurrent
        // first-openers instead of one of them failing outright: an IMMEDIATE
        // transaction still needs a wait budget, or the loser gets SQLITE_BUSY
        // immediately rather than blocking until the winner commits.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| format!("Failed to set pragmas: {}", e))?;

        // Create tables
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS vectors (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    vector BLOB NOT NULL,
                    model TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_vectors_source ON vectors(source_id, source_type);
                CREATE INDEX IF NOT EXISTS idx_vectors_model ON vectors(model);

                CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    publication_id TEXT NOT NULL,
                    text TEXT NOT NULL,
                    page_number INTEGER,
                    char_offset INTEGER NOT NULL,
                    char_length INTEGER NOT NULL,
                    chunk_index INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_chunks_pub ON chunks(publication_id);
                ",
        )
        .map_err(|e| format!("Failed to create tables: {}", e))?;

        run_migrations(&mut conn)?;

        Ok(Self { db_path })
    }

    fn conn(&self) -> Result<rusqlite::Connection, String> {
        rusqlite::Connection::open(&self.db_path)
            .map_err(|e| format!("Failed to open connection: {}", e))
    }

    // -- Vectors -----------------------------------------------------------

    /// Save embedding vectors (upserts by id).
    pub fn save_vectors(&self, vectors: &[StoredVector]) -> Result<usize, String> {
        let conn = self.conn()?;
        let mut count = 0usize;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction error: {}", e))?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO vectors (id, source_id, source_type, vector, model, created_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                )
                .map_err(|e| format!("Prepare error: {}", e))?;

            for v in vectors {
                let blob = floats_to_bytes(&v.vector);
                stmt.execute(rusqlite::params![
                    v.id,
                    v.source_id,
                    v.source_type,
                    blob,
                    v.model,
                    v.created_at,
                ])
                .map_err(|e| format!("Insert error: {}", e))?;
                count += 1;
            }
        }

        tx.commit().map_err(|e| format!("Commit error: {}", e))?;

        Ok(count)
    }

    /// Get all vectors for a given source entity.
    pub fn get_vectors(&self, source_id: &str) -> Result<Vec<StoredVector>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, source_type, vector, model, created_at
                     FROM vectors WHERE source_id = ?1",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map([source_id], |row| {
                let blob: Vec<u8> = row.get(3)?;
                Ok(StoredVector {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    source_type: row.get(2)?,
                    vector: bytes_to_floats(&blob),
                    model: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row error: {}", e))
    }

    /// Load all vectors (for rebuilding HNSW index at startup).
    pub fn load_all_vectors(&self) -> Result<Vec<StoredVector>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare("SELECT id, source_id, source_type, vector, model, created_at FROM vectors")
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                let blob: Vec<u8> = row.get(3)?;
                Ok(StoredVector {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    source_type: row.get(2)?,
                    vector: bytes_to_floats(&blob),
                    model: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row error: {}", e))
    }

    /// Load vectors filtered by source_type (e.g., "publication" or "chunk").
    pub fn load_vectors_by_type(&self, source_type: &str) -> Result<Vec<StoredVector>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, source_type, vector, model, created_at
                     FROM vectors WHERE source_type = ?1",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map([source_type], |row| {
                let blob: Vec<u8> = row.get(3)?;
                Ok(StoredVector {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    source_type: row.get(2)?,
                    vector: bytes_to_floats(&blob),
                    model: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row error: {}", e))
    }

    /// Load vectors filtered by both `source_type` and `model`, via the
    /// `idx_vectors_model_type` composite index (ADR-0028 D2/D4).
    ///
    /// Rust-only: not exported over FFI. Swift keeps calling
    /// [`EmbeddingStore::load_vectors_by_type`] unfiltered, which is correct
    /// for imbib's single-model-per-device world — the model split matters
    /// only to cross-process readers (`impress-mcp`, the backfill executor)
    /// that must not mix `apple-nl` and `fastembed` vectors in one search.
    pub fn load_vectors_by_type_and_model(
        &self,
        source_type: &str,
        model: &str,
    ) -> Result<Vec<StoredVector>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, source_id, source_type, vector, model, created_at
                     FROM vectors WHERE source_type = ?1 AND model = ?2",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map(rusqlite::params![source_type, model], |row| {
                let blob: Vec<u8> = row.get(3)?;
                Ok(StoredVector {
                    id: row.get(0)?,
                    source_id: row.get(1)?,
                    source_type: row.get(2)?,
                    vector: bytes_to_floats(&blob),
                    model: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row error: {}", e))
    }

    /// Whether the store holds at least one vector for `model`.
    ///
    /// Rust-only. Used to gate the model filter (ADR-0028 D4): a device whose
    /// sidecar holds only foreign-model vectors (e.g. Swift's `apple-nl`)
    /// must keep serving cross-model results until a same-space vector
    /// exists to switch reads to — otherwise shipping the filter would blank
    /// out live semantic search the moment it deploys, ahead of the backfill.
    pub fn has_vectors_for_model(&self, model: &str) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vectors WHERE model = ?1)",
            [model],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("Query error: {}", e))
    }

    /// Whether the store holds at least one vector for both `source_type` and
    /// `model`.
    ///
    /// Rust-only. The store-wide check above stopped being a safe gate for
    /// per-type reads the moment the ADR-0028 embed executor started writing
    /// `memory-item` vectors into the same sidecar: one memory vector for a
    /// model satisfies the store-wide EXISTS while a strict per-type load for
    /// that model still returns zero rows. A gate that decides which rows of
    /// one `source_type` load must ask about that `source_type`.
    pub fn has_vectors_for_source_and_model(
        &self,
        source_type: &str,
        model: &str,
    ) -> Result<bool, String> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM vectors WHERE source_type = ?1 AND model = ?2)",
            rusqlite::params![source_type, model],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| format!("Query error: {}", e))
    }

    /// Which of `source_ids` already hold a vector under `model`, as
    /// `(source_id, source_type)` pairs.
    ///
    /// Rust-only, like [`EmbeddingStore::load_vectors_by_type_and_model`]:
    /// this is the embed backfill's already-embedded probe (ADR-0028 D7) —
    /// one batched read per candidate page, instead of a per-row
    /// [`EmbeddingStore::get_vectors`] loop or dragging every vector blob
    /// through the type-and-model load just to learn which ids exist. Pairs
    /// rather than bare ids because one page can mix source types
    /// (`"chunk"` and `"memory-item"`), and a hit only counts against the
    /// type the caller would stamp.
    pub fn embedded_sources_for_model(
        &self,
        source_ids: &[String],
        model: &str,
    ) -> Result<Vec<(String, String)>, String> {
        if source_ids.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn()?;
        let mut found = Vec::new();
        // Chunked IN lists: stays far under SQLITE_MAX_VARIABLE_NUMBER (999
        // on conservative builds) whatever page size the caller scans with.
        for chunk in source_ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "SELECT source_id, source_type FROM vectors
                     WHERE model = ? AND source_id IN ({placeholders})"
            );
            let mut stmt = conn
                .prepare(&sql)
                .map_err(|e| format!("Prepare error: {}", e))?;
            let mut params: Vec<&str> = Vec::with_capacity(chunk.len() + 1);
            params.push(model);
            params.extend(chunk.iter().map(String::as_str));
            let rows = stmt
                .query_map(rusqlite::params_from_iter(params), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|e| format!("Query error: {}", e))?;
            for row in rows {
                found.push(row.map_err(|e| format!("Row error: {}", e))?);
            }
        }
        Ok(found)
    }

    /// Delete all vectors for a source entity.
    pub fn delete_by_source(&self, source_id: &str) -> Result<u32, String> {
        let conn = self.conn()?;
        let count = conn
            .execute("DELETE FROM vectors WHERE source_id = ?1", [source_id])
            .map_err(|e| format!("Delete error: {}", e))?;
        Ok(count as u32)
    }

    /// Delete all vectors for a given model (used when switching providers).
    pub fn delete_by_model(&self, model: &str) -> Result<u32, String> {
        let conn = self.conn()?;
        let count = conn
            .execute("DELETE FROM vectors WHERE model = ?1", [model])
            .map_err(|e| format!("Delete error: {}", e))?;
        Ok(count as u32)
    }

    /// Delete vectors for one owner scoped to a `source_type`, e.g. dropping
    /// only the `"chunk"` vectors for an item without touching a
    /// same-id `"publication"` vector. Rust-only counterpart to
    /// [`EmbeddingStore::delete_by_source`], which deletes every
    /// `source_type` for a `source_id`.
    pub fn delete_by_source_and_type(
        &self,
        source_id: &str,
        source_type: &str,
    ) -> Result<u32, String> {
        let conn = self.conn()?;
        let count = conn
            .execute(
                "DELETE FROM vectors WHERE source_id = ?1 AND source_type = ?2",
                rusqlite::params![source_id, source_type],
            )
            .map_err(|e| format!("Delete error: {}", e))?;
        Ok(count as u32)
    }

    /// Total vector count.
    pub fn vector_count(&self) -> Result<u32, String> {
        let conn = self.conn()?;
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM vectors", [], |row| row.get(0))
            .map_err(|e| format!("Count error: {}", e))?;
        Ok(count)
    }

    /// Get per-model statistics.
    pub fn model_stats(&self) -> Result<Vec<ModelStats>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT model, COUNT(*), LENGTH(vector) / 4
                     FROM vectors GROUP BY model",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map([], |row| {
                Ok(ModelStats {
                    model: row.get(0)?,
                    vector_count: row.get(1)?,
                    dimension: row.get(2)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row error: {}", e))
    }

    // -- Chunks ------------------------------------------------------------

    /// Save text chunks (upserts by id).
    pub fn save_chunks(&self, chunks: &[StoredChunk]) -> Result<usize, String> {
        let conn = self.conn()?;
        let mut count = 0usize;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction error: {}", e))?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO chunks
                         (id, publication_id, text, page_number, char_offset, char_length, chunk_index)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                )
                .map_err(|e| format!("Prepare error: {}", e))?;

            for c in chunks {
                stmt.execute(rusqlite::params![
                    c.id,
                    c.publication_id,
                    c.text,
                    c.page_number,
                    c.char_offset,
                    c.char_length,
                    c.chunk_index,
                ])
                .map_err(|e| format!("Insert error: {}", e))?;
                count += 1;
            }
        }

        tx.commit().map_err(|e| format!("Commit error: {}", e))?;

        Ok(count)
    }

    /// Save text chunks (upserts by id), stamping `owner_type` explicitly.
    ///
    /// Rust-only sibling of [`EmbeddingStore::save_chunks`] rather than a
    /// change to it: [`StoredChunk`] carries no `owner_type` field — it
    /// mirrors imbib-core's `uniffi::Record` byte-for-byte (ADR-0028 D2) —
    /// so `save_chunks` keeps writing the `'publication'` default every
    /// existing caller (Swift, via imbib-core's shim) already expects. This
    /// method is for writers that know their chunks belong to something
    /// else, e.g. the backfill executor chunking a `memory/claim` body.
    pub fn save_chunks_with_owner_type(
        &self,
        chunks: &[StoredChunk],
        owner_type: &str,
    ) -> Result<usize, String> {
        let conn = self.conn()?;
        let mut count = 0usize;

        let tx = conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction error: {}", e))?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO chunks
                         (id, publication_id, text, page_number, char_offset, char_length, chunk_index, owner_type)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|e| format!("Prepare error: {}", e))?;

            for c in chunks {
                stmt.execute(rusqlite::params![
                    c.id,
                    c.publication_id,
                    c.text,
                    c.page_number,
                    c.char_offset,
                    c.char_length,
                    c.chunk_index,
                    owner_type,
                ])
                .map_err(|e| format!("Insert error: {}", e))?;
                count += 1;
            }
        }

        tx.commit().map_err(|e| format!("Commit error: {}", e))?;

        Ok(count)
    }

    /// Get all chunks for a publication.
    pub fn get_chunks(&self, publication_id: &str) -> Result<Vec<StoredChunk>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, publication_id, text, page_number, char_offset, char_length, chunk_index
                     FROM chunks WHERE publication_id = ?1 ORDER BY chunk_index",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;

        let rows = stmt
            .query_map([publication_id], |row| {
                Ok(StoredChunk {
                    id: row.get(0)?,
                    publication_id: row.get(1)?,
                    text: row.get(2)?,
                    page_number: row.get(3)?,
                    char_offset: row.get(4)?,
                    char_length: row.get(5)?,
                    chunk_index: row.get(6)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| format!("Row error: {}", e))
    }

    /// Get a single chunk by ID.
    pub fn get_chunk(&self, chunk_id: &str) -> Result<Option<StoredChunk>, String> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT id, publication_id, text, page_number, char_offset, char_length, chunk_index
                     FROM chunks WHERE id = ?1",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;

        let mut rows = stmt
            .query_map([chunk_id], |row| {
                Ok(StoredChunk {
                    id: row.get(0)?,
                    publication_id: row.get(1)?,
                    text: row.get(2)?,
                    page_number: row.get(3)?,
                    char_offset: row.get(4)?,
                    char_length: row.get(5)?,
                    chunk_index: row.get(6)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;

        match rows.next() {
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(e)) => Err(format!("Row error: {}", e)),
            None => Ok(None),
        }
    }

    /// Delete all chunks for a publication.
    pub fn delete_chunks(&self, publication_id: &str) -> Result<u32, String> {
        let conn = self.conn()?;
        let count = conn
            .execute(
                "DELETE FROM chunks WHERE publication_id = ?1",
                [publication_id],
            )
            .map_err(|e| format!("Delete error: {}", e))?;
        Ok(count as u32)
    }

    /// Total chunk count.
    pub fn chunk_count(&self) -> Result<u32, String> {
        let conn = self.conn()?;
        let count: u32 = conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))
            .map_err(|e| format!("Count error: {}", e))?;
        Ok(count)
    }

    /// Every tier of the index in one pass — the honest answer to "how much
    /// of the library is embedded?", which no single count can give.
    pub fn index_status(&self) -> Result<EmbeddingIndexStatus, String> {
        let conn = self.conn()?;
        let mut statement = conn
            .prepare(
                "SELECT source_type, COUNT(*), COUNT(DISTINCT source_id)
                     FROM vectors GROUP BY source_type",
            )
            .map_err(|e| format!("Prepare error: {}", e))?;
        let rows = statement
            .query_map([], |row| {
                Ok(SourceTypeCount {
                    source_type: row.get(0)?,
                    vectors: row.get(1)?,
                    sources: row.get(2)?,
                })
            })
            .map_err(|e| format!("Query error: {}", e))?;
        let mut by_source_type: Vec<SourceTypeCount> = Vec::new();
        for row in rows {
            by_source_type.push(row.map_err(|e| format!("Row error: {}", e))?);
        }
        by_source_type.sort_by(|a, b| a.source_type.cmp(&b.source_type));

        let (chunked_publications, chunk_count): (u32, u32) = conn
            .query_row(
                "SELECT COUNT(DISTINCT publication_id), COUNT(*) FROM chunks",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|e| format!("Count error: {}", e))?;

        let publication = by_source_type
            .iter()
            .find(|tier| tier.source_type == SOURCE_TYPE_PUBLICATION);
        let chunk = by_source_type
            .iter()
            .find(|tier| tier.source_type == SOURCE_TYPE_CHUNK);
        let status = EmbeddingIndexStatus {
            indexed_publications: publication.map_or(0, |tier| tier.sources),
            publication_vectors: publication.map_or(0, |tier| tier.vectors),
            chunked_publications,
            chunk_vectors: chunk.map_or(0, |tier| tier.vectors),
            chunk_count,
            vector_count: by_source_type.iter().map(|tier| tier.vectors).sum(),
            by_source_type: by_source_type.clone(),
        };
        Ok(status)
    }

    /// Number of publications with chunks.
    pub fn chunked_publication_count(&self) -> Result<u32, String> {
        let conn = self.conn()?;
        let count: u32 = conn
            .query_row(
                "SELECT COUNT(DISTINCT publication_id) FROM chunks",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Count error: {}", e))?;
        Ok(count)
    }

    /// Delete everything (used when switching providers entirely).
    pub fn clear_all(&self) -> Result<(), String> {
        let conn = self.conn()?;
        conn.execute_batch("DELETE FROM vectors; DELETE FROM chunks;")
            .map_err(|e| format!("Clear error: {}", e))?;
        Ok(())
    }
}

// -- Schema migrations (ADR-0028 D2) ----------------------------------------

/// Schema version this build expects [`EmbeddingStore::open`] to leave the
/// file at. Bump alongside a new step added to [`run_migrations`].
const SCHEMA_VERSION: i64 = 1;

/// Bring a possibly-older sidecar file up to [`SCHEMA_VERSION`].
///
/// Runs entirely inside one IMMEDIATE transaction so two processes racing to
/// open the same file for the first time (a headless `impress` invocation
/// and imbib.app starting at once, say) serialize on SQLite's write lock
/// instead of one corrupting the other's DDL or both double-migrating. The
/// `busy_timeout` pragma set by `open` just before this runs is what turns
/// that serialization into a wait instead of an immediate `SQLITE_BUSY`
/// error for whichever process loses the race.
fn run_migrations(conn: &mut rusqlite::Connection) -> Result<(), String> {
    if schema_version(conn)? >= SCHEMA_VERSION {
        return Ok(());
    }

    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|e| format!("Failed to begin migration transaction: {}", e))?;

    // Re-read inside the transaction: this opener may have been waiting on
    // the lock above while another opener finished the same migration, in
    // which case there is nothing left to do.
    if schema_version(&tx)? < 1 {
        migrate_v0_to_v1(&tx)?;
    }

    tx.pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(|e| format!("Failed to set schema version: {}", e))?;

    tx.commit()
        .map_err(|e| format!("Failed to commit migration: {}", e))?;

    Ok(())
}

fn schema_version(conn: &rusqlite::Connection) -> Result<i64, String> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|e| format!("Failed to read schema version: {}", e))
}

/// v0 -> v1: add `chunks.owner_type` and the `(model, source_type)`
/// composite index the model-aware reads in this module and in
/// `impress-mcp` need (ADR-0028 D2, D4). See the module docs for what
/// `owner_type` means and why `publication_id` is not renamed alongside it.
fn migrate_v0_to_v1(tx: &rusqlite::Transaction) -> Result<(), String> {
    if !column_exists(tx, "chunks", "owner_type")? {
        if let Err(e) = tx.execute(
            "ALTER TABLE chunks ADD COLUMN owner_type TEXT NOT NULL DEFAULT 'publication'",
            [],
        ) {
            // Belt-and-suspenders for a v0 file that raced another opener
            // despite the IMMEDIATE transaction above: swallow only the
            // "already there" failure; anything else is a real error.
            if !e.to_string().contains("duplicate column name") {
                return Err(format!("Failed to add owner_type column: {}", e));
            }
        }
    }

    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_vectors_model_type ON vectors(model, source_type);",
    )
    .map_err(|e| format!("Failed to create idx_vectors_model_type: {}", e))?;

    Ok(())
}

/// Whether `table` has a column named `column`, via `PRAGMA table_info`.
///
/// Guards a migration step that is not itself idempotent — unlike every
/// `IF NOT EXISTS` form used elsewhere in this module, `ALTER TABLE ... ADD
/// COLUMN` errors if the column is already there.
fn column_exists(conn: &rusqlite::Connection, table: &str, column: &str) -> Result<bool, String> {
    // PRAGMA does not accept bound parameters for its argument. Safe to
    // format directly: `table` is always one of this module's own hardcoded
    // table names, never caller-controlled input.
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn
        .prepare(&sql)
        .map_err(|e| format!("Failed to inspect {table} schema: {}", e))?;
    let mut rows = stmt
        .query([])
        .map_err(|e| format!("Failed to inspect {table} schema: {}", e))?;
    while let Some(row) = rows
        .next()
        .map_err(|e| format!("Failed to read {table} schema: {}", e))?
    {
        let name: String = row
            .get(1)
            .map_err(|e| format!("Failed to read {table} column name: {}", e))?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

// -- Helpers ---------------------------------------------------------------

/// Encode f32 slice as little-endian bytes for SQLite BLOB storage.
fn floats_to_bytes(floats: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(floats.len() * 4);
    for f in floats {
        bytes.extend_from_slice(&f.to_le_bytes());
    }
    bytes
}

/// Decode little-endian bytes back to f32 vec.
fn bytes_to_floats(bytes: &[u8]) -> Vec<f32> {
    let (chunks, _) = bytes.as_chunks::<4>();
    chunks
        .iter()
        .map(|chunk| f32::from_le_bytes(*chunk))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    struct TestStore {
        store: EmbeddingStore,
        _dir: tempfile::TempDir,
    }

    impl std::ops::Deref for TestStore {
        type Target = EmbeddingStore;

        fn deref(&self) -> &Self::Target {
            &self.store
        }
    }

    fn temp_store() -> TestStore {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_embeddings.sqlite");
        let store = EmbeddingStore::open(path.to_str().unwrap()).unwrap();
        TestStore { store, _dir: dir }
    }

    fn vector(id: &str, source_id: &str, source_type: &str) -> StoredVector {
        StoredVector {
            id: id.into(),
            source_id: source_id.into(),
            source_type: source_type.into(),
            vector: vec![0.1, 0.2, 0.3, 0.4],
            model: "test-model-4".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn chunk(id: &str, publication_id: &str, index: u32) -> StoredChunk {
        StoredChunk {
            id: id.into(),
            publication_id: publication_id.into(),
            text: format!("chunk {index}"),
            page_number: Some(index),
            char_offset: index * 100,
            char_length: 100,
            chunk_index: index,
        }
    }

    #[test]
    fn index_status_separates_the_metadata_tier_from_the_full_text_tier() {
        let store = temp_store();

        // Two papers with a metadata vector; only one of them has a PDF that
        // was chunked (two chunks), plus an unrelated tenant's vector.
        store
            .save_vectors(&[
                vector("pub-a", "paper-a", SOURCE_TYPE_PUBLICATION),
                vector("pub-b", "paper-b", SOURCE_TYPE_PUBLICATION),
                vector("paper-a-chunk-0", "paper-a-chunk-0", SOURCE_TYPE_CHUNK),
                vector("paper-a-chunk-1", "paper-a-chunk-1", SOURCE_TYPE_CHUNK),
                vector("mem-1", "mem-1", "memory-item"),
            ])
            .unwrap();
        store
            .save_chunks(&[
                chunk("paper-a-chunk-0", "paper-a", 0),
                chunk("paper-a-chunk-1", "paper-a", 1),
            ])
            .unwrap();

        let status = store.index_status().unwrap();
        assert_eq!(status.indexed_publications, 2, "both papers have metadata");
        assert_eq!(status.publication_vectors, 2);
        assert_eq!(status.chunked_publications, 1, "only one PDF was chunked");
        assert_eq!(status.chunk_vectors, 2);
        assert_eq!(status.chunk_count, 2);
        assert_eq!(status.vector_count, 5, "every tenant's vectors");
        assert_eq!(
            status
                .by_source_type
                .iter()
                .map(|tier| tier.source_type.as_str())
                .collect::<Vec<_>>(),
            vec!["chunk", "memory-item", "publication"],
            "tiers are reported by name for tenants this crate does not know"
        );
    }

    #[test]
    fn index_status_of_an_empty_store_is_all_zeroes() {
        let store = temp_store();
        assert_eq!(
            store.index_status().unwrap(),
            EmbeddingIndexStatus::default()
        );
    }

    #[test]
    fn test_vector_roundtrip() {
        let store = temp_store();

        let vectors = vec![StoredVector {
            id: "v1".into(),
            source_id: "pub1".into(),
            source_type: "publication".into(),
            vector: vec![1.0, 0.5, -0.3, 0.0],
            model: "test-model-4".into(),
            created_at: "2026-01-01T00:00:00Z".into(),
        }];

        let saved = store.save_vectors(&vectors).unwrap();
        assert_eq!(saved, 1);

        let loaded = store.get_vectors("pub1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "v1");
        assert_eq!(loaded[0].vector, vec![1.0, 0.5, -0.3, 0.0]);
        assert_eq!(loaded[0].model, "test-model-4");
    }

    #[test]
    fn test_chunk_roundtrip() {
        let store = temp_store();

        let chunks = vec![
            StoredChunk {
                id: "c1".into(),
                publication_id: "pub1".into(),
                text: "First chunk of text.".into(),
                page_number: Some(0),
                char_offset: 0,
                char_length: 20,
                chunk_index: 0,
            },
            StoredChunk {
                id: "c2".into(),
                publication_id: "pub1".into(),
                text: "Second chunk of text.".into(),
                page_number: Some(1),
                char_offset: 20,
                char_length: 21,
                chunk_index: 1,
            },
        ];

        let saved = store.save_chunks(&chunks).unwrap();
        assert_eq!(saved, 2);

        let loaded = store.get_chunks("pub1").unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].chunk_index, 0);
        assert_eq!(loaded[1].chunk_index, 1);
    }

    #[test]
    fn test_load_all_vectors() {
        let store = temp_store();

        let vectors = vec![
            StoredVector {
                id: "v1".into(),
                source_id: "pub1".into(),
                source_type: "publication".into(),
                vector: vec![1.0, 0.0],
                model: "m1".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            StoredVector {
                id: "v2".into(),
                source_id: "c1".into(),
                source_type: "chunk".into(),
                vector: vec![0.0, 1.0],
                model: "m1".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];

        store.save_vectors(&vectors).unwrap();

        let all = store.load_all_vectors().unwrap();
        assert_eq!(all.len(), 2);

        let pubs = store.load_vectors_by_type("publication").unwrap();
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].source_id, "pub1");

        let chunks = store.load_vectors_by_type("chunk").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].source_id, "c1");
    }

    #[test]
    fn test_model_stats() {
        let store = temp_store();

        let vectors = vec![
            StoredVector {
                id: "v1".into(),
                source_id: "pub1".into(),
                source_type: "publication".into(),
                vector: vec![1.0, 0.0, 0.0],
                model: "apple-nl-384".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            StoredVector {
                id: "v2".into(),
                source_id: "pub2".into(),
                source_type: "publication".into(),
                vector: vec![0.0, 1.0, 0.0],
                model: "apple-nl-384".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];

        store.save_vectors(&vectors).unwrap();

        let stats = store.model_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].model, "apple-nl-384");
        assert_eq!(stats[0].vector_count, 2);
    }

    #[test]
    fn test_delete_operations() {
        let store = temp_store();

        let vectors = vec![
            StoredVector {
                id: "v1".into(),
                source_id: "pub1".into(),
                source_type: "publication".into(),
                vector: vec![1.0],
                model: "m1".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
            StoredVector {
                id: "v2".into(),
                source_id: "pub2".into(),
                source_type: "publication".into(),
                vector: vec![0.0],
                model: "m1".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            },
        ];

        store.save_vectors(&vectors).unwrap();
        assert_eq!(store.vector_count().unwrap(), 2);

        store.delete_by_source("pub1").unwrap();
        assert_eq!(store.vector_count().unwrap(), 1);

        store.clear_all().unwrap();
        assert_eq!(store.vector_count().unwrap(), 0);
    }

    // -- Migration (ADR-0028 D2) --------------------------------------------

    /// The exact CREATE TABLE statements `EmbeddingStore::open` used before
    /// migrations existed (ADR-0028 fact 4), copied here — not reused from
    /// `open()` — so a future edit to the current-version schema cannot
    /// silently rewrite what "v0" means out from under this test. Seeds one
    /// vector row shaped like Swift's `apple-nl` writer and one chunk row,
    /// both pre-dating `owner_type`.
    fn write_v0_fixture(path: &std::path::Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS vectors (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL,
                    source_type TEXT NOT NULL,
                    vector BLOB NOT NULL,
                    model TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_vectors_source ON vectors(source_id, source_type);
                CREATE INDEX IF NOT EXISTS idx_vectors_model ON vectors(model);

                CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    publication_id TEXT NOT NULL,
                    text TEXT NOT NULL,
                    page_number INTEGER,
                    char_offset INTEGER NOT NULL,
                    char_length INTEGER NOT NULL,
                    chunk_index INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_chunks_pub ON chunks(publication_id);
                ",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO vectors (id, source_id, source_type, vector, model, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                "v1",
                "pub1",
                "publication",
                floats_to_bytes(&[1.0, 0.5, -0.3, 0.0]),
                "apple-nl",
                "2026-01-01T00:00:00Z",
            ],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO chunks
                 (id, publication_id, text, page_number, char_offset, char_length, chunk_index)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params!["c1", "pub1", "chunk text", 0, 0, 10, 0],
        )
        .unwrap();

        assert_eq!(
            schema_version(&conn).unwrap(),
            0,
            "fixture must start at v0"
        );
    }

    #[test]
    fn test_v0_fixture_migrates_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("v0_embeddings.sqlite");
        write_v0_fixture(&path);

        let store = EmbeddingStore::open(path.to_str().unwrap()).unwrap();

        // Rows survive the migration, intact and readable.
        let vectors = store.get_vectors("pub1").unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].model, "apple-nl");
        assert_eq!(vectors[0].vector, vec![1.0, 0.5, -0.3, 0.0]);

        let chunks = store.get_chunks("pub1").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].id, "c1");
        assert_eq!(chunks[0].text, "chunk text");

        // model_stats is unchanged by the migration.
        let stats = store.model_stats().unwrap();
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].model, "apple-nl");
        assert_eq!(stats[0].vector_count, 1);

        // user_version bumped, owner_type defaulted for the pre-existing row.
        let conn = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(column_exists(&conn, "chunks", "owner_type").unwrap());
        let owner_type: String = conn
            .query_row("SELECT owner_type FROM chunks WHERE id = 'c1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(owner_type, "publication");

        // Double-open is idempotent: no error, same data, version unchanged.
        let store2 = EmbeddingStore::open(path.to_str().unwrap()).unwrap();
        assert_eq!(store2.get_chunks("pub1").unwrap().len(), 1);
        assert_eq!(store2.get_vectors("pub1").unwrap().len(), 1);
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn test_concurrent_open_migration_serializes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race_embeddings.sqlite");
        write_v0_fixture(&path);

        let path_str = path.to_str().unwrap().to_string();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let p = path_str.clone();
                std::thread::spawn(move || {
                    EmbeddingStore::open(&p).expect("concurrent open must not fail")
                })
            })
            .collect();

        for h in handles {
            let store = h.join().expect("opener thread panicked");
            assert_eq!(store.get_chunks("pub1").unwrap().len(), 1);
            assert_eq!(store.get_vectors("pub1").unwrap().len(), 1);
        }

        let conn = rusqlite::Connection::open(&path).unwrap();
        assert_eq!(schema_version(&conn).unwrap(), SCHEMA_VERSION);
        assert!(column_exists(&conn, "chunks", "owner_type").unwrap());
    }

    // -- Model-aware reads (ADR-0028 D4) ------------------------------------

    #[test]
    fn test_has_vectors_for_model() {
        let store = temp_store();
        assert!(!store
            .has_vectors_for_model("fastembed/AllMiniLML6V2")
            .unwrap());

        store
            .save_vectors(&[StoredVector {
                id: "v1".into(),
                source_id: "c1".into(),
                source_type: "chunk".into(),
                vector: vec![1.0, 0.0],
                model: "fastembed/AllMiniLML6V2".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }])
            .unwrap();

        assert!(store
            .has_vectors_for_model("fastembed/AllMiniLML6V2")
            .unwrap());
        assert!(!store.has_vectors_for_model("apple-nl").unwrap());
    }

    /// Both predicates must hold: a `memory-item` vector for a model must not
    /// make the store claim it has `chunk` vectors for that model (and vice
    /// versa) — that is exactly the confusion the store-wide check permits.
    #[test]
    fn test_has_vectors_for_source_and_model() {
        let store = temp_store();
        assert!(!store
            .has_vectors_for_source_and_model("chunk", "fastembed/AllMiniLML6V2")
            .unwrap());

        store
            .save_vectors(&[StoredVector {
                id: "v1".into(),
                source_id: "m1".into(),
                source_type: "memory-item".into(),
                vector: vec![1.0, 0.0],
                model: "fastembed/AllMiniLML6V2".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }])
            .unwrap();

        // The store-wide check answers yes; the per-type check must not.
        assert!(store
            .has_vectors_for_model("fastembed/AllMiniLML6V2")
            .unwrap());
        assert!(!store
            .has_vectors_for_source_and_model("chunk", "fastembed/AllMiniLML6V2")
            .unwrap());
        assert!(store
            .has_vectors_for_source_and_model("memory-item", "fastembed/AllMiniLML6V2")
            .unwrap());
        assert!(!store
            .has_vectors_for_source_and_model("memory-item", "apple-nl")
            .unwrap());

        store
            .save_vectors(&[StoredVector {
                id: "v2".into(),
                source_id: "c1".into(),
                source_type: "chunk".into(),
                vector: vec![0.0, 1.0],
                model: "fastembed/AllMiniLML6V2".into(),
                created_at: "2026-01-01T00:00:00Z".into(),
            }])
            .unwrap();

        assert!(store
            .has_vectors_for_source_and_model("chunk", "fastembed/AllMiniLML6V2")
            .unwrap());
    }

    /// The already-embedded probe must be exact on all three axes — a miss
    /// re-embeds (wasteful, harmless), but a false hit would skip a row
    /// forever, silently.
    #[test]
    fn test_embedded_sources_for_model() {
        let store = temp_store();
        let ids: Vec<String> = ["m1", "c1", "never-embedded"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert!(store
            .embedded_sources_for_model(&ids, "fastembed/AllMiniLML6V2")
            .unwrap()
            .is_empty());
        assert!(store
            .embedded_sources_for_model(&[], "any")
            .unwrap()
            .is_empty());

        store
            .save_vectors(&[
                StoredVector {
                    id: "v1".into(),
                    source_id: "m1".into(),
                    source_type: "memory-item".into(),
                    vector: vec![1.0, 0.0],
                    model: "fastembed/AllMiniLML6V2".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                StoredVector {
                    id: "v2".into(),
                    source_id: "c1".into(),
                    source_type: "chunk".into(),
                    vector: vec![0.0, 1.0],
                    model: "fastembed/AllMiniLML6V2".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                StoredVector {
                    id: "v3".into(),
                    source_id: "other-model".into(),
                    source_type: "chunk".into(),
                    vector: vec![0.5, 0.5],
                    model: "apple-nl".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            ])
            .unwrap();

        let mut found = store
            .embedded_sources_for_model(
                &[
                    "m1".to_string(),
                    "c1".to_string(),
                    "other-model".to_string(),
                    "never-embedded".to_string(),
                ],
                "fastembed/AllMiniLML6V2",
            )
            .unwrap();
        found.sort();
        assert_eq!(
            found,
            vec![
                ("c1".to_string(), "chunk".to_string()),
                ("m1".to_string(), "memory-item".to_string()),
            ],
            "a foreign-model vector and an absent id must not report as embedded"
        );

        // Ids not asked about never come back, even though they exist.
        let scoped = store
            .embedded_sources_for_model(&["m1".to_string()], "fastembed/AllMiniLML6V2")
            .unwrap();
        assert_eq!(scoped, vec![("m1".to_string(), "memory-item".to_string())]);
    }

    #[test]
    fn test_load_vectors_by_type_and_model() {
        let store = temp_store();

        store
            .save_vectors(&[
                StoredVector {
                    id: "v1".into(),
                    source_id: "c1".into(),
                    source_type: "chunk".into(),
                    vector: vec![1.0, 0.0],
                    model: "fastembed/AllMiniLML6V2".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                StoredVector {
                    id: "v2".into(),
                    source_id: "c2".into(),
                    source_type: "chunk".into(),
                    vector: vec![0.0, 1.0],
                    model: "apple-nl".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                StoredVector {
                    id: "v3".into(),
                    source_id: "pub1".into(),
                    source_type: "publication".into(),
                    vector: vec![0.5, 0.5],
                    model: "fastembed/AllMiniLML6V2".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            ])
            .unwrap();

        let chunks = store
            .load_vectors_by_type_and_model("chunk", "fastembed/AllMiniLML6V2")
            .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].source_id, "c1");

        let apple = store
            .load_vectors_by_type_and_model("chunk", "apple-nl")
            .unwrap();
        assert_eq!(apple.len(), 1);
        assert_eq!(apple[0].source_id, "c2");

        let pubs = store
            .load_vectors_by_type_and_model("publication", "fastembed/AllMiniLML6V2")
            .unwrap();
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].source_id, "pub1");

        let none = store
            .load_vectors_by_type_and_model("publication", "apple-nl")
            .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn test_delete_by_source_and_type() {
        let store = temp_store();

        store
            .save_vectors(&[
                StoredVector {
                    id: "v1".into(),
                    source_id: "x1".into(),
                    source_type: "chunk".into(),
                    vector: vec![1.0],
                    model: "m1".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
                StoredVector {
                    id: "v2".into(),
                    source_id: "x1".into(),
                    source_type: "publication".into(),
                    vector: vec![0.0],
                    model: "m1".into(),
                    created_at: "2026-01-01T00:00:00Z".into(),
                },
            ])
            .unwrap();

        let deleted = store.delete_by_source_and_type("x1", "chunk").unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(store.vector_count().unwrap(), 1);
        assert_eq!(
            store.get_vectors("x1").unwrap()[0].source_type,
            "publication"
        );
    }

    #[test]
    fn test_save_chunks_with_owner_type() {
        let store = temp_store();

        store
            .save_chunks_with_owner_type(
                &[StoredChunk {
                    id: "c1".into(),
                    publication_id: "item1".into(),
                    text: "a memory claim chunk".into(),
                    page_number: None,
                    char_offset: 0,
                    char_length: 20,
                    chunk_index: 0,
                }],
                "memory-item",
            )
            .unwrap();

        // StoredChunk itself carries no owner_type field (the FFI mirror
        // stays byte-identical); check the column directly.
        let conn = rusqlite::Connection::open(&store.db_path).unwrap();
        let owner_type: String = conn
            .query_row("SELECT owner_type FROM chunks WHERE id = 'c1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(owner_type, "memory-item");

        // get_chunks (unchanged) still reads the row back correctly.
        let chunks = store.get_chunks("item1").unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "a memory claim chunk");
    }
}
