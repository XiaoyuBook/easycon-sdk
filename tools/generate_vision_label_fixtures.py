#!/usr/bin/env python3
"""Generate or verify the synthetic Phase 3 legacy .IL parser corpus."""

import argparse
import base64
import hashlib
import json
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "spec" / "fixtures" / "vision" / "labels"
CODEC_DIR = ROOT / "spec" / "fixtures" / "vision" / "codec"


def read_hex(name):
    return bytes.fromhex((CODEC_DIR / name).read_text(encoding="ascii").strip())


def label_fields(method=5, image="", **overrides):
    fields = {
        "searchMethod": method,
        "ImgBase64": image,
        "RangeX": 0,
        "RangeY": 0,
        "RangeWidth": 4,
        "RangeHeight": 4,
        "TargetX": 1,
        "TargetY": 1,
        "TargetWidth": 2,
        "TargetHeight": 2,
    }
    fields.update(overrides)
    return fields


def json_bytes(fields):
    return (json.dumps(fields, ensure_ascii=True, separators=(",", ":")) + "\n").encode(
        "utf-8"
    )


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def generated_files():
    bmp = base64.b64encode(read_hex("bgr-2x2.bmp.hex")).decode("ascii")
    png = base64.b64encode(read_hex("bgr-2x2.png.hex")).decode("ascii")
    valid_png = label_fields(3, png)
    valid_ocr = label_fields(
        107,
        "EASCON",
        RangeWidth=273,
        RangeHeight=77,
        TargetX=0,
        TargetY=0,
        TargetWidth=273,
        TargetHeight=77,
    )

    default_method = dict(valid_png)
    default_method.pop("searchMethod")
    unknown_field = dict(valid_png)
    unknown_field["FutureField"] = {"nested": [1, 2, 3]}
    duplicate_key = json_bytes(valid_png).replace(
        b'"RangeX":0,', b'"RangeX":0,"RangeX":1,', 1
    )
    invalid_utf8 = json_bytes(valid_ocr).replace(b"EASCON", b"EAS\xffCON", 1)

    payloads = {
        "valid-bmp.IL": json_bytes(label_fields(5, bmp)),
        "valid-png.IL": json_bytes(valid_png),
        "valid-ocr.IL": json_bytes(valid_ocr),
        "default-method.IL": json_bytes(default_method),
        "unknown-field.IL": json_bytes(unknown_field),
        "unknown-method.IL": json_bytes(label_fields(13, png)),
        "string-method.IL": json_bytes(label_fields("CCoeffNormed", png)),
        "fraction-roi.IL": json_bytes(label_fields(5, png, RangeX=0.5)),
        "negative-roi.IL": json_bytes(label_fields(5, png, TargetX=-1)),
        "overflow-roi.IL": json_bytes(label_fields(5, png, RangeWidth=4294967296)),
        "invalid-utf8.IL": invalid_utf8,
        "duplicate-key.IL": duplicate_key,
        "bad-base64.IL": json_bytes(label_fields(5, "!!!!")),
        "missing-padding.IL": json_bytes(label_fields(5, "YWI")),
        "decoded-limit.IL": json_bytes(valid_png),
        "dimension-mismatch.IL": json_bytes(
            label_fields(5, png, TargetWidth=3)
        ),
        "target-outside-range.IL": json_bytes(
            label_fields(5, png, TargetX=3)
        ),
        "frame-outside.IL": json_bytes(
            label_fields(3, png, RangeWidth=8, RangeHeight=8)
        ),
        "rejected.ILX": json_bytes(valid_png),
        "duplicates/a/shared.IL": json_bytes(valid_ocr),
        "duplicates/b/shared.IL": json_bytes(valid_ocr),
        "fuzz/truncated-json.seed": b'{"searchMethod":',
        "fuzz/deep-unknown.seed": (
            b'{"FutureField":' + (b"[" * 140) + b"0" + (b"]" * 140) + b"}"
        ),
        "fuzz/random-bytes.seed": bytes([0, 0x7B, 0xFF, 0x7D, 0x0A]),
    }
    expected = {
        "valid-bmp.IL": "ok",
        "valid-png.IL": "ok",
        "valid-ocr.IL": "ok",
        "default-method.IL": "ok-default-5",
        "unknown-field.IL": "warning-unknown-field",
        "unknown-method.IL": "unsupported-method",
        "string-method.IL": "invalid-type",
        "fraction-roi.IL": "invalid-number",
        "negative-roi.IL": "invalid-number",
        "overflow-roi.IL": "invalid-number",
        "invalid-utf8.IL": "invalid-utf8",
        "duplicate-key.IL": "duplicate-key",
        "bad-base64.IL": "invalid-base64",
        "missing-padding.IL": "invalid-base64",
        "decoded-limit.IL": "target-limit-with-small-limits",
        "dimension-mismatch.IL": "target-dimensions",
        "target-outside-range.IL": "target-outside-range",
        "frame-outside.IL": "evaluate-frame-bounds",
        "rejected.ILX": "unsupported-extension",
        "duplicates/a/shared.IL": "duplicate-name-in-registry",
        "duplicates/b/shared.IL": "duplicate-name-in-registry",
        "fuzz/truncated-json.seed": "fuzz-seed",
        "fuzz/deep-unknown.seed": "fuzz-seed",
        "fuzz/random-bytes.seed": "fuzz-seed",
    }
    files = dict(payloads)
    manifest = {
        "version": 1,
        "license": "GPL-3.0-only",
        "generator": "tools/generate_vision_label_fixtures.py",
        "provenance": (
            "Synthetic legacy .IL JSON authored for EasyCon SDK Phase 3. Image bytes "
            "reuse the repository-owned deterministic codec fixtures; no EasyCon file, "
            "font, traineddata, or external label is copied."
        ),
        "legacy_source_facts": {
            "name": "file stem",
            "default_searchMethod": 5,
            "enabled_searchMethod": [1, 3, 5, 11, 12, 107],
            "score_range": [0.0, 1.0],
            "future_ecs_integer_range": [0, 100],
            "excluded_extension": ".ILX",
        },
        "entries": [
            {
                "path": name,
                "bytes": len(data),
                "sha256": sha256(data),
                "expected": expected[name],
            }
            for name, data in sorted(payloads.items())
        ],
    }
    files["manifest.json"] = (
        json.dumps(manifest, indent=2, ensure_ascii=True) + "\n"
    ).encode("utf-8")
    return files


