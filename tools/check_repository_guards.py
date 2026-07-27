#!/usr/bin/env python3
"""Enforce frozen workspace, dependency, license, and architecture guards."""

import datetime
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path, PurePosixPath
from urllib.parse import urlsplit


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
W0_ZERO_DEPENDENCY_MANIFEST = "crates/easycon-ecs/Cargo.toml"
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
WINDOWS_BUILD_FILES = {
    "tools/run_windows_workspace.ps1",
    "tools/test_windows_environment_lifecycle.ps1",
    "tools/test_windows_workspace.ps1",
    "tools/test_windows_bootstrap_contracts.py",
    "tools/windows_build_environment.json",
    "tools/windows_workspace.psm1",
}

CHECKOUT_ACTION = "actions/checkout@34e114876b0b11c390a56381ad16ebd13914f8d5"
CACHE_RESTORE_ACTION = (
    "actions/cache/restore@0057852bfaa89a56745cba8c7296529d2fc39830"
)
CACHE_SAVE_ACTION = "actions/cache/save@0057852bfaa89a56745cba8c7296529d2fc39830"
WORKSPACE_INVOCATION = (
    "./tools/run_windows_workspace.ps1 -Mode Workspace -RequireCleanTree"
)
SETUP_INVOCATION = "./tools/run_windows_workspace.ps1 -Mode Setup"
WINDOWS_CACHE_ROOT_INITIALIZATION = """$ErrorActionPreference = "Stop"
if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) {
  throw "RUNNER_TEMP is not available"
}
$cacheRoot = Join-Path $env:RUNNER_TEMP "easycon-windows-workspace"
New-Item -ItemType Directory -Force -Path (Join-Path $cacheRoot "caches") | Out-Null
Add-Content -LiteralPath $env:GITHUB_ENV -Value "EASYCON_BUILD_CACHE_ROOT=$cacheRoot" -Encoding utf8"""
SETUP_ASSET_CACHE_PATHS = """${{ runner.temp }}/easycon-windows-workspace/caches/assets-v1
${{ runner.temp }}/easycon-windows-workspace/caches/vcpkg-scripts
${{ runner.temp }}/easycon-windows-workspace/caches/vcpkg-downloads
${{ runner.temp }}/easycon-windows-workspace/caches/rustup-home
"""
SETUP_ASSET_CACHE_KEY = (
    "windows-workspace-setup-assets-v1-${{ github.event_name == 'pull_request' && "
    "format('pr-{0}', github.event.pull_request.number) || 'trusted-main' }}-"
    "${{ github.run_id }}-${{ github.run_attempt }}"
)
SETUP_ASSET_RESTORE_KEYS = (
    "windows-workspace-setup-assets-v1-${{ github.event_name == 'pull_request' && "
    "format('pr-{0}-', github.event.pull_request.number) || 'trusted-main-' }}\n"
    "windows-workspace-setup-assets-v1-trusted-main-\n"
)
JOB_LEVEL_RUNNER_CONTEXT = re.compile(
    r"\$\{\{(?:(?!\}\}).)*\brunner\s*\.", re.IGNORECASE
)
POLICY_RUN_SHA256 = {
    "Isolate generated outputs": "774f504e5fdcdfb96cfb68e74e803d2e228f4df65a7846b305fbb76359bb3292",
    "Load the MSVC x64 developer environment": "738056873e87faf3bc1ee900547897413b0e462c67fa48b2b4b2dc14cda414fc",
    "Install and verify the frozen Rust toolchain": "dddf2ec394db5fdd537cda92ecd397fe32c2c029e4a74b760b5c6f4900ede4e1",
    "Run repository policy gates": "2103a97e81bdc4366d3b4d1f89cd9391b24d4ebe614938deafe42b286d6abe4b",
    "Check changed lines and repository cleanliness": "cf7ca03b111e12bb1e3e951bb3957e487b87249609a73f1f6b34aac24bc95286",
}
WINDOWS_CONFIGURATION_KEYS = {
    "version",
    "target",
    "fingerprintInputs",
    "visionModelDirectory",
    "hostTools",
    "vcpkg",
}
FINGERPRINT_INPUT_KEYS = {"path", "kind"}
VCPKG_CONFIGURATION_KEYS = {
    "scriptsRepository",
    "scriptsCommit",
    "registryBaseline",
    "toolRelease",
    "toolCommit",
    "windowsAsset",
    "toolsManifest",
    "internalTools",
    "nativeDependencies",
}
WINDOWS_ASSET_KEYS = {"name", "url", "bytes", "sha256"}
HOST_TOOL_KEYS = {
    "pythonMinimumVersion",
    "visualStudioMajorVersion",
    "msvcToolsVersion",
    "windowsSdkVersion",
}
INTERNAL_TOOL_KEYS = {"name", "version", "url", "archive", "executable", "sha512"}
NATIVE_DEPENDENCY_KEYS = {"name", "version", "portVersion", "gitTree"}


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


def package_dependency_entries(metadata, path):
    package = next(
        package for package in metadata["packages"] if relative_manifest(package) == path
    )
    return package["dependencies"]


def managed_crate_roots(tracked):
    roots = set()
    for path in tracked:
        parts = PurePosixPath(path).parts
        is_workspace_crate = (
            len(parts) >= 2 and parts[0] == "crates" and parts[-1] == "Cargo.toml"
        )
        is_test_support = parts == ("tests", "support", "Cargo.toml")
        if is_workspace_crate or is_test_support:
            roots.add(PurePosixPath(path).parent.as_posix())
    return roots


