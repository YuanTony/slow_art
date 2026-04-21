#!/usr/bin/env python3
"""Upgrade all L2 artworks in artworks.db to L3 (deep research).

Usage:
    python3 scripts/upgrade_l2_to_l3.py [--db artworks.db] [--research-agent claude|codex] [--dry-run]
"""

import argparse
import json
import sqlite3
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DB = ROOT / "artworks.db"
VENV_PYTHON = ROOT / ".venv" / "bin" / "python3"


def get_l2_artworks(db_path: str) -> list[tuple[int, str]]:
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT id, official_name FROM artworks WHERE description_level = 2 ORDER BY id"
    ).fetchall()
    conn.close()
    return rows


def main():
    parser = argparse.ArgumentParser(description="Upgrade L2 artworks to L3 deep research")
    parser.add_argument("--db", default=str(DEFAULT_DB))
    parser.add_argument("--research-agent", default="claude", choices=["claude", "codex"])
    parser.add_argument("--dry-run", action="store_true", help="List artworks without running research")
    args = parser.parse_args()

    artworks = get_l2_artworks(args.db)
    if not artworks:
        print("No L2 artworks found.")
        return

    print(f"Found {len(artworks)} L2 artwork(s) to upgrade:\n")
    for object_id, name in artworks:
        print(f"  {object_id}: {name}")
    print()

    if args.dry_run:
        return

    results = []
    for i, (object_id, name) in enumerate(artworks, 1):
        print(f"[{i}/{len(artworks)}] Researching: {name} (ID {object_id})")
        python = str(VENV_PYTHON) if VENV_PYTHON.exists() else sys.executable
        cmd = [
            python,
            str(ROOT / "scripts" / "add_met_artwork_by_object_id.py"),
            "--object-id", str(object_id),
            "--level", "3",
            "--force",
            "--no-embedding",
            "--research-agent", args.research_agent,
            "--db", args.db,
        ]
        try:
            result = subprocess.run(cmd, capture_output=True, text=True, timeout=720)
            if result.stderr:
                print(result.stderr, file=sys.stderr)
            output = result.stdout.strip()
            try:
                parsed = json.loads(output)
            except json.JSONDecodeError:
                parsed = {"raw_output": output[:500]}
            parsed["object_id"] = object_id
            parsed["name"] = name
            parsed["returncode"] = result.returncode
            results.append(parsed)
            status = parsed.get("status", "unknown")
            chars = parsed.get("research_chars", 0)
            print(f"  -> {status} (research: {chars} chars)\n")
        except subprocess.TimeoutExpired:
            print(f"  -> TIMED OUT\n")
            results.append({"object_id": object_id, "name": name, "status": "timeout"})
        except Exception as exc:
            print(f"  -> ERROR: {exc}\n")
            results.append({"object_id": object_id, "name": name, "status": "error", "error": str(exc)})

    print("\n=== Summary ===")
    succeeded = sum(1 for r in results if r.get("status") == "inserted")
    failed = len(results) - succeeded
    print(f"Upgraded: {succeeded}/{len(results)}")
    if failed:
        print(f"Failed:   {failed}")
        for r in results:
            if r.get("status") != "inserted":
                print(f"  {r['object_id']}: {r['name']} — {r.get('status', 'unknown')}")


if __name__ == "__main__":
    main()
