#!/usr/bin/env python3
import argparse
import json
import re
import sqlite3
import time
from pathlib import Path
from urllib.error import URLError, HTTPError
from urllib.parse import urlencode
from urllib.request import Request, urlopen

from research_artwork import run_agent_research, run_agent_shallow_research
from hf_embeddings import lookup_embedding, insert_embedding, enable_vec
from hf_metadata import lookup_display_info
from add_met_artwork_by_object_id import strip_dead_urls

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_DB = ROOT / "artworks.db"
DEFAULT_OUT_DIR = ROOT / "output"

NON_ARTWORK_HINTS = [
    "welcome",
    "introduction",
    "intro",
    "tour",
    "playlist",
    "director",
    "next stop",
    "download today",
    "bloomberg connects",
]


def fetch_text(url: str):
    req = Request(url, headers={"User-Agent": "ten-minute-art/1.0"})
    with urlopen(req, timeout=30) as resp:
        return resp.getcode(), resp.read().decode("utf-8", errors="replace")


def fetch_json(url: str, params=None):
    if params:
        url = f"{url}?{urlencode(params)}"
    status, text = fetch_text(url)
    return status, json.loads(text)


def html_title(html: str):
    start = html.find("<title>")
    if start == -1:
        return None
    start += 7
    end = html.find("</title>", start)
    if end == -1:
        return None
    title = html[start:end].split(" - The Metropolitan Museum of Art")[0].strip()
    return title


def strip_html(html: str):
    out = []
    in_tag = False
    for ch in html:
        if ch == '<':
            in_tag = True
        elif ch == '>':
            in_tag = False
            out.append(' ')
        elif not in_tag:
            out.append(ch)
    return " ".join("".join(out).split())


def extract_met_full_text(html: str):
    return strip_html(html)



def extract_crd_id(html: str):
    """Extract the Met collection object ID (crdId) from audio guide HTML."""
    match = re.search(r'\\?"crdId\\?"\s*:\s*\\?"(\d+)\\?"', html)
    return int(match.group(1)) if match else None


def collect_met_image_urls(data: dict) -> list[str]:
    """Extract all image URLs from a Met API response."""
    urls = []
    for key in ["primaryImage", "primaryImageSmall"]:
        url = (data.get(key) or "").strip()
        if url:
            urls.append(url)
    for url in data.get("additionalImages") or []:
        url = (url or "").strip()
        if url:
            urls.append(url)
    seen = set()
    return [u for u in urls if not (u in seen or seen.add(u))]


def fetch_met_collection_metadata(object_id: int):
    """Fetch structured metadata from the Met Collection API for a given object ID.

    Returns (metadata_text, met_image_urls).
    """
    url = f"https://collectionapi.metmuseum.org/public/collection/v1/objects/{object_id}"
    try:
        status, data = fetch_json(url)
        if status != 200:
            return "", []
    except Exception:
        return "", []

    image_urls = collect_met_image_urls(data)

    bits = []
    for key, label in [
        ("artistDisplayName", "Artist"),
        ("culture", "Culture"),
        ("objectDate", "Date"),
        ("period", "Period"),
        ("dynasty", "Dynasty"),
        ("medium", "Medium"),
        ("dimensions", "Dimensions"),
        ("classification", "Classification"),
        ("department", "Department"),
        ("creditLine", "Credit line"),
        ("repository", "Repository"),
        ("objectURL", "Met collection page"),
    ]:
        val = (data.get(key) or "").strip()
        if val:
            bits.append(f"{label}: {val}.")

    geography = ", ".join(
        x for x in [
            (data.get("country") or "").strip(),
            (data.get("region") or "").strip(),
            (data.get("subregion") or "").strip(),
        ] if x
    )
    if geography:
        bits.append(f"Geographic context: {geography}.")

    tags = ", ".join(
        t.get("term", "") for t in (data.get("tags") or [])[:12] if t.get("term")
    )
    if tags:
        bits.append(f"Tags/themes: {tags}.")

    return " ".join(bits), image_urls