def w0_boundary_failures(tracked, metadata):
    failures = []
    roots = managed_crate_roots(tracked)
    missing = EXPECTED_MEMBERS - roots
    unexpected = roots - EXPECTED_MEMBERS
    if missing:
        failures.append(
            "missing managed crate root: {}".format(", ".join(sorted(missing)))
        )
    if unexpected:
        failures.append(
            "unexpected managed crate root: {}".format(", ".join(sorted(unexpected)))
        )
    if package_dependency_entries(metadata, W0_ZERO_DEPENDENCY_MANIFEST):
        failures.append("W0 easycon-ecs dependency list must remain empty")
    return failures


def repository_guard_regression_failures():
    failures = []
    valid_tracked = {
        "{}/Cargo.toml".format(root) for root in EXPECTED_MEMBERS
    } | {
        "Cargo.toml",
        "crates/README.md",
        "tests/hardware/Cargo.toml",
    }

    def metadata_with(dependencies):
        return {
            "packages": [
                {
                    "manifest_path": str(ROOT / W0_ZERO_DEPENDENCY_MANIFEST),
                    "dependencies": dependencies,
                }
            ]
        }

    def expect(label, tracked, dependencies, should_fail):
        case_failures = w0_boundary_failures(tracked, metadata_with(dependencies))
        if bool(case_failures) != should_fail:
            failures.append("repository guard regression failed: {}".format(label))

    expect("exact valid managed roots", valid_tracked, [], False)
    expect(
        "external normal dependency",
        valid_tracked,
        [{"name": "serde", "kind": None}],
        True,
    )
    expect(
        "external build dependency",
        valid_tracked,
        [{"name": "cc", "kind": "build"}],
        True,
    )
    expect(
        "external dev dependency",
        valid_tracked,
        [{"name": "serde", "kind": "dev"}],
        True,
    )
    expect(
        "internal build dependency",
        valid_tracked,
        [{"name": "easycon-controller", "kind": "build"}],
        True,
    )
    expect(
        "unknown tracked crate",
        valid_tracked
        | {
            "crates/unexpected/Cargo.toml",
            "crates/unexpected/src/lib.rs",
        },
        [],
        True,
    )
    expect(
        "nested crate under unknown root",
        valid_tracked
        | {
            "crates/unexpected/nested/Cargo.toml",
            "crates/unexpected/nested/src/lib.rs",
        },
        [],
        True,
    )
    expect(
        "nested crate under managed root",
        valid_tracked
        | {
            "crates/easycon-ecs/nested/Cargo.toml",
            "crates/easycon-ecs/nested/src/lib.rs",
        },
        [],
        True,
    )
    expect(
        "extra managed crate root",
        valid_tracked | {"crates/extra/Cargo.toml"},
        [],
        True,
    )
    expect(
        "missing expected crate root",
        valid_tracked - {"crates/easycon-ecs/Cargo.toml"},
        [],
        True,
    )
    expect(
        "non-crate tracked path",
        valid_tracked | {"crates/unexpected/src/lib.rs"},
        [],
        False,
    )
    return failures
