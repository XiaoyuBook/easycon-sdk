#!/usr/bin/env python3
"""Provision the frozen, test-only English Tesseract model into ignored storage."""

import argparse
import hashlib
import json
import os
import sys
import tempfile
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EXPECTED_MANIFEST = ROOT / "spec" / "fixtures" / "vision" / "ocr-model.json"
ALLOWED_HOST = "raw.githubusercontent.com"
CACHE_ROOT = ROOT / ".tools" / "vision-models"
FROZEN_MODEL = {
    "language": "eng",
    "path": "eng.traineddata",
    "url": (
        "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/"
        "refs/tags/4.1.0/eng.traineddata"
    ),
    "bytes": 4113088,
    "sha256": "7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2",
}
FROZEN_LICENSE = {
    "spdx": "Apache-2.0",
    "path": "LICENSE",
    "url": (
        "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/"
        "refs/tags/4.1.0/LICENSE"
    ),
    "bytes": 11358,
    "sha256": "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30",
}


class ProvisionError(Exception):
    """Raised when the frozen test model cannot be proven."""


def require_allowed_url(url):
    parsed = urllib.parse.urlparse(url)
    if parsed.scheme != "https" or parsed.hostname != ALLOWED_HOST:
        raise ProvisionError("download URL is outside the frozen HTTPS host")


class FrozenRedirectHandler(urllib.request.HTTPRedirectHandler):
    """Reject a redirect before urllib follows an off-host hop."""

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        require_allowed_url(newurl)
        return super().redirect_request(req, fp, code, msg, headers, newurl)


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_file(path, entry):
    try:
        actual_size = path.stat().st_size
    except OSError as error:
        raise ProvisionError("{}: {}".format(path, error)) from error
    if actual_size != entry["bytes"]:
        raise ProvisionError(
            "{} has {} bytes; expected {}".format(path, actual_size, entry["bytes"])
        )
    actual_hash = sha256(path)
    if actual_hash != entry["sha256"]:
        raise ProvisionError(
            "{} has SHA-256 {}; expected {}".format(
                path, actual_hash, entry["sha256"]
            )
        )


def download(entry, destination):
    require_allowed_url(entry["url"])

    request = urllib.request.Request(
        entry["url"], headers={"User-Agent": "EasyCon-SDK-vision-model-provisioner/1"}
    )
    temporary_path = None
    try:
        try:
            opener = urllib.request.build_opener(FrozenRedirectHandler())
            with opener.open(request, timeout=60) as response:
                require_allowed_url(response.geturl())
                with tempfile.NamedTemporaryFile(
                    mode="wb", dir=destination.parent, delete=False
                ) as temporary:
                    temporary_path = Path(temporary.name)
                    while True:
                        chunk = response.read(1024 * 1024)
                        if not chunk:
                            break
                        temporary.write(chunk)
        except (OSError, urllib.error.URLError) as error:
            raise ProvisionError(
                "download failed for {}: {}".format(entry["url"], error)
            ) from error
        if temporary_path is None:
            raise ProvisionError("download produced no temporary file")
        verify_file(temporary_path, entry)
        os.replace(temporary_path, destination)
    finally:
        if temporary_path is not None:
            try:
                temporary_path.unlink(missing_ok=True)
            except OSError:
                pass


def load_manifest(path):
    try:
        resolved = path.resolve(strict=True)
        expected = EXPECTED_MANIFEST.resolve(strict=True)
        if resolved != expected:
            raise ProvisionError("only the tracked frozen OCR manifest is accepted")
        with resolved.open("r", encoding="utf-8") as stream:
            manifest = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        raise ProvisionError("{}: {}".format(path, error)) from error

    if manifest.get("version") != 1:
        raise ProvisionError("OCR model manifest version must be 1")
    if manifest.get("model") != FROZEN_MODEL or manifest.get("license") != FROZEN_LICENSE:
        raise ProvisionError("OCR model source, license, size, or hash differs from the freeze")
    for name in ("model", "license"):
        entry = manifest.get(name)
        if not isinstance(entry, dict):
            raise ProvisionError("OCR model manifest is missing {}".format(name))
        required = {"path", "url", "bytes", "sha256"}
        if not required.issubset(entry):
            raise ProvisionError("OCR model {} entry is incomplete".format(name))
        if Path(entry["path"]).name != entry["path"]:
            raise ProvisionError("OCR model output names must be single path components")
        if not isinstance(entry["bytes"], int) or entry["bytes"] <= 0:
            raise ProvisionError("OCR model byte count must be positive")
        if not isinstance(entry["sha256"], str) or len(entry["sha256"]) != 64:
            raise ProvisionError("OCR model SHA-256 must contain 64 hexadecimal characters")
    return manifest


def provision(manifest, output):
    cache_root = CACHE_ROOT.resolve()
    if not output.is_absolute():
        output = ROOT / output
    resolved_output = output.resolve()
    try:
        resolved_output.relative_to(cache_root)
    except ValueError as error:
        raise ProvisionError(
            "OCR model output must remain inside ignored .tools/vision-models"
        ) from error
    output = resolved_output
    output.mkdir(parents=True, exist_ok=True)
    for name in ("model", "license"):
        entry = manifest[name]
        destination = output / entry["path"]
        if destination.exists():
            verify_file(destination, entry)
        else:
            download(entry, destination)
            verify_file(destination, entry)
    print("provisioned and verified test-only OCR model at {}".format(output.resolve()))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    manifest = load_manifest(arguments.manifest)
    provision(manifest, arguments.output)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ProvisionError as error:
        print("OCR model provisioning failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
