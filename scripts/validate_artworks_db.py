#!/usr/bin/env python3
import sqlite3
import sys

DB = "artworks.db"

conn = sqlite3.connect(DB)
cur = conn.cursor()
cur.execute("SELECT name FROM sqlite_master WHERE type='table' AND name='artworks'")
if not cur.fetchone():
    print("missing artworks table", file=sys.stderr)
    sys.exit(1)

cur.execute("SELECT id, audio_guide_id, official_name, description FROM artworks ORDER BY official_name")
rows = cur.fetchall()
if not rows:
    print("empty artworks table", file=sys.stderr)
    sys.exit(1)

for row in rows:
    object_id, audio_guide_id, official_name, description = row
    if not all([official_name, description]):
        print(f"row {object_id} has empty required field", file=sys.stderr)
        sys.exit(1)

print(f"OK: validated {len(rows)} artworks in simplified schema")