class _StrictWorkflowParser:
    """Parse the small, intentionally frozen YAML subset used by required-ci.yml."""

    KEY = re.compile(r"^([A-Za-z0-9_-]+):(.*)$")
    INTEGER = re.compile(r"^(?:0|[1-9][0-9]*)$")

    def __init__(self, text):
        if text.startswith("\ufeff"):
            raise ValueError("UTF-8 BOM is not accepted")
        self.lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
        self.index = 0

    def parse(self):
        for number, line in enumerate(self.lines, 1):
            if "\t" in line:
                raise ValueError("tabs are not accepted at line {}".format(number))
            indent = len(line) - len(line.lstrip(" "))
            if indent % 2:
                raise ValueError("indentation must use two-space levels at line {}".format(number))
        self._skip_ignored()
        if self.index == len(self.lines):
            raise ValueError("workflow is empty")
        document = self._parse_block(0)
        self._skip_ignored()
        if self.index != len(self.lines):
            self._error("unexpected trailing content")
        return document

    def _error(self, message, index=None):
        if index is None:
            index = self.index
        raise ValueError("{} at line {}".format(message, index + 1))

    def _line(self, index=None):
        if index is None:
            index = self.index
        line = self.lines[index]
        return len(line) - len(line.lstrip(" ")), line.lstrip(" ")

    def _skip_ignored(self):
        while self.index < len(self.lines):
            _, content = self._line()
            if content and not content.startswith("#"):
                break
            self.index += 1

    def _next_content(self):
        index = self.index
        while index < len(self.lines):
            indent, content = self._line(index)
            if content and not content.startswith("#"):
                return index, indent, content
            index += 1
        return None, None, None

    @staticmethod
    def _strip_comment(value):
        quote = None
        escaped = False
        for index, character in enumerate(value):
            if quote == '"':
                if escaped:
                    escaped = False
                elif character == "\\":
                    escaped = True
                elif character == '"':
                    quote = None
            elif quote == "'":
                if character == "'":
                    if index + 1 < len(value) and value[index + 1] == "'":
                        continue
                    quote = None
            elif character in "'\"":
                quote = character
            elif character == "#" and (index == 0 or value[index - 1].isspace()):
                return value[:index].rstrip()
        if quote is not None:
            raise ValueError("unterminated quoted scalar")
        return value.rstrip()

    def _parse_scalar(self, raw):
        value = self._strip_comment(raw.strip())
        if not value:
            self._error("missing scalar")
        if value[0] in "&*!" or value.startswith(("[", "{")):
            self._error("anchors, aliases, tags, and flow collections are not accepted")
        if value.startswith('"'):
            try:
                parsed = json.loads(value)
            except json.JSONDecodeError as error:
                self._error("invalid double-quoted scalar: {}".format(error.msg))
            if not isinstance(parsed, str):
                self._error("double-quoted workflow scalars must be strings")
            return parsed
        if value.startswith("'"):
            if len(value) < 2 or not value.endswith("'"):
                self._error("invalid single-quoted scalar")
            return value[1:-1].replace("''", "'")
        if value == "true":
            return True
        if value == "false":
            return False
        if self.INTEGER.fullmatch(value):
            return int(value)
        return value

    def _parse_literal(self, parent_indent):
        start = self.index
        end = start
        content_indents = []
        while end < len(self.lines):
            indent, content = self._line(end)
            if content and indent <= parent_indent:
                break
            if content:
                content_indents.append(indent)
            end += 1
        if not content_indents:
            self._error("literal block must not be empty", start - 1)
        block_indent = min(content_indents)
        if block_indent != parent_indent + 2:
            self._error("literal block must use the next indentation level", start)
        value = "\n".join(
            line[block_indent:] if line.strip() else "" for line in self.lines[start:end]
        )
        self.index = end
        return value.rstrip("\n") + "\n"

    def _parse_value(self, raw, parent_indent):
        raw = self._strip_comment(raw.strip())
        if raw == "|":
            return self._parse_literal(parent_indent)
        if raw:
            return self._parse_scalar(raw)
        next_index, next_indent, _ = self._next_content()
        if next_index is None or next_indent <= parent_indent:
            return None
        if next_indent != parent_indent + 2:
            self._error("nested value must use the next indentation level", next_index)
        self.index = next_index
        return self._parse_block(next_indent)

    def _parse_mapping_entry(self, content, indent, mapping):
        match = self.KEY.fullmatch(content)
        if not match:
            self._error("expected a simple mapping key")
        key, raw = match.groups()
        if key in mapping:
            self._error("duplicate key {!r}".format(key))
        self.index += 1
        mapping[key] = self._parse_value(raw, indent)

    def _parse_mapping(self, indent, initial=None):
        mapping = {} if initial is None else initial
        while True:
            self._skip_ignored()
            if self.index == len(self.lines):
                break
            current_indent, content = self._line()
            if current_indent < indent:
                break
            if current_indent != indent or content.startswith("- "):
                self._error("unexpected mapping indentation")
            self._parse_mapping_entry(content, indent, mapping)
        return mapping

    def _parse_sequence(self, indent):
        sequence = []
        while True:
            self._skip_ignored()
            if self.index == len(self.lines):
                break
            current_indent, content = self._line()
            if current_indent < indent:
                break
            if current_indent != indent or not content.startswith("- "):
                self._error("unexpected sequence indentation")
            item = content[2:]
            match = self.KEY.fullmatch(item)
            if match:
                mapping = {}
                self._parse_mapping_entry(item, indent + 2, mapping)
                mapping = self._parse_mapping(indent + 2, mapping)
                sequence.append(mapping)
            else:
                self.index += 1
                sequence.append(self._parse_scalar(item))
        return sequence

    def _parse_block(self, indent):
        current_indent, content = self._line()
        if current_indent != indent:
            self._error("unexpected block indentation")
        if content.startswith("- "):
            return self._parse_sequence(indent)
        return self._parse_mapping(indent)


def parse_required_ci(workflow):
    document = _StrictWorkflowParser(workflow).parse()
    if not isinstance(document, dict):
        raise ValueError("workflow root must be a mapping")
    return document


def _exact_keys(value, expected, description, failures):
    if not isinstance(value, dict):
        failures.append("{} must be a mapping".format(description))
        return False
    actual = set(value)
    if actual != set(expected):
        failures.append(
            "{} keys changed: expected {}; got {}".format(
                description, sorted(expected), sorted(map(str, actual))
            )
        )
        return False
    return True


def _step_map(job, expected_names, description, failures):
    steps = job.get("steps") if isinstance(job, dict) else None
    if not isinstance(steps, list):
        failures.append("{} steps must be a sequence".format(description))
        return {}
    result = {}
    for step in steps:
        if not isinstance(step, dict) or not isinstance(step.get("name"), str):
            failures.append("{} contains a step without a string name".format(description))
            continue
        name = step["name"]
        if name in result:
            failures.append("{} contains duplicate step name {!r}".format(description, name))
        result[name] = step
    if [step.get("name") for step in steps if isinstance(step, dict)] != expected_names:
        failures.append("{} enabled step graph changed".format(description))
    return result


def _validate_checkout(step, description, failures):
    if not _exact_keys(step, {"name", "uses", "with"}, description, failures):
        return
    if step.get("uses") != CHECKOUT_ACTION:
        failures.append("{} action pin changed".format(description))
    if step.get("with") != {"fetch-depth": 0, "persist-credentials": False}:
        failures.append("{} trust settings changed".format(description))


