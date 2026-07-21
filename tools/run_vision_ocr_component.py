#!/usr/bin/env python3
"""Verify the frozen local OCR model, then run one native component executable."""

import argparse
import os
import subprocess
import sys
from pathlib import Path

import provision_vision_test_model as model


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--executable", type=Path, required=True)
    arguments = parser.parse_args()

    value = os.environ.get("EASYCON_VISION_TEST_TESSDATA")
    if not value:
        raise model.ProvisionError(
            "EASYCON_VISION_TEST_TESSDATA must name the provisioned model directory"
        )
    model_root = Path(value)
    if not model_root.is_absolute() or not model_root.is_dir():
        raise model.ProvisionError("OCR component model root must be an absolute directory")

    manifest = model.load_manifest(arguments.manifest)
    for name in ("model", "license"):
        entry = manifest[name]
        model.verify_file(model_root / entry["path"], entry)

    executable = arguments.executable.resolve(strict=True)
    completed = subprocess.run([str(executable)], check=False)
    return completed.returncode


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, model.ProvisionError) as error:
        print("OCR component setup failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
