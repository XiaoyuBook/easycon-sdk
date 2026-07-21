#!/usr/bin/env python3
"""Enforce frozen workspace, dependency, license, and architecture guards."""

import json
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
    "crates/easycon-native-sys",
    "crates/easycon-vision",
    "tests/support",
}
FORBIDDEN_PREFIXES = (
    "bindings/",
    "ci/",
    "firmware/",
    "services/",
    "ui/",
    "crates/easycon-capi/",
    "crates/easycon-ecs/",
    "crates/easycon-sdk/",
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
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=str(ROOT),
        stderr=subprocess.STDOUT,
    )
    return [item.decode("utf-8") for item in output.split(b"\0") if item]


def cargo_metadata():
    output = subprocess.check_output(
        ["cargo", "metadata", "--format-version", "1", "--no-deps"],
        cwd=str(ROOT),
        stderr=subprocess.STDOUT,
    )
    return json.loads(output.decode("utf-8"))


def relative_manifest(package):
    return Path(package["manifest_path"]).resolve().relative_to(ROOT).as_posix()


def workspace_members(metadata):
    member_ids = set(metadata["workspace_members"])
    return {
        str(Path(relative_manifest(package)).parent).replace("\\", "/")
        for package in metadata["packages"]
        if package["id"] in member_ids
    }


def workspace_dependencies(metadata, path):
    package = next(
        package for package in metadata["packages"] if relative_manifest(package) == path
    )
    return {
        dependency["name"]
        for dependency in package["dependencies"]
        if dependency["kind"] != "build" and dependency["name"].startswith("easycon-")
    }


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
    if any(path.startswith("native/") and not path.startswith("native/bridge/") for path in tracked):
        failures.append("native content exists outside native/bridge")
    if any(path.lower().endswith(".traineddata") for path in tracked):
        failures.append("traineddata must not be tracked")

    metadata = cargo_metadata()
    if workspace_members(metadata) != EXPECTED_MEMBERS:
        failures.append("workspace members differ from frozen Phase 2A plus Phase 3 packages")
    for package in metadata["packages"]:
        if package["id"] in metadata["workspace_members"] and package["license"] != "GPL-3.0-only":
            failures.append("{} is not GPL-3.0-only".format(package["name"]))

    expected_dependencies = {
        "crates/easycon-model/Cargo.toml": set(),
        "crates/easycon-runtime/Cargo.toml": {"easycon-model"},
        "crates/easycon-controller/Cargo.toml": {"easycon-model", "easycon-runtime"},
        "crates/easycon-serial/Cargo.toml": {
            "easycon-controller",
            "easycon-model",
            "easycon-runtime",
        },
        "crates/easycon-native-sys/Cargo.toml": set(),
        "crates/easycon-vision/Cargo.toml": {
            "easycon-model",
            "easycon-native-sys",
            "easycon-runtime",
        },
        "tests/support/Cargo.toml": {
            "easycon-controller",
            "easycon-model",
            "easycon-runtime",
            "easycon-serial",
        },
    }
    for manifest_path, expected in expected_dependencies.items():
        if workspace_dependencies(metadata, manifest_path) != expected:
            failures.append("workspace dependency direction changed in {}".format(manifest_path))
        text = (ROOT / manifest_path).read_text(encoding="utf-8")
        if "license.workspace = true" not in text:
            failures.append("{} does not inherit GPL workspace license".format(manifest_path))
        if "EasyCon/" in text or "EasyCon\\" in text:
            failures.append("{} references the ignored source snapshot".format(manifest_path))

    source_paths = [
        path
        for path in tracked
        if (path.startswith("crates/") or path.startswith("tests/") or path.startswith("native/"))
        and Path(path).suffix in {".rs", ".toml", ".h", ".hpp", ".cpp"}
        and (ROOT / path).is_file()
    ]
    for relative in source_paths:
        text = (ROOT / relative).read_text(encoding="utf-8")
        if "EasyCon/" in text or "EasyCon\\" in text:
            failures.append("functional source references ignored EasyCon path: {}".format(relative))
        for label, pattern in FORBIDDEN_RUNTIME_PATTERNS.items():
            if pattern.search(text):
                failures.append("legacy {} architecture keyword in {}".format(label, relative))
        if "easycon_v1_" in text:
            failures.append("public C ABI symbol leaked into Phase 3 source: {}".format(relative))

    cmake_paths = ["CMakeLists.txt", "native/bridge/CMakeLists.txt"]
    for relative in cmake_paths:
        text = (ROOT / relative).read_text(encoding="utf-8")
        if re.search(r"(^|\s)install\s*\(", text, re.IGNORECASE):
            failures.append("Phase 3 private native targets must not install files")

    vcpkg_configuration = (ROOT / "vcpkg-configuration.json").read_text(encoding="utf-8")
    if "cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3" not in vcpkg_configuration:
        failures.append("Phase 3 vcpkg registry baseline changed")

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
        "repository guards passed: frozen workspace, Phase 3 dependency direction, GPL license, "
        "private native boundary, source boundary, Drop finalizer ban, and legacy process scan"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