def required_ci_failures(workflow):
    failures = []
    try:
        document = parse_required_ci(workflow)
    except ValueError as error:
        return ["Required CI YAML is invalid: {}".format(error)]

    if not _exact_keys(
        document,
        {"name", "on", "permissions", "concurrency", "env", "jobs"},
        "Required CI root",
        failures,
    ):
        return failures
    if document.get("name") != "Required CI":
        failures.append("Required CI workflow name changed")
    if document.get("on") != {
        "pull_request": None,
        "push": {"branches": ["main"]},
        "merge_group": None,
    }:
        failures.append("Required CI trigger contract changed")
    if document.get("permissions") != {"contents": "read"}:
        failures.append("Required CI permissions must remain contents: read")
    if document.get("concurrency") != {
        "group": "required-ci-${{ github.event.pull_request.number || github.event.merge_group.head_sha || github.ref }}",
        "cancel-in-progress": "${{ github.event_name == 'pull_request' }}",
    }:
        failures.append("Required CI concurrency contract changed")
    if document.get("env") != {
        "CARGO_TERM_COLOR": "always",
        "EASYCON_RUST_VERSION": "1.97.1",
    }:
        failures.append("Required CI root environment changed")

    jobs = document.get("jobs")
    if not _exact_keys(jobs, {"policy", "windows-workspace"}, "Required CI jobs", failures):
        return failures
    for job_name, job in jobs.items():
        environment = job.get("env") if isinstance(job, dict) else None
        if not isinstance(environment, dict):
            continue
        for variable, value in environment.items():
            if isinstance(value, str) and JOB_LEVEL_RUNNER_CONTEXT.search(value):
                failures.append(
                    "Required CI job {!r} environment {!r} uses unavailable runner context".format(
                        job_name, variable
                    )
                )
    policy = jobs["policy"]
    windows_workspace = jobs["windows-workspace"]
    if _exact_keys(
        policy,
        {"name", "runs-on", "timeout-minutes", "env", "steps"},
        "Required / Policy job",
        failures,
    ):
        if policy["name"] != "Required / Policy":
            failures.append("Required / Policy check name changed")
        if policy["runs-on"] != "windows-2022":
            failures.append("Required / Policy must run on windows-2022")
        if policy["timeout-minutes"] != 20:
            failures.append("Required / Policy timeout changed")
        if policy["env"] != {
            "CARGO_BUILD_TARGET": "x86_64-pc-windows-msvc",
            "CARGO_INCREMENTAL": "0",
        }:
            failures.append("Required / Policy target environment changed")

    policy_names = [
        "Check out the candidate",
        "Isolate generated outputs",
        "Load the MSVC x64 developer environment",
        "Install and verify the frozen Rust toolchain",
        "Run repository policy gates",
        "Check changed lines and repository cleanliness",
    ]
    policy_steps = _step_map(policy, policy_names, "Required / Policy", failures)
    if "Check out the candidate" in policy_steps:
        _validate_checkout(
            policy_steps["Check out the candidate"], "Required / Policy checkout", failures
        )
    for name in policy_names[1:]:
        step = policy_steps.get(name, {})
        allowed = {"name", "shell", "run"}
        if name == "Check changed lines and repository cleanliness":
            allowed.add("env")
        if _exact_keys(step, allowed, "Required / Policy step {!r}".format(name), failures):
            if step.get("shell") != "pwsh":
                failures.append("Required / Policy step {!r} shell changed".format(name))

    for name, expected_digest in POLICY_RUN_SHA256.items():
        script = policy_steps.get(name, {}).get("run")
        actual_digest = (
            hashlib.sha256(script.encode("utf-8")).hexdigest()
            if isinstance(script, str)
            else None
        )
        if actual_digest != expected_digest:
            failures.append(
                "Required / Policy step {!r} exact run block changed".format(name)
            )
    clean_step = policy_steps.get("Check changed lines and repository cleanliness", {})
    if clean_step.get("env") != {
        "BASE_SHA": "${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha || github.event.before }}"
    }:
        failures.append("Required / Policy base SHA contract changed")

    if _exact_keys(
        windows_workspace,
        {"name", "runs-on", "timeout-minutes", "steps"},
        "Required / Windows Workspace job",
        failures,
    ):
        if windows_workspace["name"] != "Required / Windows Workspace":
            failures.append("Required / Windows Workspace check name changed")
        if windows_workspace["runs-on"] != "windows-2022":
            failures.append("Required / Windows Workspace must run on windows-2022")
        if windows_workspace["timeout-minutes"] != 180:
            failures.append("Required / Windows Workspace timeout changed")

    workspace_names = [
        "Check out the candidate",
        "Initialize the controlled cache root",
        "Restore verified Setup assets",
        "Restore Cargo downloads",
        "Restore vcpkg binary cache",
        "Set up the pinned Windows build environment",
        "Run the complete Windows workspace gates",
        "Save verified Setup assets for this pull request",
        "Save verified Setup assets from trusted main",
        "Save Cargo downloads from trusted main",
        "Save vcpkg binaries from trusted main",
    ]
    workspace_steps = _step_map(
        windows_workspace, workspace_names, "Required / Windows Workspace", failures
    )
    if "Check out the candidate" in workspace_steps:
        _validate_checkout(
            workspace_steps["Check out the candidate"],
            "Required / Windows Workspace checkout",
            failures,
        )

    cache_root_step = workspace_steps.get("Initialize the controlled cache root", {})
    if _exact_keys(
        cache_root_step,
        {"name", "shell", "run"},
        "Windows workspace cache root initialization step",
        failures,
    ):
        if cache_root_step["shell"] != "pwsh":
            failures.append("Windows workspace cache root initialization shell changed")
        if cache_root_step["run"].strip() != WINDOWS_CACHE_ROOT_INITIALIZATION:
            failures.append(
                "Windows workspace cache root must be initialized from RUNNER_TEMP through GITHUB_ENV"
            )

    setup_assets_restore = workspace_steps.get("Restore verified Setup assets", {})
    if _exact_keys(
        setup_assets_restore,
        {"name", "id", "uses", "with"},
        "Required / Windows Workspace Setup asset restore",
        failures,
    ):
        if (
            setup_assets_restore["id"] != "setup_assets_cache"
            or setup_assets_restore["uses"] != CACHE_RESTORE_ACTION
        ):
            failures.append("Setup asset restore identity or action pin changed")
        if setup_assets_restore.get("with") != {
            "path": SETUP_ASSET_CACHE_PATHS,
            "key": SETUP_ASSET_CACHE_KEY,
            "restore-keys": SETUP_ASSET_RESTORE_KEYS,
        }:
            failures.append("Setup asset restore inputs or PR/main isolation changed")

    restore_contracts = {
        "Restore Cargo downloads": (
            "cargo_cache",
            "${{ runner.temp }}/easycon-windows-workspace/caches/cargo-download-cache",
            "windows-workspace-cargo-v5-${{ hashFiles('Cargo.lock', 'Cargo.toml', "
            "'crates/**/Cargo.toml', 'tests/support/Cargo.toml', 'rust-toolchain.toml') }}",
        ),
        "Restore vcpkg binary cache": (
            "vcpkg_cache",
            "${{ runner.temp }}/easycon-windows-workspace/caches/vcpkg-binary-cache",
            "windows-workspace-vcpkg-v5-${{ hashFiles("
            "'vcpkg.json', 'vcpkg-configuration.json', 'cmake/triplets/**', "
            "'CMakePresets.json') }}",
        ),
    }
    for name, (step_id, expected_path, expected_key) in restore_contracts.items():
        step = workspace_steps.get(name, {})
        if _exact_keys(
            step,
            {"name", "id", "uses", "with"},
            "Required / Windows Workspace step {!r}".format(name),
            failures,
        ):
            if step["id"] != step_id or step["uses"] != CACHE_RESTORE_ACTION:
                failures.append("{} restore identity or action pin changed".format(name))
            values = step.get("with")
            if values != {"path": expected_path, "key": expected_key}:
                failures.append("{} restore inputs changed".format(name))

    setup_step = workspace_steps.get("Set up the pinned Windows build environment", {})
    if _exact_keys(
        setup_step,
        {"name", "shell", "run"},
        "Windows environment Setup step",
        failures,
    ):
        if setup_step["shell"] != "pwsh" or setup_step["run"].strip() != SETUP_INVOCATION:
            failures.append("Windows environment Setup invocation changed or is not active")

    runner_step = workspace_steps.get("Run the complete Windows workspace gates", {})
    if _exact_keys(
        runner_step,
        {"name", "shell", "env", "run"},
        "Windows workspace runner step",
        failures,
    ):
        if runner_step["shell"] != "pwsh":
            failures.append("Windows workspace runner shell changed")
        if runner_step["env"] != {
            "BASE_SHA": "${{ github.event.pull_request.base.sha || github.event.merge_group.base_sha || github.event.before }}"
        }:
            failures.append("Windows workspace runner base SHA contract changed")
        if runner_step["run"].strip() != WORKSPACE_INVOCATION:
            failures.append("Windows workspace runner invocation changed or is not active")

    setup_asset_saves = {
        "Save verified Setup assets for this pull request": (
            "always() && github.event_name == 'pull_request'"
        ),
        "Save verified Setup assets from trusted main": (
            "github.event_name == 'push' && github.ref == 'refs/heads/main' && success()"
        ),
    }
    for name, expected_condition in setup_asset_saves.items():
        step = workspace_steps.get(name, {})
        if _exact_keys(
            step,
            {"name", "if", "uses", "with"},
            "Required / Windows Workspace step {!r}".format(name),
            failures,
        ):
            if step["if"] != expected_condition:
                failures.append("{} cache trust boundary changed".format(name))
            if step["uses"] != CACHE_SAVE_ACTION:
                failures.append("{} action pin changed".format(name))
            if step.get("with") != {
                "path": SETUP_ASSET_CACHE_PATHS,
                "key": "${{ steps.setup_assets_cache.outputs.cache-primary-key }}",
            }:
                failures.append("{} save inputs changed".format(name))

    save_contracts = {
        "Save Cargo downloads from trusted main": (
            "cargo_cache",
            "${{ runner.temp }}/easycon-windows-workspace/caches/cargo-download-cache",
            "${{ steps.cargo_cache.outputs.cache-primary-key }}",
        ),
        "Save vcpkg binaries from trusted main": (
            "vcpkg_cache",
            "${{ runner.temp }}/easycon-windows-workspace/caches/vcpkg-binary-cache",
            "${{ steps.vcpkg_cache.outputs.cache-primary-key }}",
        ),
    }
    for name, (restore_id, expected_path, expected_key) in save_contracts.items():
        step = workspace_steps.get(name, {})
        if _exact_keys(
            step,
            {"name", "if", "uses", "with"},
            "Required / Windows Workspace step {!r}".format(name),
            failures,
        ):
            expected_condition = (
                "github.event_name == 'push' && github.ref == 'refs/heads/main' && "
                "steps.{}.outputs.cache-hit != 'true'".format(restore_id)
            )
            if step["if"] != expected_condition:
                failures.append("{} is not restricted to trusted main cache misses".format(name))
            if step["uses"] != CACHE_SAVE_ACTION:
                failures.append("{} action pin changed".format(name))
            values = step.get("with")
            if values != {"path": expected_path, "key": expected_key}:
                failures.append("{} save inputs changed".format(name))

    for job in (policy, windows_workspace):
        for step in job.get("steps", []) if isinstance(job, dict) else []:
            if isinstance(step, dict) and "uses" in step:
                action = step["uses"]
                if action not in {CHECKOUT_ACTION, CACHE_RESTORE_ACTION, CACHE_SAVE_ACTION}:
                    failures.append("Required CI action is not pinned to an approved SHA: {!r}".format(action))
    return failures


