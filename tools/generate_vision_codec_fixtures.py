#!/usr/bin/env python3
"""Generate or verify the synthetic Phase 3 codec fixtures using only stdlib."""

import argparse
import binascii
import hashlib
import json
import struct
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
FIXTURE_DIR = ROOT / "spec" / "fixtures" / "vision" / "codec"
EXPECTED_BGR = bytes.fromhex("0000ff00ff00ff0000ffffff")


def png_chunk(chunk_type, payload):
    body = chunk_type + payload
    return struct.pack(">I", len(payload)) + body + struct.pack(">I", binascii.crc32(body))


def adler32(data):
    first = 1
    second = 0
    for value in data:
        first = (first + value) % 65521
        second = (second + first) % 65521
    return (second << 16) | first


def stored_zlib(data):
    if len(data) > 65535:
        raise ValueError("fixture scanlines exceed one stored DEFLATE block")
    length = len(data)
    return (
        b"\x78\x01"
        + b"\x01"
        + struct.pack("<HH", length, length ^ 0xFFFF)
        + data
        + struct.pack(">I", adler32(data))
    )


def make_png(width, height, color_type, rows, chunks_before_idat=()):
    scanlines = b"".join(b"\x00" + row for row in rows)
    header = struct.pack(">IIBBBBB", width, height, 8, color_type, 0, 0, 0)
    return (
        b"\x89PNG\r\n\x1a\n"
        + png_chunk(b"IHDR", header)
        + b"".join(png_chunk(chunk_type, payload) for chunk_type, payload in chunks_before_idat)
        + png_chunk(b"IDAT", stored_zlib(scanlines))
        + png_chunk(b"IEND", b"")
    )


def make_bgr_png():
    return make_png(
        2,
        2,
        2,
        (
            bytes.fromhex("ff000000ff00"),
            bytes.fromhex("0000ffffffff"),
        ),
    )


def make_gray_alpha_png():
    return make_png(1, 1, 4, (bytes.fromhex("7fff"),))


def make_palette_png():
    return make_png(
        1,
        1,
        3,
        (b"\x00",),
        chunks_before_idat=((b"PLTE", bytes.fromhex("ff0000")),),
    )


def make_bmp():
    width = 2
    height = 2
    row_stride = 8
    pixel_bytes = row_stride * height
    file_size = 14 + 40 + pixel_bytes
    file_header = b"BM" + struct.pack("<IHHI", file_size, 0, 0, 54)
    dib_header = struct.pack(
        "<IiiHHIIiiII",
        40,
        width,
        height,
        1,
        24,
        0,
        pixel_bytes,
        2835,
        2835,
        0,
        0,
    )
    bottom_up_rows = bytes.fromhex("ff0000ffffff0000") + bytes.fromhex(
        "0000ff00ff000000"
    )
    return file_header + dib_header + bottom_up_rows


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def generated_files():
    payloads = {
        "bgr-2x2.bmp.hex": make_bmp(),
        "bgr-2x2.png.hex": make_bgr_png(),
        "expected-bgr.hex": EXPECTED_BGR,
        "gray-alpha-1x1.png.hex": make_gray_alpha_png(),
        "palette-1x1.png.hex": make_palette_png(),
    }
    files = {
        name: data.hex().encode("ascii") + b"\n" for name, data in payloads.items()
    }
    manifest = {
        "version": 1,
        "license": "GPL-3.0-only",
        "generator": "tools/generate_vision_codec_fixtures.py",
        "provenance": (
            "Synthetic pixels authored for EasyCon SDK Phase 3; PNG uses a "
            "deterministic stored-DEFLATE zlib stream and BMP uses uncompressed "
            "bottom-up 24-bit rows."
        ),
        "pixels": {
            "order": "top-to-bottom BGR8",
            "description": "red, green, blue, white",
            "sha256": sha256(EXPECTED_BGR),
        },
        "files": [
            {
                "path": name,
                "decoded_bytes": len(payloads[name]),
                "decoded_sha256": sha256(payloads[name]),
            }
            for name in (
                "bgr-2x2.bmp.hex",
                "bgr-2x2.png.hex",
                "expected-bgr.hex",
                "gray-alpha-1x1.png.hex",
                "palette-1x1.png.hex",
            )
        ],
    }
    files["manifest.json"] = (
        json.dumps(manifest, indent=2, ensure_ascii=True) + "\n"
    ).encode("utf-8")
    return files


def check(files):
    expected_names = set(files)
    actual_names = {
        path.name for path in FIXTURE_DIR.iterdir() if path.is_file()
    }
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
    print("validated 5 generated vision codec fixtures and their manifest")
    return 0


def write(files):
    FIXTURE_DIR.mkdir(parents=True, exist_ok=True)
    for name, data in files.items():
        (FIXTURE_DIR / name).write_bytes(data)
    print("generated 5 vision codec fixtures and their manifest")
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
