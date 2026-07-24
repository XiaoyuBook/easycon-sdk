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
    "crates/easycon-ecs",
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
    "crates/easycon-sdk/",
)
FORBIDDEN_RUNTIME_PATTERNS = {
    "gRPC": re.compile(r"\bgrpc\b", re.IGNORECASE),
    "named pipe": re.compile(r"named[_ -]?pipe", re.IGNORECASE),
    "network socket": re.compile(r"\b(?:std|tokio)::net\b"),
    "service process": re.compile(r"service[_ -]?process", re.IGNORECASE),
    "WebSocket": re.compile(r"\bwebsocket\b", re.IGNORECASE),
}
NATIVE_COMMON_SOURCES = {
    "native/bridge/src/common/bridge.cpp",
    "native/bridge/src/common/bridge_internal.hpp",
    "native/bridge/src/common/capture_common.cpp",
    "native/bridge/src/common/capture_platform.hpp",
    "native/bridge/src/common/image_codec.cpp",
    "native/bridge/src/common/ocr.cpp",
    "native/bridge/src/common/vision_ops.cpp",
}
NATIVE_PLATFORM_SOURCES = {
    "native/bridge/src/platform/windows/capture.cpp",
    "native/bridge/src/platform/linux/capture.cpp",
    "native/bridge/src/platform/macos/capture_unavailable.cpp",
}
PLATFORM_NATIVE_PATTERNS = (
    re.compile(
        r"(?:<Windows\.h>|<dshow\.h>|<mfapi\.h>|<mfidl\.h>|"
        r"<linux/videodev2\.h>|AVFoundation|Objective-C)",
        re.IGNORECASE,
    ),
    re.compile(r"\b(?:HRESULT|HANDLE)\b"),
)
CONFORMANCE_MARKER = re.compile(
    r"^[ \t]*// conformance: [a-z0-9][a-z0-9.-]*[ \t]*$", re.MULTILINE
)
WORKFLOW_JOB = re.compile(
    r"^  (?P<name>[a-z0-9-]+):\r?\n(?P<body>.*?)(?=^  [a-z0-9-]+:\r?\n|\Z)",
    re.MULTILINE | re.DOTALL,
)


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