def _unique_json_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate JSON key {!r}".format(key))
        result[key] = value
    return result


def _require_json_keys(value, expected, description):
    if type(value) is not dict:
        raise ValueError("{} must be a JSON object".format(description))
    actual = set(value)
    if actual != set(expected):
        raise ValueError(
            "{} keys must be exactly {}; got {}".format(
                description, sorted(expected), sorted(actual)
            )
        )


def _require_string(value, description):
    if type(value) is not str:
        raise ValueError("{} must be a JSON string".format(description))
    return value


def _is_system_version_semver(value):
    if not re.fullmatch(
        r"(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)", value
    ):
        return False
    return all(int(component) <= 2_147_483_647 for component in value.split("."))


def parse_windows_build_environment(text):
    try:
        configuration = json.loads(text, object_pairs_hook=_unique_json_object)
    except (json.JSONDecodeError, ValueError) as error:
        raise ValueError("invalid JSON: {}".format(error)) from error

    _require_json_keys(
        configuration, WINDOWS_CONFIGURATION_KEYS, "Windows build environment root"
    )
    if type(configuration["version"]) is not int or configuration["version"] != 3:
        raise ValueError("Windows build environment version must be the JSON integer 3")
    if configuration["target"] != "x86_64-pc-windows-msvc":
        raise ValueError("Windows build target must remain x86_64-pc-windows-msvc")
    fingerprint_inputs = configuration["fingerprintInputs"]
    if type(fingerprint_inputs) is not list:
        raise ValueError("fingerprintInputs must be a JSON array")
    for index, fingerprint_input in enumerate(fingerprint_inputs):
        _require_json_keys(
            fingerprint_input,
            FINGERPRINT_INPUT_KEYS,
            "fingerprint input {}".format(index),
        )
        path = _require_string(fingerprint_input["path"], "fingerprint input path")
        components = path.split("/")
        if (
            not re.fullmatch(r"[A-Za-z0-9._/-]+", path)
            or path.startswith("/")
            or "//" in path
            or any(component in ("", ".", "..") for component in components)
        ):
            raise ValueError(
                "fingerprint input must be one normalized repository-relative path"
            )
        kind = _require_string(fingerprint_input["kind"], "fingerprint input kind")
        if kind not in ("text", "binary"):
            raise ValueError("fingerprint input kind must be exactly text or binary")
    identities = [item["path"].casefold() for item in fingerprint_inputs]
    if len(identities) != len(set(identities)):
        raise ValueError("fingerprintInputs contains duplicate Windows path identity")
    expected_fingerprint_paths = [
        "tools/windows_build_environment.json",
        "tools/windows_workspace.psm1",
        "tools/run_windows_workspace.ps1",
        "rust-toolchain.toml",
        "Cargo.toml",
        "Cargo.lock",
        "crates/easycon-model/Cargo.toml",
        "crates/easycon-runtime/Cargo.toml",
        "crates/easycon-controller/Cargo.toml",
        "crates/easycon-ecs/Cargo.toml",
        "crates/easycon-serial/Cargo.toml",
        "crates/easycon-native-sys/Cargo.toml",
        "crates/easycon-vision/Cargo.toml",
        "tests/support/Cargo.toml",
        "vcpkg.json",
        "vcpkg-configuration.json",
        "cmake/triplets/x64-windows-static-md.cmake",
        "CMakePresets.json",
        "spec/fixtures/vision/ocr-model.json",
        "tools/provision_vision_test_model.py",
    ]
    expected_fingerprint_inputs = [
        {"path": path, "kind": "text"} for path in expected_fingerprint_paths
    ]
    if fingerprint_inputs != expected_fingerprint_inputs:
        raise ValueError("Windows environment fingerprint input set or order changed")
    model_directory = _require_string(
        configuration["visionModelDirectory"], "OCR model directory"
    )
    if not re.fullmatch(r"[A-Za-z0-9._-]+", model_directory):
        raise ValueError("OCR model directory must be one portable path component")

    host_tools = configuration["hostTools"]
    _require_json_keys(host_tools, HOST_TOOL_KEYS, "host tool configuration")
    for name in ("pythonMinimumVersion", "msvcToolsVersion"):
        value = _require_string(host_tools[name], name)
        if not _is_system_version_semver(value):
            raise ValueError("{} must be an exact semantic version".format(name))
    if not re.fullmatch(r"(?:0|[1-9][0-9]*)", _require_string(
        host_tools["visualStudioMajorVersion"], "Visual Studio major version"
    )):
        raise ValueError("Visual Studio major version must be ASCII digits")
    if not re.fullmatch(
        r"(?:0|[1-9][0-9]*)(?:\.(?:0|[1-9][0-9]*)){3}",
        _require_string(host_tools["windowsSdkVersion"], "Windows SDK version"),
    ):
        raise ValueError("Windows SDK version must contain four exact numeric components")

    vcpkg = configuration["vcpkg"]
    _require_json_keys(vcpkg, VCPKG_CONFIGURATION_KEYS, "vcpkg configuration")
    scripts_repository = _require_string(
        vcpkg["scriptsRepository"], "vcpkg scripts repository"
    )
    if scripts_repository != "https://github.com/microsoft/vcpkg.git":
        raise ValueError("vcpkg scripts repository URL is not exactly canonical")
    for name in ("scriptsCommit", "registryBaseline", "toolCommit"):
        value = _require_string(vcpkg[name], "vcpkg {}".format(name))
        if not re.fullmatch(r"[0-9a-f]{40}", value):
            raise ValueError("vcpkg {} must be 40 lowercase hexadecimal digits".format(name))
    if vcpkg["registryBaseline"] != vcpkg["scriptsCommit"]:
        raise ValueError("vcpkg scripts and registry pins must remain separately equal")
    release = _require_string(vcpkg["toolRelease"], "vcpkg tool release")
    if not re.fullmatch(r"[0-9]{4}-[0-9]{2}-[0-9]{2}", release):
        raise ValueError("vcpkg tool release must be an ISO calendar date")
    try:
        datetime.date.fromisoformat(release)
    except ValueError as error:
        raise ValueError("vcpkg tool release must be a valid calendar date") from error

    asset = vcpkg["windowsAsset"]
    _require_json_keys(asset, WINDOWS_ASSET_KEYS, "Windows vcpkg asset")
    if _require_string(asset["name"], "Windows vcpkg asset name") != "vcpkg.exe":
        raise ValueError("Windows vcpkg asset name changed")
    expected_url = (
        "https://github.com/microsoft/vcpkg-tool/releases/download/{}/vcpkg.exe".format(
            release
        )
    )
    if _require_string(asset["url"], "Windows vcpkg asset URL") != expected_url:
        raise ValueError("Windows vcpkg asset URL is not exactly canonical")
    if type(asset["bytes"]) is not int or asset["bytes"] <= 0:
        raise ValueError("Windows vcpkg asset bytes must be a positive JSON integer")
    sha256 = _require_string(asset["sha256"], "Windows vcpkg asset SHA-256")
    if not re.fullmatch(r"[0-9a-f]{64}", sha256):
        raise ValueError("Windows vcpkg asset SHA-256 must be lowercase hexadecimal")

    tools_manifest = vcpkg["toolsManifest"]
    _require_json_keys(tools_manifest, {"path", "sha256"}, "vcpkg tools manifest")
    if tools_manifest["path"] != "scripts/vcpkg-tools.json":
        raise ValueError("vcpkg tools manifest path changed")
    if not re.fullmatch(
        r"[0-9a-f]{64}",
        _require_string(tools_manifest["sha256"], "vcpkg tools manifest SHA-256"),
    ):
        raise ValueError("vcpkg tools manifest SHA-256 must be lowercase hexadecimal")

    internal_tools = vcpkg["internalTools"]
    if type(internal_tools) is not list:
        raise ValueError("vcpkg internalTools must be a JSON array")
    internal_names = []
    for index, tool in enumerate(internal_tools):
        tool_keys = INTERNAL_TOOL_KEYS
        if isinstance(tool, dict) and tool.get("name") == "7zip":
            tool_keys = tool_keys | {"executableSha256"}
        _require_json_keys(tool, tool_keys, "internal tool {}".format(index))
        name = _require_string(tool["name"], "internal tool name")
        internal_names.append(name)
        version = _require_string(tool["version"], "{} version".format(name))
        if not re.fullmatch(r"[0-9]+(?:\.[0-9]+){1,2}", version):
            raise ValueError("{} version must be numeric and exact".format(name))
        parsed_url = urlsplit(_require_string(tool["url"], "{} URL".format(name)))
        if (
            parsed_url.scheme != "https"
            or parsed_url.username is not None
            or parsed_url.password is not None
            or parsed_url.port not in (None, 443)
            or parsed_url.query
            or parsed_url.fragment
            or not parsed_url.hostname
        ):
            raise ValueError("{} URL must be canonical HTTPS".format(name))
        for field in ("archive", "executable"):
            if not _require_string(tool[field], "{} {}".format(name, field)):
                raise ValueError("{} {} must not be empty".format(name, field))
        if not re.fullmatch(
            r"[0-9a-f]{128}", _require_string(tool["sha512"], "{} SHA-512".format(name))
        ):
            raise ValueError("{} SHA-512 must be lowercase hexadecimal".format(name))
        if name == "7zip" and not re.fullmatch(
            r"[0-9a-f]{64}",
            _require_string(tool["executableSha256"], "7zip executable SHA-256"),
        ):
            raise ValueError("7zip executable SHA-256 must be lowercase hexadecimal")
    if sorted(internal_names) != ["7zip", "7zr", "cmake", "ninja"]:
        raise ValueError("internal tools must be exactly 7zip, 7zr, cmake, and ninja")
    seven_zr = next(tool for tool in internal_tools if tool["name"] == "7zr")
    expected_seven_zr_name = seven_zr["sha512"][:8] + "-7zr.exe"
    if seven_zr["archive"] != expected_seven_zr_name:
        raise ValueError("7zr local download name must match the vcpkg content hash prefix")

    dependencies = vcpkg["nativeDependencies"]
    if type(dependencies) is not list:
        raise ValueError("vcpkg nativeDependencies must be a JSON array")
    dependency_names = []
    for index, dependency in enumerate(dependencies):
        _require_json_keys(
            dependency, NATIVE_DEPENDENCY_KEYS, "native dependency {}".format(index)
        )
        name = _require_string(dependency["name"], "native dependency name")
        dependency_names.append(name)
        if not re.fullmatch(
            r"[0-9]+(?:\.[0-9]+){1,2}",
            _require_string(dependency["version"], "{} version".format(name)),
        ):
            raise ValueError("{} version must be numeric and exact".format(name))
        if type(dependency["portVersion"]) is not int or dependency["portVersion"] < 0:
            raise ValueError("{} portVersion must be a non-negative integer".format(name))
        if not re.fullmatch(
            r"[0-9a-f]{40}",
            _require_string(dependency["gitTree"], "{} git tree".format(name)),
        ):
            raise ValueError("{} git tree must be lowercase hexadecimal".format(name))
    if sorted(dependency_names) != ["leptonica", "opencv4", "tesseract"]:
        raise ValueError(
            "native dependencies must be exactly leptonica, opencv4, and tesseract"
        )
    return configuration


