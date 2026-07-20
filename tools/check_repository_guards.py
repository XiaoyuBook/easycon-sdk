#!/usr/bin/env python3
"""Enforce Phase 2A ownership, dependency, license, and architecture guards."""

import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
EXPECTED_MEMBERS = {
    "crates/easycon-model",
    "crates/easycon-runtime",
    "crates/easycon-controller",
    "crates/easycon-serial",
    "tests/support",
}
FORBIDDEN_PREFIXES = (
    "bindings/",
    "ci/",
    "firmware/",
    "native/",
    "services/",
    "ui/",
    "crates/easycon-capi/",
    "crates/easycon-ecs/",
    "crates/easycon-native-sys/",
    "crates/easycon-sdk/",
    "crates/easycon-vision/",
)
FORBIDDEN_RUNTIME_PATTERNS = {
    "gRPC": re.compile(r"\bgrpc\b", re.IGNORECASE),
    "named pipe": re.compile(r"named[_ -]?pipe", re.IGNORECASE),
    "network socket": re.compile(r"\b(?:std|tokio)::net\b"),
    "service process": re.compile(r"service[_ -]?process", re.IGNORECASE),
    "WebSocket": re.compile(r"\bwebsocket\b", re.IGNORECASE),
}


def git_files():
    output = subprocess.check_output(
        ["git", "ls-files", "-z"], cwd=str(ROOT), stderr=subprocess.STDOUT
    )
    return [item.decode("utf-8") for item in output.split(b"\0") if item]


def workspace_members(cargo_text):
    match = re.search(r"members\s*=\s*\[(.*?)\]", cargo_text, re.DOTALL)
    if not match:
        return set()
    return set(re.findall(r'"([^"]+)"', match.group(1)))


def workspace_dependencies(manifest):
    text = (ROOT / manifest).read_text(encoding="utf-8")
    return set(re.findall(r"\b(easycon-[a-z-]+)\s*=", text))


def main():
    failures = []
    tracked = git_files()
    tracked_set = set(tracked)
    if any(path == "EasyCon" or path.startswith("EasyCon/") for path in tracked):
        failures.append("outer repository tracks EasyCon content")
    if ".gitmodules" in tracked_set:
        failures.append("submodules are forbidden in this milestone")
    for prefix in FORBIDDEN_PREFIXES:
        if any(path.startswith(prefix) for path in tracked):
            failures.append("out-of-scope tracked path: {}".format(prefix))

    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    if workspace_members(cargo) != EXPECTED_MEMBERS:
        failures.append("workspace members differ from the four milestone packages")
    if 'license = "GPL-3.0-only"' not in cargo:
        failures.append("workspace license is not GPL-3.0-only")

    expected_dependencies = {
        "crates/easycon-model/Cargo.toml": set(),
        "crates/easycon-runtime/Cargo.toml": {"easycon-model"},
        "crates/easycon-controller/Cargo.toml": {"easycon-model", "easycon-runtime"},
        "crates/easycon-serial/Cargo.toml": {
            "easycon-controller",
            "easycon-model",
            "easycon-runtime",
        },
        "tests/support/Cargo.toml": {
            "easycon-controller",
            "easycon-model",
            "easycon-runtime",
        },
    }
    for manifest, expected in expected_dependencies.items():
        if workspace_dependencies(manifest) != expected:
            failures.append("workspace dependency direction changed in {}".format(manifest))
        text = (ROOT / manifest).read_text(encoding="utf-8")
        if "license.workspace = true" not in text:
            failures.append("{} does not inherit GPL workspace license".format(manifest))
        if "EasyCon/" in text or "EasyCon\\" in text:
            failures.append("{} references the ignored source snapshot".format(manifest))

    source_paths = [
        path
        for path in tracked
        if (path.startswith("crates/") or path.startswith("tests/"))
        and Path(path).suffix in {".rs", ".toml"}
        and (ROOT / path).is_file()
    ]
    for relative in source_paths:
        text = (ROOT / relative).read_text(encoding="utf-8")
        if "EasyCon/" in text or "EasyCon\\" in text:
            failures.append("functional source references ignored EasyCon path: {}".format(relative))
        for label, pattern in FORBIDDEN_RUNTIME_PATTERNS.items():
            if pattern.search(text):
                failures.append("legacy {} architecture keyword in {}".format(label, relative))

    runtime_source = (ROOT / "crates/easycon-runtime/src/runtime.rs").read_text(
        encoding="utf-8"
    )
    for forbidden in ("spawn_finalizer", "easycon-finalizer", "close_after_final_handle_drop"):
        if forbidden in runtime_source:
            failures.append("Runtime Drop finalizer remains: {}".format(forbidden))

    if failures:
        print("repository guard check failed:", file=sys.stderr)
        for failure in sorted(set(failures)):
            print("  " + failure, file=sys.stderr)
        return 1
    print(
        "repository guards passed: Phase 2A workspace, dependency direction, GPL license, "
        "source boundary, Drop finalizer ban, and legacy service-process scan"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
