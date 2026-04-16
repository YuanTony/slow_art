"""Shared module: invoke an agent CLI (claude or codex) to research an artwork."""

import subprocess
import sys


RESEARCH_PROMPT_TEMPLATE = """\
You are a museum research assistant. Your task is to produce a deeply researched, comprehensive report on the following artwork from The Metropolitan Museum of Art.

## Artwork information

Title: {title}
Met collection page: {met_url}
{metadata_section}

## Instructions

PHASE 1 — EXTRACT MET PAGE CONTENT:
1. Visit the Met collection page linked above.
2. Extract ALL textual content from the page: the artwork description, provenance, exhibition history, references, and any other information the Met provides.
3. Find all image URLs on the Met page. Test each image URL by fetching it and confirming you get a valid response (HTTP 200). Record only the URLs that pass this test.

PHASE 2 — DEEP RESEARCH:
4. Search the internet — including Wikipedia, museum databases, academic sources, art history sites, and news — for additional information about this artwork.
5. Research and include multiple viewpoints on:
   - The origin and historical context of the artwork
   - Background on the artist or culture that created it
   - The materials, techniques, and craftsmanship involved
   - Different scholarly or critical interpretations of the artwork
   - Its significance within art history and within the Met's collection
   - Any interesting stories, provenance details, or controversies
6. Find additional public image URLs of this artwork from other sources (e.g. Wikimedia Commons, museum partner sites). You MUST test every image URL by fetching it and confirming you get a valid image response (HTTP 200 with an image content type) before including it. Do NOT include any URL that fails this test.

PHASE 3 — WRITE REPORT:
7. Write the report in plain text (no markdown formatting). Structure it as follows:
   a. MET PAGE CONTENT — the full extracted text from the Met collection page
   b. VERIFIED IMAGE URLS — all image URLs that passed testing, with source noted
   c. RESEARCH REPORT — your deep research findings organized with clear section headings
8. Be thorough and comprehensive — there is no length limit. Include all relevant details you can find.

Write ONLY the report as your response. No preamble, no meta-commentary.\
"""


SHALLOW_RESEARCH_PROMPT_TEMPLATE = """\
You are a museum research assistant. Your task is to produce a concise research summary on the following artwork from The Metropolitan Museum of Art.

## Artwork information

Title: {title}
Met collection page: {met_url}
{metadata_section}

## Instructions

PHASE 1 — EXTRACT MET PAGE CONTENT:
1. Visit the Met collection page linked above.
2. Extract ALL textual content from the page: the artwork description, provenance, exhibition history, references, and any other information the Met provides.
3. Find all image URLs on the Met page. Test each image URL by fetching it and confirming you get a valid response (HTTP 200). Record only the URLs that pass this test.

PHASE 2 — LIGHT RESEARCH:
4. Search Wikipedia for this artwork, its artist, or the culture that created it. Consult at most one or two additional reputable sources (e.g. museum databases, art history sites).
5. Focus on the most essential facts:
   - Who made it and when
   - What it depicts or represents
   - Why it matters (significance in art history or within the Met's collection)
   - One interesting story, detail, or lesser-known fact

PHASE 3 — WRITE REPORT:
6. Write the report in plain text (no markdown formatting). Structure it as follows:
   a. MET PAGE CONTENT — the full extracted text from the Met collection page
   b. VERIFIED IMAGE URLS — all image URLs that passed testing, with source noted
   c. RESEARCH SUMMARY — your research findings, concise and well-organized
7. Keep the report concise — aim for 2000-4000 characters in the research summary section.

Write ONLY the report as your response. No preamble, no meta-commentary.\
"""


def _build_metadata_section(met_metadata: str) -> str:
    if met_metadata:
        return f"Structured metadata: {met_metadata}"
    return ""


def build_research_prompt(title: str, met_url: str, met_metadata: str) -> str:
    return RESEARCH_PROMPT_TEMPLATE.format(
        title=title,
        met_url=met_url or "(not available)",
        metadata_section=_build_metadata_section(met_metadata),
    )


def build_shallow_research_prompt(title: str, met_url: str, met_metadata: str) -> str:
    return SHALLOW_RESEARCH_PROMPT_TEMPLATE.format(
        title=title,
        met_url=met_url or "(not available)",
        metadata_section=_build_metadata_section(met_metadata),
    )


def _run_agent(agent: str, title: str, prompt: str, timeout: int) -> str:
    """Invoke an agent CLI with a prompt. Returns the report text."""
    if agent == "claude":
        cmd = ["claude", "-p", "--dangerously-skip-permissions", prompt]
    elif agent == "codex":
        cmd = ["codex", "exec", "--full-auto", prompt]
    else:
        print(f"[research] unknown agent: {agent}", file=sys.stderr)
        return ""

    print(f"[research] invoking {agent} for: {title}", file=sys.stderr)
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        if result.returncode != 0:
            print(f"[research] {agent} exited with code {result.returncode}", file=sys.stderr)
            if result.stderr:
                print(f"[research] stderr: {result.stderr[:500]}", file=sys.stderr)
            return ""
        report = result.stdout.strip()
        print(f"[research] got {len(report)} chars from {agent}", file=sys.stderr)
        return report
    except subprocess.TimeoutExpired:
        print(f"[research] {agent} timed out after {timeout}s", file=sys.stderr)
        return ""
    except FileNotFoundError:
        print(f"[research] {agent} command not found — is it installed and on PATH?", file=sys.stderr)
        return ""
    except Exception as exc:
        print(f"[research] error running {agent}: {exc}", file=sys.stderr)
        return ""


def run_agent_research(agent: str, title: str, met_url: str, met_metadata: str) -> str:
    """Invoke an agent CLI for deep research on an artwork."""
    prompt = build_research_prompt(title, met_url, met_metadata)
    return _run_agent(agent, title, prompt, timeout=600)


def run_agent_shallow_research(agent: str, title: str, met_url: str, met_metadata: str) -> str:
    """Invoke an agent CLI for shallow (Wikipedia-level) research on an artwork."""
    prompt = build_shallow_research_prompt(title, met_url, met_metadata)
    return _run_agent(agent, title, prompt, timeout=180)