def windows_build_environment_failures(configuration, native_quality):
    failures = []
    try:
        configuration = parse_windows_build_environment(
            json.dumps(configuration, ensure_ascii=True, separators=(",", ":"))
        )
    except ValueError as error:
        return ["Windows build environment config is invalid: {}".format(error)]

    vcpkg = configuration["vcpkg"]
    asset = vcpkg["windowsAsset"]

    native_pins = {
        "EASYCON_VCPKG_SCRIPTS_COMMIT": str(vcpkg.get("scriptsCommit", "")),
        "EASYCON_VCPKG_REGISTRY_BASELINE": str(vcpkg.get("registryBaseline", "")),
        "EASYCON_VCPKG_TOOL_RELEASE": str(vcpkg.get("toolRelease", "")),
        "EASYCON_VCPKG_TOOL_COMMIT": str(vcpkg.get("toolCommit", "")),
        "EASYCON_VCPKG_WINDOWS_ASSET_BYTES": str(asset.get("bytes", "")),
        "EASYCON_VCPKG_WINDOWS_ASSET_SHA256": str(asset.get("sha256", "")),
    }
    for name, expected in native_pins.items():
        match = re.search(
            r"(?m)^  {}: [\"']?([^\"'\r\n]+)[\"']?\s*$".format(
                re.escape(name)
            ),
            native_quality,
        )
        actual = match.group(1).strip() if match else ""
        if actual != expected:
            failures.append(
                "native-quality {} does not match windows_build_environment.json".format(
                    name
                )
            )
    return failures


