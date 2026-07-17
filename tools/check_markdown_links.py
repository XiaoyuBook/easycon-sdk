#!/usr/bin/env python3
"""Check local Markdown file targets and heading anchors without network access."""

import re
import sys
import unicodedata
from pathlib import Path
from urllib.parse import unquote, urlparse


ROOT = Path(__file__).resolve().parents[1]
EXCLUDED_PARTS = {".git", ".tools", "artifacts", "EasyCon", "target"}
INLINE_LINK = re.compile(r"!?\[[^\]]*\]\(([^)]+)\)")
REFERENCE_LINK = re.compile(r"^\s*\[[^\]]+\]:\s*(\S+)", re.MULTILINE)
HEADING = re.compile(r"^#{1,6}\s+(.+?)\s*#*\s*$", re.MULTILINE)
EXPLICIT_ANCHOR = re.compile(r"<a\s+(?:name|id)=[\"']([^\"']+)[\"']", re.IGNORECASE)


def markdown_files():
    return sorted(
        path
        for path in ROOT.rglob("*.md")
        if not any(part in EXCLUDED_PARTS for part in path.relative_to(ROOT).parts)
    )


def heading_anchors(path):
    text = path.read_text(encoding="utf-8")
    anchors = set(EXPLICIT_ANCHOR.findall(text))
    occurrences = {}
    for heading in HEADING.findall(text):
        base = github_slug(heading)
        if not base:
            continue
        index = occurrences.get(base, 0)
        occurrences[base] = index + 1
        anchors.add(base if index == 0 else "{}-{}".format(base, index))
    return anchors


def github_slug(heading):
    heading = re.sub(r"<[^>]+>", "", heading)
    heading = re.sub(r"!?\[([^\]]+)\]\([^)]*\)", r"\1", heading)
    heading = heading.replace("`", "").replace("*", "").replace("_", "_")
    output = []
    for character in heading.strip().lower():
        category = unicodedata.category(character)
        if character.isspace():
            output.append("-")
        elif character in "-_" or category[0] in {"L", "M", "N"}:
            output.append(character)
    return "".join(output)


def extract_destination(raw):
    raw = raw.strip()
    if raw.startswith("<") and ">" in raw:
        return raw[1 : raw.index(">")]
    return raw.split(None, 1)[0]


def validate_link(source, destination, anchor_cache):
    destination = unquote(extract_destination(destination))
    parsed = urlparse(destination)
    if parsed.scheme or destination.startswith("//"):
        return None
    path_text, separator, anchor = destination.partition("#")
    target = source if not path_text else (source.parent / path_text).resolve()
    try:
        target.relative_to(ROOT)
    except ValueError:
        return "target escapes repository: {}".format(destination)
    if not target.exists():
        return "missing target: {}".format(destination)
    if separator and anchor:
        if target.is_dir():
            target = target / "README.md"
        if target.suffix.lower() != ".md":
            return "anchor points to non-Markdown file: {}".format(destination)
        anchors = anchor_cache.setdefault(target, heading_anchors(target))
        if anchor not in anchors:
            return "missing anchor #{} in {}".format(anchor, target.relative_to(ROOT))
    return None


def main():
    files = markdown_files()
    anchor_cache = {}
    failures = []
    checked = 0
    for source in files:
        text = source.read_text(encoding="utf-8")
        destinations = INLINE_LINK.findall(text) + REFERENCE_LINK.findall(text)
        for destination in destinations:
            checked += 1
            failure = validate_link(source, destination, anchor_cache)
            if failure:
                failures.append("{}: {}".format(source.relative_to(ROOT), failure))
    if failures:
        print("Markdown link check failed:", file=sys.stderr)
        for failure in failures:
            print("  " + failure, file=sys.stderr)
        return 1
    print("validated {} local/remote Markdown link references across {} files".format(checked, len(files)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