def looks_like_artwork(title: str, met_text: str):
    low_title = title.lower()
    low_text = met_text.lower()[:1200]
    for hint in NON_ARTWORK_HINTS:
        if hint in low_title:
            return False, f"title contains non-artwork hint: {hint}"
    if title.lower().startswith("the highlights tour"):
        return False, "playlist intro page"
    if "next stop" in low_text and len(title.split()) <= 4:
        return False, "looks like navigation/introduction text"
    return True, "looks like specific artwork or architectural artifact"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--audio-id", type=int, required=True, help="Single Met audio guide ID to fetch")
    parser.add_argument("--db", default=str(DEFAULT_DB))
    parser.add_argument("--out-dir", default=str(DEFAULT_OUT_DIR))
    parser.add_argument("--research-agent", default="claude", choices=["claude", "codex"],
                        help="Agent CLI to use for deep research (default: claude)")
    parser.add_argument("--level", type=int, default=1, choices=[1, 2, 3],
                        help="Description level: 1=metadata only, 2=shallow research, 3=deep research (default: 1)")
    parser.add_argument("--no-research", action="store_true",
                        help="Skip agent research (equivalent to --level 1)")
    parser.add_argument("--sleep", type=float, default=1.5)
    parser.add_argument("--force", action="store_true", help="Replace existing row for this audio guide ID")
    parser.add_argument("--embeddings-dir", default=None,
                        help="Directory containing embedding parquet files (default: data/embeddings/)")
    parser.add_argument("--no-embedding", action="store_true",
                        help="Skip embedding lookup")
    args = parser.parse_args()

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)
    out_file = out_dir / f"audio_{args.audio_id}.json"

    url = f"https://www.metmuseum.org/audio-guide/{args.audio_id}"
    result = {"audio_guide_id": args.audio_id, "url": url}

    conn = sqlite3.connect(args.db)
    cur = conn.cursor()

    time.sleep(args.sleep)
    try:
        status, html = fetch_text(url)
    except (URLError, HTTPError, TimeoutError) as exc:
        result.update({"status": "error", "error": str(exc)})
        out_file.write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps(result, indent=2))
        return

    result["http_status"] = status
    if status != 200:
        result.update({"status": "not_found"})
        out_file.write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps(result, indent=2))
        return

    title = html_title(html) or f"Audio Stop {args.audio_id}"
    met_text = extract_met_full_text(html)
    crd_id = extract_crd_id(html)
    if crd_id:
        met_object_meta, met_image_urls = fetch_met_collection_metadata(crd_id)
    else:
        met_object_meta, met_image_urls = "", []
    if crd_id:
        hl, gn = lookup_display_info(crd_id)
        is_highlight = 1 if hl else 0
        gallery_number = gn
    else:
        is_highlight, gallery_number = 0, ""
    is_artwork, filter_reason = looks_like_artwork(title, met_text)

    result.update({
        "title": title,
        "filter_reason": filter_reason,
        "met_object_id": crd_id,
    })

    if not is_artwork:
        result.update({"status": "filtered_out"})
        out_file.write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps(result, indent=2))
        return

    if len(met_text) < 1000:
        result.update({"status": "filtered_out", "filter_reason": f"met text too short ({len(met_text)} chars)"})
        out_file.write_text(json.dumps(result, indent=2), encoding="utf-8")
        print(json.dumps(result, indent=2))
        return

    # Build the Met collection page URL for the agent
    met_collection_url = f"https://www.metmuseum.org/art/collection/search/{crd_id}" if crd_id else url

    if args.no_research:
        args.level = 1

    research_report = ""
    if args.level == 2:
        research_report = run_agent_shallow_research(
            agent=args.research_agent,
            title=title,
            met_url=met_collection_url,
            met_metadata=met_object_meta,
        )
    elif args.level == 3:
        research_report = run_agent_research(
            agent=args.research_agent,
            title=title,
            met_url=met_collection_url,
            met_metadata=met_object_meta,
        )

    description_parts = [
        f"Met audio guide stop {args.audio_id}.",
        f"Title: {title}.",
    ]
    if met_object_meta:
        description_parts.append(met_object_meta)
    if research_report:
        prefix = "Deep research report:" if args.level == 3 else "Research report:"
        description_parts.append(f"{prefix} {research_report}")
    if met_image_urls:
        description_parts.append("Met image URLs: " + " ".join(met_image_urls))
    description = " ".join(description_parts).strip()

    # Validate all URLs and strip dead ones
    description, dead_count = strip_dead_urls(description)
    if dead_count:
        print(f"[validate] stripped {dead_count} dead URL(s) from description", file=sys.stderr)

    search_text = met_object_meta

    row_id = crd_id if crd_id is not None else None
    if crd_id is not None:
        cur.execute(
            """
            INSERT INTO artworks (id, audio_guide_id, official_name, search_text, description, description_level, is_highlight, gallery_number)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(id) DO UPDATE SET
              audio_guide_id = excluded.audio_guide_id,
              official_name = excluded.official_name,
              search_text = excluded.search_text,
              description = excluded.description,
              description_level = excluded.description_level,
              is_highlight = excluded.is_highlight,
              gallery_number = excluded.gallery_number
            """,
            (crd_id, args.audio_id, title, search_text, description, args.level, is_highlight, gallery_number),
        )
    else:
        if args.force:
            cur.execute(
                "UPDATE artworks SET official_name = ?, search_text = ?, description = ?, description_level = ?, is_highlight = ?, gallery_number = ? WHERE audio_guide_id = ?",
                (title, search_text, description, args.level, is_highlight, gallery_number, args.audio_id),
            )
            if cur.rowcount == 0:
                cur.execute(
                    "INSERT INTO artworks (audio_guide_id, official_name, search_text, description, description_level, is_highlight, gallery_number) VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (args.audio_id, title, search_text, description, args.level, is_highlight, gallery_number),
                )
                row_id = cur.lastrowid
            else:
                cur.execute("SELECT id FROM artworks WHERE audio_guide_id = ? LIMIT 1", (args.audio_id,))
                existing = cur.fetchone()
                row_id = existing[0] if existing else None
        else:
            cur.execute(
                "INSERT INTO artworks (audio_guide_id, official_name, search_text, description, description_level, is_highlight, gallery_number) VALUES (?, ?, ?, ?, ?, ?, ?)",
                (args.audio_id, title, search_text, description, args.level, is_highlight, gallery_number),
            )
            row_id = cur.lastrowid

    # FTS stays in sync via artworks triggers.

    # Look up and insert pre-computed embedding (only if we have a Met object ID)
    embedding_stored = False
    if not args.no_embedding and crd_id is not None:
        embedding = lookup_embedding(crd_id, embeddings_dir=args.embeddings_dir)
        if embedding:
            enable_vec(conn)
            insert_embedding(cur, crd_id, embedding)
            embedding_stored = True

    conn.commit()
    conn.close()

    result.update({
        "status": "inserted",
        "row_id": row_id,
        "met_object_id": crd_id,
        "met_collection_chars": len(met_object_meta),
        "research_chars": len(research_report),
        "description_level": args.level,
        "embedding_stored": embedding_stored,
    })
    out_file.write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