def main():
    failures = repository_guard_regression_failures()
    windows_build_configuration_text = (
        ROOT / "tools/windows_build_environment.json"
    ).read_text(encoding="utf-8")
    try:
        windows_build_configuration = parse_windows_build_environment(
            windows_build_configuration_text
        )
    except ValueError as error:
        failures.append("Windows build environment config is invalid: {}".format(error))
        windows_build_configuration = None
    required_ci = (ROOT / ".github/workflows/required-ci.yml").read_text(encoding="utf-8")
    failures.extend(required_ci_failures(required_ci))
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
    if not WINDOWS_BUILD_FILES.issubset(tracked_set):
        failures.append("tracked Windows workspace bootstrap files are incomplete")

    metadata = cargo_metadata()
    failures.extend(w0_boundary_failures(tracked, metadata))
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

    vcpkg_configuration_data = json.loads(vcpkg_configuration)
    registry_baseline = vcpkg_configuration_data["default-registry"]["baseline"]
    scripts_commit = (
        windows_build_configuration["vcpkg"]["scriptsCommit"]
        if windows_build_configuration is not None
        else None
    )
    if windows_build_configuration is not None and scripts_commit != registry_baseline:
        failures.append(
            "Windows vcpkg scripts pin and registry baseline must be separately verified "
            "at the frozen commit"
        )

    native_quality = (ROOT / ".github/workflows/native-quality.yml").read_text(
        encoding="utf-8"
    )
    if windows_build_configuration is not None:
        failures.extend(
            windows_build_environment_failures(
                windows_build_configuration, native_quality
            )
        )

    windows_module = (ROOT / "tools/windows_workspace.psm1").read_text(
        encoding="utf-8"
    )
    for forbidden in (
        r"C:\\Users\\",
        r"D:\\project",
        "git config --global",
        'SetEnvironmentVariable($name, $value, "User")',
        'SetEnvironmentVariable($name, $value, "Machine")',
    ):
        if forbidden in windows_module:
            failures.append(
                "Windows workspace bootstrap contains forbidden persistent or machine-local "
                "state: {}".format(forbidden)
            )

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