def required_ci_failures(workflow):
    failures = []
    jobs = {
        match.group("name"): match.group("body")
        for match in WORKFLOW_JOB.finditer(workflow)
    }
    policy = jobs.get("policy", "")
    windows_workspace = jobs.get("windows-workspace", "")
    if "name: Required / Policy" not in policy:
        failures.append("Required / Policy check name changed or its job is missing")
    if "name: Required / Windows Workspace" not in windows_workspace:
        failures.append("Required / Windows Workspace check name changed or its job is missing")

    runner = re.search(r"(?m)^    runs-on: ([^\r\n]+)", policy)
    runner = runner.group(1).strip() if runner else ""
    explicit_target = re.search(
        r"(?m)^      CARGO_BUILD_TARGET: ([^\r\n]+)", policy
    )
    default_target = re.search(
        r'(?m)^target = "([^"\r\n]+)"',
        (ROOT / ".cargo/config.toml").read_text(encoding="utf-8"),
    )
    target = (
        explicit_target.group(1).strip()
        if explicit_target
        else default_target.group(1) if default_target else ""
    )
    windows_marker_count = sum(
        len(CONFORMANCE_MARKER.findall(path.read_text(encoding="utf-8")))
        for path in (ROOT / "crates/easycon-serial/src/windows").rglob("*.rs")
    )

    if runner.startswith("ubuntu") and target == "x86_64-pc-windows-msvc":
        failures.append(
            "Required / Policy pairs an Ubuntu runner with the MSVC Cargo target; "
            "exact Rust tests cannot link without link.exe"
        )
    if runner.startswith("ubuntu") and target == "x86_64-unknown-linux-gnu" and windows_marker_count:
        failures.append(
            "Required / Policy selects a Linux target that excludes {} Windows-only exact "
            "conformance tests".format(windows_marker_count)
        )
    if runner != "windows-2022":
        failures.append("Required / Policy must run on windows-2022")
    if target != "x86_64-pc-windows-msvc":
        failures.append("Required / Policy must execute the x86_64-pc-windows-msvc target")

    required_policy_fragments = [
        "Import-Module $devShell",
        "Enter-VsDevShell -VsInstallPath $installationPath -SkipAutomaticLocation "
        '-DevCmdArguments "-arch=x64 -host_arch=x64"',
        "Add-Content -LiteralPath $env:GITHUB_ENV",
        "Add-Content -LiteralPath $env:GITHUB_PATH",
        "Get-Command link.exe",
        "BASE_SHA: ${{ github.event.pull_request.base.sha || "
        "github.event.merge_group.base_sha || github.event.before }}",
        "git diff --check",
        "git status --porcelain=v1 --untracked-files=all",
    ]
    python = "python" if runner == "windows-2022" else "python3"
    required_policy_fragments.extend(
        "{} tools/{}.py".format(python, tool)
        for tool in (
            "validate_specs",
            "check_markdown_links",
            "check_repository_guards",
        )
    )
    for fragment in required_policy_fragments:
        if fragment not in policy:
            failures.append("Required / Policy is missing {!r}".format(fragment))
    return failures


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
        failures.append("workspace members differ from frozen workspace packages")
    for package in metadata["packages"]:
        if package["id"] in metadata["workspace_members"] and package["license"] != "GPL-3.0-only":
            failures.append("{} is not GPL-3.0-only".format(package["name"]))

    expected_dependencies = {
        "crates/easycon-model/Cargo.toml": set(),
        "crates/easycon-runtime/Cargo.toml": {"easycon-model"},
        "crates/easycon-controller/Cargo.toml": {"easycon-model", "easycon-runtime"},
        "crates/easycon-ecs/Cargo.toml": set(),
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

    if not NATIVE_COMMON_SOURCES.issubset(tracked_set):
        failures.append("native common source layout is incomplete")
    if not NATIVE_PLATFORM_SOURCES.issubset(tracked_set):
        failures.append("native platform source layout is incomplete")
    for relative in sorted(NATIVE_COMMON_SOURCES & tracked_set):
        text = (ROOT / relative).read_text(encoding="utf-8")
        if any(pattern.search(text) for pattern in PLATFORM_NATIVE_PATTERNS):
            failures.append("platform type or header leaked into native common source: {}".format(relative))

    vision_capture = (ROOT / "crates/easycon-vision/src/capture.rs").read_text(encoding="utf-8")
    for platform_backend in ("DirectShow", "MediaFoundation", "V4l2", "AvFoundation"):
        if platform_backend in vision_capture:
            failures.append(
                "platform backend leaked into public Vision capture module: {}".format(
                    platform_backend
                )
            )

    cmake_paths = ["CMakeLists.txt", "native/bridge/CMakeLists.txt"]
    for relative in cmake_paths:
        text = (ROOT / relative).read_text(encoding="utf-8")
        if re.search(r"(^|\s)install\s*\(", text, re.IGNORECASE):
            failures.append("Phase 3 private native targets must not install files")

    vcpkg_configuration = (ROOT / "vcpkg-configuration.json").read_text(encoding="utf-8")
    if "cd61e1e26a038e82d6550a3ebbe0fbbfe7da78e3" not in vcpkg_configuration:
        failures.append("Phase 3 vcpkg registry baseline changed")

    required_ci = (ROOT / ".github/workflows/required-ci.yml").read_text(encoding="utf-8")
    failures.extend(required_ci_failures(required_ci))

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
        "repository guards passed: frozen workspace and dependency direction, GPL license, "
        "private native boundary, source boundary, Drop finalizer ban, legacy process scan, and "
        "Required CI execution ownership"
    )
    return 0


if __name__ == "__main__":
    if sys.argv[1:] == ["--required-ci-stdin"]:
        workflow_failures = required_ci_failures(sys.stdin.read())
        if workflow_failures:
            print("Required CI guard check failed:", file=sys.stderr)
            for failure in sorted(set(workflow_failures)):
                print("  " + failure, file=sys.stderr)
            sys.exit(1)
        print("Required CI guard passed")
        sys.exit(0)
    if sys.argv[1:]:
        print("usage: check_repository_guards.py [--required-ci-stdin]", file=sys.stderr)
        sys.exit(2)
    sys.exit(main())