def relative_files():
    if not FIXTURE_DIR.exists():
        return set()
    return {
        path.relative_to(FIXTURE_DIR).as_posix()
        for path in FIXTURE_DIR.rglob("*")
        if path.is_file()
    }


def check(files):
    expected_names = set(files)
    actual_names = relative_files()
    failures = []
    if actual_names != expected_names:
        failures.append(
            "fixture file set differs: missing={}, extra={}".format(
                sorted(expected_names - actual_names),
                sorted(actual_names - expected_names),
            )
        )
    for name, expected in files.items():
        path = FIXTURE_DIR / name
        try:
            actual = path.read_bytes()
        except OSError as error:
            failures.append("{}: {}".format(path, error))
            continue
        if actual != expected:
            failures.append("{} is not reproducible from the tracked generator".format(path))
    if failures:
        for failure in failures:
            print(failure, file=sys.stderr)
        return 1
    print("validated 24 generated legacy .IL corpus entries and their manifest")
    return 0


def write(files):
    for name, data in files.items():
        path = FIXTURE_DIR / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    print("generated 24 legacy .IL corpus entries and their manifest")
    return 0


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--check",
        action="store_true",
        help="verify tracked fixture bytes instead of replacing them",
    )
    arguments = parser.parse_args()
    files = generated_files()
    return check(files) if arguments.check else write(files)


if __name__ == "__main__":
    sys.exit(main())
