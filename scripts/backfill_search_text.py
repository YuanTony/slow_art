#!/usr/bin/env python3
"""Backfill search_text from Met Collection API for all artworks with empty search_text."""

import json
import sqlite3
import sys
import time
from urllib.parse import urlencode
from urllib.request import Request, urlopen


def fetch_text(url: str):
    req = Request(url, headers={"User-Agent": "ten-minute-art/1.0"})
    with urlopen(req, timeout=30) as resp:
        return resp.getcode(), resp.read().decode("utf-8", errors="replace")


def fetch_json(url: str):
    status, text = fetch_text(url)
    return status, json.loads(text)


def build_met_metadata_text(obj: dict) -> str:
    bits = []
    for key, label in [
        ("artistDisplayName", "Artist"),
        ("artistDisplayBio", "Artist bio"),
        ("artistNationality", "Artist nationality"),
        ("artistRole", "Artist role"),
        ("culture", "Culture"),
        ("objectDate", "Date"),
        ("period", "Period"),
        ("dynasty", "Dynasty"),
        ("reign", "Reign"),
        ("medium", "Medium"),
        ("dimensions", "Dimensions"),
        ("classification", "Classification"),
        ("department", "Department"),
        ("creditLine", "Credit line"),
        ("repository", "Repository"),
        ("objectURL", "Met collection page"),
    ]:
        val = (obj.get(key) or "").strip()
        if val:
            bits.append(f"{label}: {val}.")

    geography = ", ".join(
        x for x in [
            (obj.get("country") or "").strip(),
            (obj.get("region") or "").strip(),
            (obj.get("subregion") or "").strip(),
            (obj.get("locale") or "").strip(),
        ] if x
    )
    if geography:
        bits.append(f"Geographic context: {geography}.")

    tags = ", ".join(
        t.get("term", "") for t in (obj.get("tags") or []) if t.get("term")
    )
    if tags:
        bits.append(f"Tags/themes: {tags}.")

    return " ".join(bits)


def main():
    db_path = sys.argv[1] if len(sys.argv) > 1 else "artworks.db"
    conn = sqlite3.connect(db_path)
    cur = conn.cursor()

    rows = cur.execute(
        "SELECT id, official_name FROM artworks WHERE length(search_text) = 0 ORDER BY id"
    ).fetchall()

    print(f"[backfill] {len(rows)} artworks need search_text", file=sys.stderr)

    updated = 0
    failed = 0
    for i, (artwork_id, name) in enumerate(rows):
        try:
            status, obj = fetch_json(
                f"https://collectionapi.metmuseum.org/public/collection/v1/objects/{artwork_id}"
            )
            if status != 200:
                print(f"[backfill] {artwork_id} ({name}): API returned {status}", file=sys.stderr)
                failed += 1
                continue

            search_text = build_met_metadata_text(obj)
            if not search_text:
                print(f"[backfill] {artwork_id} ({name}): empty metadata", file=sys.stderr)
                failed += 1
                continue

            cur.execute("UPDATE artworks SET search_text = ? WHERE id = ?", (search_text, artwork_id))
            # Sync FTS
            cur.execute("DELETE FROM artworks_fts WHERE rowid = ?", (artwork_id,))
            cur.execute(
                "INSERT INTO artworks_fts(rowid, official_name, search_text) VALUES (?, ?, ?)",
                (artwork_id, name, search_text),
            )
            updated += 1

            if (i + 1) % 50 == 0:
                conn.commit()
                print(f"[backfill] progress: {i + 1}/{len(rows)}, updated: {updated}, failed: {failed}", file=sys.stderr)

        except Exception as exc:
            print(f"[backfill] {artwork_id} ({name}): error: {exc}", file=sys.stderr)
            failed += 1

        time.sleep(0.3)

    conn.commit()
    conn.close()
    print(f"[backfill] done: {updated} updated, {failed} failed out of {len(rows)}", file=sys.stderr)


if __name__ == "__main__":
    main()
