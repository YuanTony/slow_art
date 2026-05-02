#!/usr/bin/env python3
"""Upgrade all L1 artworks in artworks.db to L3 (deep research).

Processes artworks in batches, logs progress to a file, and can be resumed
after interruption (already-upgraded artworks are skipped automatically).

Usage:
    python3 scripts/upgrade_l1_to_l3.py [--db artworks.db] [--research-agent codex] [--dry-run] [--limit N]
"""

import argparse
import json
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DB = ROOT / "artworks.db"
VENV_PYTHON = ROOT / ".venv" / "bin" / "python3"
LOG_FILE = ROOT / "l1_to_l3_progress.log"


def get_l1_artworks(db_path: str) -> list[tuple[int, str]]:
    conn = sqlite3.connect(db_path)
    rows = conn.execute(
        "SELECT id, official_name FROM artworks WHERE description_level = 1 ORDER BY id"
    ).fetchall()
    conn.close()
    return rows


def log(msg: str):
    timestamp = time.strftime("%Y-%m-%d %H:%M:%S")
    line = f"[{timestamp}] {msg}"
    print(line)
    with open(LOG_FILE, "a") as f:
        f.write(line + "\n")


def main():
    parser = argparse.ArgumentParser(description="Upgrade L1 artworks to L3 deep research")
    parser.add_argument("--db", default=str(DEFAULT_DB))
    parser.add_argument("--research-agent", default="codex", choices=["claude", "codex"])
    parser.add_argument("--dry-run", action="store_true", help="List artworks without running research")
    parser.add_argument("--limit", type=int, default=0, help="Process at most N artworks (0 = all)")
    args = parser.parse_args()

    artworks = get_l1_artworks(args.db)
    if not artworks:
        print("No L1 artworks found.")
        return

    if args.limit > 0:
        artworks = artworks[:args.limit]

    print(f"Found {len(artworks)} L1 artwork(s) to upgrade.")

    if args.dry_run:
        for object_id, name in artworks:
            print(f"  {object_id}: {name}")
        return

    log(f"Starting L1→L3 upgrade: {len(artworks)} artworks, agent={args.research_agent}")

    python = str(VENV_PYTHON) if VENV_PYTHON.exists() else sys.executable
    succeeded = 0
    failed = 0
    skipped = 0

    for i, (object_id, name) in enumerate(artworks, 1):
        # Re-check current level in case it was already upgraded (resume support)
        conn = sqlite3.connect(args.db)
        current_level = conn.execute(
            "SELECT description_level FROM artworks WHERE id = ?", (object_id,)
        ).fetchone()
        conn.close()
        if current_level and current_level[0] >= 3:
            log(f"[{i}/{len(artworks)}] SKIP (already L3): {name} (ID {object_id})")
            skipped += 1
            continue

        log(f"[{i}/{len(artworks)}] Researching: {name} (ID {object_id})")
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
                # Only print research-related stderr, skip metadata loading noise
                for line in result.stderr.splitlines():
                    if not line.startswith("[metadata]"):
                        print(f"  stderr: {line}", file=sys.stderr)
            output = result.stdout.strip()
            try:
                parsed = json.loads(output)
            except json.JSONDecodeError:
                parsed = {"raw_output": output[:500]}

            status = parsed.get("status", "unknown")
            chars = parsed.get("research_chars", 0)

            if status == "inserted" and chars > 0:
                log(f"  -> OK: {chars} research chars")
                succeeded += 1
            else:
                log(f"  -> {status} (research: {chars} chars, returncode: {result.returncode})")
                if status == "inserted":
                    succeeded += 1
                else:
                    failed += 1

        except subprocess.TimeoutExpired:
            log(f"  -> TIMED OUT after 720s")
            failed += 1
        except Exception as exc:
            log(f"  -> ERROR: {exc}")
            failed += 1

        # Progress summary every 50 artworks
        if i % 50 == 0:
            log(f"  --- Progress: {succeeded} succeeded, {failed} failed, {skipped} skipped out of {i} processed ---")

    log(f"=== DONE: {succeeded} succeeded, {failed} failed, {skipped} skipped out of {len(artworks)} total ===")


if __name__ == "__main__":
    main()
