#!/usr/bin/env python3
"""Validate image URLs in L3 artwork descriptions.

Checks all image URLs (images.metmuseum.org, .jpg, .jpeg, .png, .webp, iiif)
in L3 descriptions by sending HEAD requests. Removes dead URLs from descriptions.

Usage:
    python3 scripts/validate_image_urls.py [--db artworks.db] [--dry-run] [--timeout 10]
"""

import argparse
import re
import sqlite3
import sys
import time
from pathlib import Path
from urllib.request import Request, urlopen
from urllib.error import URLError, HTTPError

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DB = ROOT / "artworks.db"

IMAGE_URL_PATTERN = re.compile(
    r'https?://[^\s,;"\'\]\)}\|]+\.(?:jpg|jpeg|png|webp)'
    r'|https?://images\.metmuseum\.org[^\s,;"\'\]\)}\|]+'
    r'|https?://[^\s,;"\'\]\)}\|]*iiif[^\s,;"\'\]\)}\|]*',
    re.IGNORECASE,
)


def check_url(url: str, timeout: int = 10) -> bool:
    """Return True if the URL is reachable.

    Only treats HTTP 404 (Not Found) and 410 (Gone) as truly dead.
    429 (rate limited), 403, etc. are treated as alive — likely transient or
    server-side rejection of HEAD requests, not a missing resource.
    Tries HEAD first, then falls back to GET with a browser User-Agent.
    """
    safe_url = url.encode("ascii", errors="ignore").decode("ascii")
    if not safe_url or len(safe_url) < 10:
        return False

    user_agent = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36"

    # Try HEAD first
    for method in ("HEAD", "GET"):
        try:
            req = Request(safe_url, method=method, headers={"User-Agent": user_agent})
            with urlopen(req, timeout=timeout) as resp:
                return resp.getcode() == 200
        except HTTPError as e:
            # Only 404 and 410 mean the resource is truly missing
            if e.code in (404, 410):
                return False
            # Any other HTTP error code (429, 403, 500, etc.) — treat as alive
            return True
        except (URLError, TimeoutError, OSError, UnicodeError, ValueError):
            # Network error — try the next method
            continue
    # Both HEAD and GET failed with network errors — treat as alive (transient)
    return True


def extract_image_urls(description: str) -> list[str]:
    """Extract all image URLs from a description."""
    urls = IMAGE_URL_PATTERN.findall(description)
    # Deduplicate while preserving order
    seen = set()
    result = []
    for url in urls:
        # Clean trailing punctuation
        url = url.rstrip(".,;:)]}\"'")
        if url not in seen:
            seen.add(url)
            result.append(url)
    return result


def remove_url_from_description(description: str, dead_url: str) -> str:
    """Remove a dead URL from the description text."""
    # Remove the URL and any surrounding whitespace
    result = description.replace(dead_url, "")
    # Clean up double spaces
    result = re.sub(r"  +", " ", result)
    return result


def main():
    parser = argparse.ArgumentParser(description="Validate image URLs in L3 artwork descriptions")
    parser.add_argument("--db", default=str(DEFAULT_DB))
    parser.add_argument("--dry-run", action="store_true", help="Report dead URLs without modifying the database")
    parser.add_argument("--timeout", type=int, default=10, help="HTTP timeout in seconds (default: 10)")
    args = parser.parse_args()

    conn = sqlite3.connect(args.db)
    rows = conn.execute(
        "SELECT id, official_name, description FROM artworks WHERE description_level = 3 ORDER BY id"
    ).fetchall()

    total_artworks = len(rows)
    total_urls = 0
    dead_urls = 0
    artworks_with_dead = 0
    artworks_updated = 0

    print(f"Checking image URLs in {total_artworks} L3 artworks...")
    print()

    for i, (art_id, name, description) in enumerate(rows, 1):
        urls = extract_image_urls(description)
        if not urls:
            continue

        total_urls += len(urls)
        dead_in_artwork = []

        for url in urls:
            if not check_url(url, timeout=args.timeout):
                dead_in_artwork.append(url)
                dead_urls += 1

        if dead_in_artwork:
            artworks_with_dead += 1
            print(f"[{i}/{total_artworks}] {name} (ID {art_id}): {len(dead_in_artwork)} dead URL(s)")
            for url in dead_in_artwork:
                print(f"  DEAD: {url}")

            if not args.dry_run:
                updated = description
                for url in dead_in_artwork:
                    updated = remove_url_from_description(updated, url)
                conn.execute(
                    "UPDATE artworks SET description = ? WHERE id = ?",
                    (updated, art_id),
                )
                artworks_updated += 1

        # Progress every 100 artworks
        if i % 100 == 0:
            print(f"  --- Progress: {i}/{total_artworks} artworks checked, {dead_urls} dead URLs found ---")

        # Rate limit to avoid hammering the server
        if urls:
            time.sleep(0.2)

    if not args.dry_run and artworks_updated > 0:
        conn.commit()

    conn.close()

    print()
    print(f"=== Summary ===")
    print(f"Artworks checked: {total_artworks}")
    print(f"Total image URLs: {total_urls}")
    print(f"Dead URLs found: {dead_urls}")
    print(f"Artworks with dead URLs: {artworks_with_dead}")
    if args.dry_run:
        print(f"(dry run — no changes made)")
    else:
        print(f"Artworks updated: {artworks_updated}")


if __name__ == "__main__":
    main()
