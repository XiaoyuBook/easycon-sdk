#!/usr/bin/env python3
"""Generate or verify the project-authored Phase 3 OCR image fixture."""

import argparse
import hashlib
import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "spec" / "fixtures" / "vision" / "ocr"
TEXT = "EASCON"
SCALE = 7
MARGIN = 14
GLYPHS = {
    "A": ("01110", "10001", "10001", "11111", "10001", "10001", "10001"),
    "C": ("01111", "10000", "10000", "10000", "10000", "10000", "01111"),
    "E": ("11111", "10000", "10000", "11110", "10000", "10000", "11111"),
    "N": ("10001", "11001", "11001", "10101", "10011", "10011", "10001"),
    "O": ("01110", "10001", "10001", "10001", "10001", "10001", "01110"),
    "S": ("01111", "10000", "10000", "01110", "00001", "00001", "11110"),
}


def render():
    glyph_width = 5 * SCALE
    spacing = SCALE
    width = MARGIN * 2 + len(TEXT) * glyph_width + (len(TEXT) - 1) * spacing
    height = MARGIN * 2 + 7 * SCALE
    pixels = bytearray([255] * (width * height))
    x = MARGIN
    for character in TEXT:
        glyph = GLYPHS[character]
        for glyph_y, row in enumerate(glyph):
            for glyph_x, value in enumerate(row):
                if value != "1":
                    continue
                for dy in range(SCALE):
                    start = (MARGIN + glyph_y * SCALE + dy) * width + x + glyph_x * SCALE
                    pixels[start:start + SCALE] = b"\x00" * SCALE
        x += glyph_width + spacing
    return width, height, bytes(pixels)


def generated_files():
    width, height, pixels = render()
    digest = hashlib.sha256(pixels).hexdigest()
    manifest = {
        "version": 1,
        "license": "GPL-3.0-only",
        "generator": "tools/generate_vision_ocr_fixtures.py",
        "provenance": (
            "Synthetic 5x7 glyph pixels authored for EasyCon SDK Phase 3; "
            "no external font or EasyCon asset is used."
        ),
        "text": TEXT,
        "image": {
            "path": "easycon-gray.hex",
            "format": "Gray8",
            "width": width,
            "height": height,
            "stride": width,
            "decoded_bytes": len(pixels),
            "decoded_sha256": digest,
        },
    }
    return {
        "easycon-gray.hex": pixels.hex().encode("ascii") + b"\n",
        "manifest.json": (
            json.dumps(manifest, indent=2, ensure_ascii=True) + "\n"
        ).encode("utf-8"),
    }


def check(files):
    expected = set(files)
    actual = {path.name for path in FIXTURE_DIR.iterdir() if path.is_file()}
    failures = []
    if expected != actual:
        failures.append(
            "fixture file set differs: missing={}, extra={}".format(
                sorted(expected - actual), sorted(actual - expected)
            )
        )
    for name, content in files.items():
        path = FIXTURE_DIR / name
        try:
            observed = path.read_bytes()
        except OSError as error:
            failures.append("{}: {}".format(path, error))
            continue
        if observed != content:
            failures.append("{} is not reproducible from the tracked generator".format(path))
    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1
    print("validated 1 generated OCR image fixture and its manifest")
    return 0


def write(files):
    FIXTURE_DIR.mkdir(parents=True, exist_ok=True)
    for name, content in files.items():
        (FIXTURE_DIR / name).write_bytes(content)
    print("generated 1 OCR image fixture and its manifest")
    return 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    files = generated_files()
    return check(files) if arguments.check else write(files)


if __name__ == "__main__":
    sys.exit(main())
