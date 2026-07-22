#!/usr/bin/env python3
"""Generate or verify the synthetic Phase 3 capture fixture manifest."""

import argparse
import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "spec" / "fixtures" / "vision" / "capture"


def generated_manifest():
    return {
        "version": 1,
        "license": "GPL-3.0-only",
        "generator": "tools/generate_vision_capture_fixtures.py",
        "provenance": (
            "Repository-owned deterministic codec BMP reserved for a future bounded "
            "file-capture qualification; current native capture admission performs no "
            "path access and no physical capture device is represented."
        ),
        "native_admission": {
            "file": "unsupported-before-path-access",
            "directshow": "unsupported-before-device-access",
            "media_foundation": "unsupported-before-device-access",
        },
        "sequence": {
            "pattern": "frame-%02d.bmp",
            "frame_count": 2,
            "source_fixture": "../codec/bgr-2x2.bmp.hex",
            "encoded_bytes_per_frame": 70,
            "encoded_sha256": (
                "32595ac4ac54ae42c4f31d77fce001599dc10f5452f7c2de5482f0ed5f0a074d"
            ),
            "width": 2,
            "height": 2,
            "stride": 6,
            "pixel_format": "BGR8",
            "decoded_sha256": (
                "3d335acc3b7c9d3edcf42098e24ad875a2c0ba87223c640b1d535be39677009c"
            ),
        },
        "hardware_support": [],
    }


def encoded_manifest():
    return (json.dumps(generated_manifest(), indent=2, ensure_ascii=True) + "\n").encode(
        "utf-8"
    )


def check(expected):
    path = FIXTURE_DIR / "manifest.json"
    try:
        actual = path.read_bytes()
    except OSError as error:
        print(error, file=sys.stderr)
        return 1
    if actual != expected:
        print("capture manifest is not reproducible from its generator", file=sys.stderr)
        return 1
    extra = sorted(item.name for item in FIXTURE_DIR.iterdir() if item.name != "manifest.json")
    if extra:
        print("capture fixture directory has unexpected files: {}".format(extra), file=sys.stderr)
        return 1
    print("validated synthetic vision capture manifest")
    return 0


def write(expected):
    FIXTURE_DIR.mkdir(parents=True, exist_ok=True)
    (FIXTURE_DIR / "manifest.json").write_bytes(expected)
    print("generated synthetic vision capture manifest")
    return 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    arguments = parser.parse_args()
    expected = encoded_manifest()
    return check(expected) if arguments.check else write(expected)


if __name__ == "__main__":
    sys.exit(main())
