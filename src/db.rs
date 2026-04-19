use anyhow::{Context, Result};
use rusqlite::{Connection, params};

use crate::types::Artwork;

#[cfg(feature = "search_image")]
use rusqlite::ffi::sqlite3_auto_extension;
#[cfg(feature = "search_image")]
use sqlite_vec::sqlite3_vec_init;

pub fn load_artworks(path: &str) -> Result<Vec<Artwork>> {
    let conn =
        Connection::open(path).with_context(|| format!("opening SQLite database at {path}"))?;
    ensure_schema(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT id, audio_guide_id, official_name, description, description_level, is_highlight, gallery_number FROM artworks ORDER BY official_name ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(Artwork {
            id: row.get(0)?,
            audio_guide_id: row.get(1)?,
            official_name: row.get(2)?,
            description: row.get(3)?,
            description_level: row.get(4)?,
            is_highlight: row.get::<_, i64>(5).unwrap_or(0) != 0,
            gallery_number: row.get::<_, String>(6).unwrap_or_default(),
        })
    })?;

    let mut artworks = Vec::new();
    for row in rows {
        artworks.push(row?);
    }
    Ok(artworks)
}

pub fn search_artworks_fts(path: &str, query: &str, limit: usize) -> Result<Vec<Artwork>> {
    let conn =
        Connection::open(path).with_context(|| format!("opening SQLite database at {path}"))?;
    ensure_schema(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT a.id, a.audio_guide_id, a.official_name, a.description, a.description_level, a.is_highlight, a.gallery_number
         FROM artworks_fts f
         JOIN artworks a ON a.id = f.rowid
         WHERE artworks_fts MATCH ?
         ORDER BY bm25(artworks_fts)
         LIMIT ?",
    )?;

    let rows = stmt.query_map(params![query, limit as i64], |row| {
        Ok(Artwork {
            id: row.get(0)?,
            audio_guide_id: row.get(1)?,
            official_name: row.get(2)?,
            description: row.get(3)?,
            description_level: row.get(4)?,
            is_highlight: row.get::<_, i64>(5).unwrap_or(0) != 0,
            gallery_number: row.get::<_, String>(6).unwrap_or_default(),
        })
    })?;

    let mut artworks = Vec::new();
    for row in rows {
        artworks.push(row?);
    }
    Ok(artworks)
}

#[cfg(feature = "search_image")]
#[allow(clippy::missing_transmute_annotations)]
pub fn register_sqlite_vec() {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }
}

#[cfg(not(feature = "search_image"))]
pub fn register_sqlite_vec() {}

#[cfg(feature = "search_image")]
pub fn search_artworks_by_embedding(
    path: &str,
    embedding: &[f32],
    limit: usize,
) -> Result<Vec<(Artwork, f64)>> {
    use zerocopy::IntoBytes;

    let conn =
        Connection::open(path).with_context(|| format!("opening SQLite database at {path}"))?;
    ensure_schema(&conn)?;

    let mut stmt = conn.prepare(
        "SELECT a.id, a.audio_guide_id, a.official_name, a.description, a.description_level, e.distance, a.is_highlight, a.gallery_number
         FROM artwork_embeddings e
         JOIN artworks a ON a.id = e.artwork_id
         WHERE e.embedding MATCH ?
           AND k = ?
         ORDER BY e.distance",
    )?;

    let rows = stmt.query_map(params![embedding.as_bytes(), limit as i64], |row| {
        Ok((
            Artwork {
                id: row.get(0)?,
                audio_guide_id: row.get(1)?,
                official_name: row.get(2)?,
                description: row.get(3)?,
                description_level: row.get(4)?,
                is_highlight: row.get::<_, i64>(6).unwrap_or(0) != 0,
                gallery_number: row.get::<_, String>(7).unwrap_or_default(),
            },
            row.get::<_, f64>(5)?,
        ))
    })?;

    let mut artworks = Vec::new();
    for row in rows {
        artworks.push(row?);
    }
    Ok(artworks)
}

#[cfg(not(feature = "search_image"))]
pub fn search_artworks_by_embedding(
    _path: &str,
    _embedding: &[f32],
    _limit: usize,
) -> Result<Vec<(Artwork, f64)>> {
    Ok(Vec::new())
}

fn ensure_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS artworks (
          id INTEGER PRIMARY KEY,
          audio_guide_id INTEGER,
          official_name TEXT NOT NULL,
          search_text TEXT NOT NULL DEFAULT '',
          description TEXT NOT NULL,
          description_level INTEGER NOT NULL DEFAULT 1
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS artworks_fts USING fts5(
          official_name,
          search_text,
          content='artworks',
          content_rowid='id'
        );

        CREATE TRIGGER IF NOT EXISTS artworks_ai AFTER INSERT ON artworks BEGIN
          INSERT INTO artworks_fts(rowid, official_name, search_text)
          VALUES (new.id, new.official_name, new.search_text);
        END;

        CREATE TRIGGER IF NOT EXISTS artworks_ad AFTER DELETE ON artworks BEGIN
          INSERT INTO artworks_fts(artworks_fts, rowid, official_name, search_text)
          VALUES ('delete', old.id, old.official_name, old.search_text);
        END;

        CREATE TRIGGER IF NOT EXISTS artworks_au AFTER UPDATE ON artworks BEGIN
          INSERT INTO artworks_fts(artworks_fts, rowid, official_name, search_text)
          VALUES ('delete', old.id, old.official_name, old.search_text);
          INSERT INTO artworks_fts(rowid, official_name, search_text)
          VALUES (new.id, new.official_name, new.search_text);
        END;
        "#,
    )
    .context("ensuring artworks + external-content fts tables exist")?;

    // Migration: add description_level column to existing databases
    conn.execute_batch(
        "ALTER TABLE artworks ADD COLUMN description_level INTEGER NOT NULL DEFAULT 1;",
    )
    .ok(); // swallows "duplicate column" error on re-runs

    // Mark existing artworks with deep research as level 3
    conn.execute(
        "UPDATE artworks SET description_level = 3 WHERE description_level = 1 AND length(description) > 2000",
        [],
    )
    .ok();

    // Migration: add is_highlight and gallery_number columns
    conn.execute_batch(
        "ALTER TABLE artworks ADD COLUMN is_highlight INTEGER NOT NULL DEFAULT 0;",
    )
    .ok();
    conn.execute_batch(
        "ALTER TABLE artworks ADD COLUMN gallery_number TEXT NOT NULL DEFAULT '';",
    )
    .ok();

    #[cfg(feature = "search_image")]
    conn.execute_batch(
        r#"
        CREATE VIRTUAL TABLE IF NOT EXISTS artwork_embeddings USING vec0(
          artwork_id integer primary key,
          embedding float[768]
        );
        "#,
    )
    .context("ensuring artwork_embeddings vec table exists")?;

    Ok(())
}
