#!/usr/bin/env python3
"""Run the bounded Loom models required by the Runtime stabilization gate."""

import os
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def cargo_executable():
    cargo = shutil.which("cargo")
    if cargo:
        return cargo
    fallback = Path.home() / ".cargo" / ("cargo.exe" if os.name == "nt" else "cargo")
    if fallback.is_file():
        return str(fallback)
    raise RuntimeError("cargo was not found in PATH or $HOME/.cargo/bin")


def main():
    command = [
        cargo_executable(),
        "test",
        "-p",
        "easycon-runtime",
        "--test",
        "loom_runtime",
        "--",
        "--test-threads=1",
    ]
    print("running Runtime Loom models: {}".format(" ".join(command)), flush=True)
    return subprocess.call(command, cwd=str(ROOT))


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError) as error:
        print("Runtime model validation failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
