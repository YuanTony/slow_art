CREATE TABLE IF NOT EXISTS artworks (
  id INTEGER PRIMARY KEY,
  audio_guide_id INTEGER,
  official_name TEXT NOT NULL,
  search_text TEXT NOT NULL DEFAULT '',
  description TEXT NOT NULL,
  description_level INTEGER NOT NULL DEFAULT 1
);

-- FTS5 full-text search index over searchable short fields.
-- External-content mode backed by the artworks table.
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

-- Embedding storage keyed by artwork ID.
-- Requires sqlite-vec extension loaded in-process.
CREATE VIRTUAL TABLE IF NOT EXISTS artwork_embeddings USING vec0(
  artwork_id integer primary key,
  embedding float[768]
);
