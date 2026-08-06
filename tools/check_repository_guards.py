#!/usr/bin/env python3
"""Enforce frozen workspace, dependency, license, and architecture guards."""

import copy
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
    "crates/easycon-file-identity",
    "crates/easycon-serial",
    "crates/easycon-native-sys",
    "crates/easycon-vision",
    "tests/support",
}
W0_ZERO_DEPENDENCY_MANIFEST = "crates/easycon-ecs/Cargo.toml"
F0_FOUNDATION_MANIFEST = "crates/easycon-file-identity/Cargo.toml"
F0_FOUNDATION_SOURCE = "crates/easycon-file-identity/src/lib.rs"
F0_FOUNDATION_WINDOWS_TEST = (
    "crates/easycon-file-identity/tests/windows_file_objects.rs"
)
F0_ADMISSION_RECORD = "docs/architecture/rust-dependency-admission.json"
F0_LEGACY_MANIFEST = "tests/hardware/file-id-handle/Cargo.toml"
F0_LEGACY_SOURCE = "tests/hardware/file-id-handle/src/lib.rs"
F0_HARDWARE_MANIFEST = "tests/hardware/Cargo.toml"
F0_HARDWARE_SOURCE = "tests/hardware/src/artifact.rs"
F0_ASSERTION_IDS = (
    "F0-NO-DETACHED-PUBLIC-AUTHORITY",
    "F0-QUERY-QUALITY-FAIL-CLOSED",
    "F0-TWO-LIVE-HANDLE-DROP-TRACE",
    "F0-ROOT-DIR-FILE-BORROWED-HANDLES",
    "F0-HARDWARE-OWNERSHIP-MIGRATION",
)
F0_REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
F0_EXPECTED_REGISTRY_PACKAGES = {
    "windows-link": {
        "name": "windows-link",
        "version": "0.2.1",
        "source": F0_REGISTRY_SOURCE,
        "checksum": "f0805222e57f7521d6a62e36fa9163bc891acd422f971defe97d64e70d0a4fe5",
        "dependencies": [],
    },
    "windows-sys": {
        "name": "windows-sys",
        "version": "0.61.2",
        "source": F0_REGISTRY_SOURCE,
        "checksum": "ae137229bcbd6cdf0f7b80a31df61766145077ddf49416a728b02cb3921ff3fc",
        "dependencies": ["windows-link"],
    },
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
WINDOWS_BUILD_FILES = {
    "tools/run_windows_workspace.ps1",
    "tools/test_windows_environment_lifecycle.ps1",
    "tools/test_windows_workspace.ps1",
    "tools/test_windows_bootstrap_contracts.py",
    "tools/windows_build_environment.json",
    "tools/windows_gate_policy.json",
    "tools/windows_gate_policy.ps1",
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
WINDOWS_GATE_POLICY_KEYS = {"version", "cargoJobs", "gates"}
WINDOWS_GATE_POLICY_GATE_KEYS = {"name", "tool", "arguments"}


def expected_windows_environment_fingerprint_paths():
    member_manifests = sorted(
        "{}/Cargo.toml".format(member) for member in EXPECTED_MEMBERS
    )
    return [
        "tools/windows_build_environment.json",
        "tools/windows_workspace.psm1",
        "rust-toolchain.toml",
        "Cargo.toml",
        "Cargo.lock",
        *member_manifests,
        "vcpkg.json",
        "vcpkg-configuration.json",
        "cmake/triplets/x64-windows-static-md.cmake",
        "CMakePresets.json",
        "spec/fixtures/vision/ocr-model.json",
        "tools/provision_vision_test_model.py",
    ]


def expected_windows_gate_policy_gates():
    return [
        ("cargo fmt --all --check", "cargo", ["fmt", "--all", "--check"]),
        ("cargo check --locked --jobs 4 --workspace --all-targets", "cargo", ["check", "--locked", "--workspace", "--all-targets"]),
        ("cargo clippy --locked --jobs 4 --workspace --all-targets --all-features -- -D warnings", "cargo", ["clippy", "--locked", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]),
        ("cargo test --locked --jobs 4 --workspace --all-features", "cargo", ["test", "--locked", "--workspace", "--all-features"]),
        ("python tools/run_runtime_models.py", "python", ["tools/run_runtime_models.py"]),
        ("python tools/validate_specs.py", "python", ["tools/validate_specs.py"]),
        ("python tools/check_markdown_links.py", "python", ["tools/check_markdown_links.py"]),
        ("python tools/check_repository_guards.py", "python", ["tools/check_repository_guards.py"]),
        ("python tools/test_windows_bootstrap_contracts.py", "python", ["tools/test_windows_bootstrap_contracts.py"]),
        ("pwsh tools/test_windows_workspace.ps1", "pwsh", ["-NoLogo", "-NoProfile", "-File", "tools/test_windows_workspace.ps1"]),
        ("pwsh tools/test_windows_environment_lifecycle.ps1", "pwsh", ["-NoLogo", "-NoProfile", "-File", "tools/test_windows_environment_lifecycle.ps1"]),
        ("git diff --check", "git", ["diff", "--check"]),
    ]


def git_files():
    output = subprocess.check_output(
        ["git", "ls-files", "--cached", "--others", "--exclude-standard", "-z"],
        cwd=str(ROOT),
        stderr=subprocess.STDOUT,
    )
    return [item.decode("utf-8") for item in output.split(b"\0") if item]


def cargo_metadata(manifest_path=None):
    command = ["cargo", "metadata", "--format-version", "1", "--no-deps"]
    if manifest_path is not None:
        command.extend(["--locked", "--manifest-path", manifest_path])
    output = subprocess.check_output(
        command,
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


def expected_f0_foundation_manifest():
    return """[package]
name = "easycon-file-identity"
description = "Safe borrowed-handle file identity foundation for EasyCon SDK"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license = "GPL-3.0-only"
publish = false

[target.'cfg(windows)'.dependencies]
windows-sys = { version = "=0.61.2", default-features = false, features = [
    "Win32_Foundation",
    "Win32_Storage_FileSystem",
] }

[lints.rust]
unsafe_code = "deny"
unsafe_op_in_unsafe_fn = "deny"

[lints.clippy]
all = { level = "warn", priority = -1 }
"""


def expected_f0_admission():
    return {
        "schema_version": 1,
        "authority": "ADR-0021 F0",
        "project_license": "GPL-3.0-only",
        "packages": [
            {
                "name": "windows-link",
                "version": "0.2.1",
                "role": "transitive",
                "source": F0_REGISTRY_SOURCE,
                "checksum": F0_EXPECTED_REGISTRY_PACKAGES["windows-link"]["checksum"],
                "license_expression": "MIT OR Apache-2.0",
                "license_compatible_with": "GPL-3.0-only",
                "declared_rust_version": "1.71",
                "tested_rust_version": "1.97.1",
            },
            {
                "name": "windows-sys",
                "version": "0.61.2",
                "role": "direct",
                "source": F0_REGISTRY_SOURCE,
                "checksum": F0_EXPECTED_REGISTRY_PACKAGES["windows-sys"]["checksum"],
                "license_expression": "MIT OR Apache-2.0",
                "license_compatible_with": "GPL-3.0-only",
                "declared_rust_version": "1.71",
                "tested_rust_version": "1.97.1",
            },
        ],
        "dependency_edges": [
            {
                "from": "easycon-file-identity",
                "to": "windows-sys",
                "requirement": "=0.61.2",
                "target": "cfg(windows)",
                "kind": "normal",
                "optional": False,
                "rename": None,
                "default_features": False,
                "features": [
                    "Win32_Foundation",
                    "Win32_Storage_FileSystem",
                ],
            },
            {
                "from": "windows-sys@0.61.2",
                "to": "windows-link",
                "requirement": "0.2.1",
                "target": None,
                "kind": "normal",
                "optional": False,
                "rename": None,
                "default_features": False,
                "features": [],
            },
        ],
        "fixed_toolchain_evidence": {
            "rustc_release": "1.97.1",
            "rustc_commit": "8bab26f4f68e0e26f0bb7960be334d5b520ea452",
            "host": "x86_64-pc-windows-msvc",
            "root_command": "cargo test --locked -p easycon-file-identity --all-features",
            "hardware_command": "cargo test --locked --workspace --all-features",
            "result": "passed",
        },
    }


def normalized_metadata_dependency(dependency):
    path = dependency.get("path")
    return {
        "name": dependency.get("name"),
        "req": dependency.get("req"),
        "kind": dependency.get("kind"),
        "rename": dependency.get("rename"),
        "optional": dependency.get("optional"),
        "uses_default_features": dependency.get("uses_default_features"),
        "features": dependency.get("features"),
        "target": dependency.get("target"),
        "path": str(Path(path).resolve()) if path is not None else None,
    }


def f0_foundation_manifest_failures(manifest_text, package):
    failures = []
    if manifest_text.replace("\r\n", "\n") != expected_f0_foundation_manifest():
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: foundation package/Windows dependency/"
            "lint manifest text differs from the exact admitted contract"
        )
    if (
        package.get("name") != "easycon-file-identity"
        or package.get("license") != "GPL-3.0-only"
        or package.get("publish") != []
        or package.get("rust_version") != "1.97.1"
        or [
            normalized_metadata_dependency(dependency)
            for dependency in package.get("dependencies", [])
        ]
        != [
            {
                "name": "windows-sys",
                "req": "=0.61.2",
                "kind": None,
                "rename": None,
                "optional": False,
                "uses_default_features": False,
                "features": [
                    "Win32_Foundation",
                    "Win32_Storage_FileSystem",
                ],
                "target": "cfg(windows)",
                "path": None,
            }
        ]
    ):
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: cargo metadata does not expose the exact "
            "internal package and Windows-only normal dependency"
        )
    return failures


def f0_hardware_manifest_failures(manifest_text, package, workspace_members):
    failures = []
    if workspace_members != {"tests/hardware"} or re.search(
        r"(?m)^members\s*=", manifest_text
    ):
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: hardware workspace still has a private "
            "identity member or changed resolver shape"
        )
    identity_dependencies = [
        normalized_metadata_dependency(dependency)
        for dependency in package.get("dependencies", [])
        if dependency.get("name") == "easycon-file-identity"
        or dependency.get("rename") == "easycon-hardware-file-id"
    ]
    if identity_dependencies != [
        {
            "name": "easycon-file-identity",
            "req": "*",
            "kind": None,
            "rename": "easycon-hardware-file-id",
            "optional": False,
            "uses_default_features": True,
            "features": [],
            "target": "cfg(windows)",
            "path": str((ROOT / "crates/easycon-file-identity").resolve()),
        }
    ] or (
        'easycon-hardware-file-id = { package = "easycon-file-identity", '
        'path = "../../crates/easycon-file-identity" }'
        not in manifest_text
    ):
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: hardware alias must be the exact "
            "Windows-only root package/path dependency"
        )
    if "file-id-handle" in manifest_text:
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: legacy helper path remains in hardware manifest"
        )
    return failures


def parse_cargo_lock_packages(text):
    packages = []
    sections = re.split(r"(?m)^\[\[package\]\]\s*$", text)
    for section in sections[1:]:
        package = {}
        for key in ("name", "version", "source", "checksum"):
            match = re.search(r'(?m)^{} = "([^"]*)"\s*$'.format(key), section)
            if match is not None:
                package[key] = match.group(1)
        dependencies = re.search(
            r"(?ms)^dependencies = \[(.*?)^\]\s*$", section
        )
        if dependencies is not None:
            package["dependencies"] = re.findall(
                r'^\s*"([^"]+)",?\s*$', dependencies.group(1), re.MULTILINE
            )
        if package.get("name") is not None:
            packages.append(package)
    return {"package": packages}


def normalized_lock_package(package):
    return {
        "name": package.get("name"),
        "version": package.get("version"),
        "source": package.get("source"),
        "checksum": package.get("checksum"),
        "dependencies": package.get("dependencies", []),
    }


def f0_lock_failures(lock, label):
    failures = []
    packages = lock.get("package", [])
    foundation = [
        package for package in packages if package.get("name") == "easycon-file-identity"
    ]
    if len(foundation) != 1 or foundation[0] != {
        "name": "easycon-file-identity",
        "version": "0.1.0",
        "dependencies": ["windows-sys"],
    }:
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: {} lock does not contain the exact "
            "foundation path package".format(label)
        )
    for name, expected in F0_EXPECTED_REGISTRY_PACKAGES.items():
        matches = [package for package in packages if package.get("name") == name]
        if len(matches) != 1 or normalized_lock_package(matches[0]) != expected:
            failures.append(
                "F0-QUERY-QUALITY-FAIL-CLOSED: {} lock {} version/source/checksum/"
                "closure differs from dependency admission".format(label, name)
            )
    return failures


def f0_admission_failures(admission):
    if admission == expected_f0_admission():
        return []
    return [
        "F0-QUERY-QUALITY-FAIL-CLOSED: dependency admission version/source/checksum/"
        "license/declared-and-tested-Rust evidence differs from the frozen record"
    ]


def f0_foundation_source_failures(source, windows_test, ffi_sources):
    failures = []
    production_source = source.split("#[cfg(test)]", 1)[0]
    if F0_ASSERTION_IDS[0] not in source:
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: stable assertion ID is missing"
        )
    public_items = re.findall(
        r"(?m)^pub\s+(?:unsafe\s+)?(fn|struct|enum|type|trait|mod|const|static)\s+"
        r"([A-Za-z_][A-Za-z0-9_]*)",
        production_source,
    )
    if public_items != [
        ("fn", "validate_file_object"),
        ("fn", "same_file_object"),
    ] or not re.search(
        r"pub fn validate_file_object\(file: &File\) -> io::Result<\(\)>",
        production_source,
    ) or not re.search(
        r"pub fn same_file_object\(left: &File, right: &File\) -> io::Result<bool>",
        production_source,
    ) or len(
        re.findall(r"(?m)^\s*pub(?:\([^)]*\))?\s+", production_source)
    ) != 2 or (
        "validate_file_object_with(file, &mut SystemFileIdentityQuery)"
        not in production_source
    ) or (
        "same_file_object_with(left, right, &mut SystemFileIdentityQuery)"
        not in production_source
    ):
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: public surface is not exactly borrowed "
            "File validation and comparison"
        )
    if (
        "#[derive(Clone, Copy, Debug, Eq, PartialEq)]\n"
        "struct HighResolutionFileIdentity {\n    volume_serial_number: u64,\n"
        "    file_id: u128,\n}" not in production_source
        or "file_id: u128::from_le_bytes(identifier)" not in production_source
        or production_source.count("left == right") != 1
        or "std::ptr::eq" in production_source
        or "let left_identity = validated_identity_with(left, query)?;\n"
        "    let right_identity = validated_identity_with(right, query)?;\n"
        "    Ok(query.compare(left_identity, right_identity))" not in production_source
    ):
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: private full u64/u128 identity or "
            "mandatory left-then-right borrowed comparison changed"
        )
    if (
        "#![deny(unsafe_code)]" not in production_source
        or "#![deny(unsafe_op_in_unsafe_fn)]" not in production_source
        or production_source.count("#[allow(unsafe_code)]") != 1
        or len(re.findall(r"\bunsafe\s*\{", production_source)) != 1
        or production_source.count("GetFileInformationByHandleEx(") != 1
        or production_source.count("SAFETY:") != 1
        or "borrowed from a live `File` for this one call" not in production_source
        or "selects the exact `FILE_ID_INFO` layout" not in production_source
        or "buffer of exactly `buffer_size` bytes" not in production_source
        or "retains neither the handle nor pointer" not in production_source
    ):
        failures.append(
            "F0-QUERY-QUALITY-FAIL-CLOSED: unique private unsafe FILE_ID_INFO leaf or "
            "its exact SAFETY proof changed"
        )
    ffi_paths = {
        path
        for path, text in ffi_sources.items()
        if any(
            token in text
            for token in (
                "GetFileInformationByHandleEx",
                "FILE_ID_INFO",
                "AsRawHandle",
            )
        )
    }
    if ffi_paths != {F0_FOUNDATION_SOURCE}:
        failures.append(
            "F0-QUERY-QUALITY-FAIL-CLOSED: FILE_ID_INFO FFI exists outside the unique "
            "foundation source"
        )
    if (
        F0_ASSERTION_IDS[1] not in source
        or "identifier.ok_or_else(|| invalid_identity(\"FILE_ID_INFO result is incomplete\"))?"
        not in production_source
        or "if volume_serial_number == 0 {" not in production_source
        or "if identifier_is_all_zero(&identifier) {" not in production_source
        or "identifier.iter().all(|byte| *byte == 0)" not in production_source
        or "file_id: u128::from_le_bytes(identifier)" not in production_source
        or "if succeeded == 0 {" not in production_source
        or "return Err(io::Error::last_os_error());" not in production_source
        or "rejected_quality_results()" not in source
        or "query_quality_failures_reach_both_safe_api_semantics_without_a_fallback"
        not in source
        or "io::ErrorKind::Unsupported" not in source
        or "low_half_only" not in source
        or "comparison_uses_volume_and_all_128_identifier_bits" not in source
    ):
        failures.append(
            "F0-QUERY-QUALITY-FAIL-CLOSED: Win32/unsupported/zero/incomplete quality "
            "assertions are incomplete"
        )
    if (
        F0_ASSERTION_IDS[2] not in source
        or "left-query" not in source
        or "right-query" not in source
        or "compare" not in source
        or "return" not in source
        or "drop-current" not in source
        or "drop-retained" not in source
        or "assert_two_live_handle_trace(true, true)" not in source
        or "assert_two_live_handle_trace(false, false)" not in source
        or "same_reference_is_queried_twice_before_comparison" not in source
        or "same_file_object_with(&file, &file, &mut query)" not in source
    ):
        failures.append(
            "F0-TWO-LIVE-HANDLE-DROP-TRACE: exact same/distinct two-live-handle event "
            "and drop trace is missing"
        )
    if (
        F0_ASSERTION_IDS[3] not in windows_test
        or "volume_or_share_root" not in windows_test
        or "ordinary_directory" not in windows_test
        or "regular_file" not in windows_test
        or "root_retained" not in windows_test
        or "directory_retained" not in windows_test
        or "file_retained" not in windows_test
        or windows_test.count("assert_true_false_error(") != 4
        or "FILE_FLAG_OPEN_REPARSE_POINT" not in windows_test
        or 'File::open("NUL")' not in windows_test
        or "assert_true_false_error" not in windows_test
        or "same_file_object(retained, current)" not in windows_test
        or "same_file_object(retained, distinct)" not in windows_test
        or "same_file_object(retained, unsupported).is_err()" not in windows_test
        or "#[ignore]" in windows_test
    ):
        failures.append(
            "F0-ROOT-DIR-FILE-BORROWED-HANDLES: real nofollow root/directory/file "
            "true/false/error assertion changed or became ignored"
        )
    if "#[ignore]" in source:
        failures.append("F0 stable foundation assertions must remain non-ignored")
    return failures


def f0_hardware_source_failures(source):
    failures = []
    if (
        "HighResolutionFileIdentity" in source
        or "high_resolution_identity" in source
        or re.search(r"(?m)^\s*identity:\s*", source)
    ):
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: hardware retains a detached identity authority"
        )
    if not re.search(
        r"trait FileIdentityProvider\s*\{\s*fn same_file_object\("
        r"&self, left: &File, right: &File\) -> io::Result<bool>;\s*\}",
        source,
        re.DOTALL,
    ):
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: hardware comparison seam must be "
            "opaque io::Result<bool> over two borrowed Files"
        )
    guarded_match = re.search(
        r"struct GuardedDiskFile\s*\{(?P<body>.*?)\}", source, re.DOTALL
    )
    if guarded_match is None or "file: File" not in guarded_match.group("body"):
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: GuardedDiskFile no longer retains its File"
        )
    if (
        F0_ASSERTION_IDS[4] not in source
        or "validate_guarded_file_object(&file)" not in source
        or "easycon_hardware_file_id::validate_file_object(file)" not in source
        or source.count("easycon_hardware_file_id::same_file_object") != 1
        or source.count("same_guarded_file_object(") != 5
        or 'same_guarded_file_object(&first, &second, "checkpoint input")' not in source
        or '"manifest staging/final"' not in source
        or '"completion staging/final"' not in source
        or '"manifest same_file_as"' not in source
        or ".same_file_object(&left.file, &right.file)" not in source
        or ".same_file_object(&staging.file, &guard)" not in source
        or "#[ignore]" in source
    ):
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: publication/manifest/checkpoint paths "
            "do not all compare two retained Files through the shared safe API"
        )
    return failures


def f0_root_manifest_failures(manifest_text, members):
    failures = []
    if members != EXPECTED_MEMBERS:
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: root workspace member set does not "
            "contain exactly one foundation"
        )
    normalized = manifest_text.replace("\r\n", "\n")
    if '[workspace.lints.rust]\nunsafe_code = "forbid"\n' not in normalized:
        failures.append("F0 root workspace unsafe_code=forbid boundary changed")
    return failures


def f0_legacy_path_failures(existing_paths):
    if existing_paths:
        return [
            "F0-HARDWARE-OWNERSHIP-MIGRATION: legacy private helper still exists: {}".format(
                ", ".join(sorted(existing_paths))
            )
        ]
    return []


def f0_contract_failures(tracked, root_metadata):
    failures = []
    required = {
        "Cargo.toml": F0_ASSERTION_IDS[0],
        "Cargo.lock": F0_ASSERTION_IDS[1],
        F0_FOUNDATION_MANIFEST: F0_ASSERTION_IDS[0],
        F0_FOUNDATION_SOURCE: F0_ASSERTION_IDS[0],
        F0_FOUNDATION_WINDOWS_TEST: F0_ASSERTION_IDS[3],
        F0_ADMISSION_RECORD: F0_ASSERTION_IDS[1],
        F0_HARDWARE_MANIFEST: F0_ASSERTION_IDS[4],
        "tests/hardware/Cargo.lock": F0_ASSERTION_IDS[4],
        F0_HARDWARE_SOURCE: F0_ASSERTION_IDS[4],
    }
    for relative, assertion_id in required.items():
        if not (ROOT / relative).is_file():
            failures.append("{}: required F0 file is missing: {}".format(assertion_id, relative))
    if failures:
        return failures

    try:
        root_manifest_text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        foundation_manifest_text = (ROOT / F0_FOUNDATION_MANIFEST).read_text(
            encoding="utf-8"
        )
        hardware_manifest_text = (ROOT / F0_HARDWARE_MANIFEST).read_text(
            encoding="utf-8"
        )
        root_lock = parse_cargo_lock_packages(
            (ROOT / "Cargo.lock").read_text(encoding="utf-8")
        )
        hardware_lock = parse_cargo_lock_packages(
            (ROOT / "tests/hardware/Cargo.lock").read_text(encoding="utf-8")
        )
        admission = json.loads((ROOT / F0_ADMISSION_RECORD).read_text(encoding="utf-8"))
        hardware_metadata = cargo_metadata(F0_HARDWARE_MANIFEST)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        return ["F0 dependency or manifest record cannot be parsed: {}".format(error)]

    foundation_packages = [
        package
        for package in root_metadata.get("packages", [])
        if relative_manifest(package) == F0_FOUNDATION_MANIFEST
    ]
    if len(foundation_packages) != 1:
        failures.append(
            "F0-NO-DETACHED-PUBLIC-AUTHORITY: cargo metadata does not contain exactly "
            "one foundation package"
        )
        foundation_package = {}
    else:
        foundation_package = foundation_packages[0]
    hardware_packages = [
        package
        for package in hardware_metadata.get("packages", [])
        if relative_manifest(package) == F0_HARDWARE_MANIFEST
    ]
    if len(hardware_packages) != 1:
        failures.append(
            "F0-HARDWARE-OWNERSHIP-MIGRATION: cargo metadata does not contain exactly "
            "one hardware package"
        )
        hardware_package = {}
    else:
        hardware_package = hardware_packages[0]

    failures.extend(
        f0_root_manifest_failures(root_manifest_text, workspace_members(root_metadata))
    )
    failures.extend(
        f0_foundation_manifest_failures(
            foundation_manifest_text, foundation_package
        )
    )
    failures.extend(
        f0_hardware_manifest_failures(
            hardware_manifest_text,
            hardware_package,
            workspace_members(hardware_metadata),
        )
    )
    failures.extend(f0_lock_failures(root_lock, "root"))
    failures.extend(f0_lock_failures(hardware_lock, "hardware"))
    failures.extend(f0_admission_failures(admission))

    foundation_source = (ROOT / F0_FOUNDATION_SOURCE).read_text(encoding="utf-8")
    windows_test = (ROOT / F0_FOUNDATION_WINDOWS_TEST).read_text(encoding="utf-8")
    hardware_source = (ROOT / F0_HARDWARE_SOURCE).read_text(encoding="utf-8")
    ffi_sources = {
        path: (ROOT / path).read_text(encoding="utf-8")
        for path in tracked
        if (path.startswith("crates/") or path.startswith("tests/"))
        and path.endswith(".rs")
        and (ROOT / path).is_file()
    }
    failures.extend(
        f0_foundation_source_failures(foundation_source, windows_test, ffi_sources)
    )
    failures.extend(f0_hardware_source_failures(hardware_source))
    failures.extend(
        f0_legacy_path_failures(
            {
                path
                for path in (F0_LEGACY_MANIFEST, F0_LEGACY_SOURCE)
                if (ROOT / path).exists()
            }
        )
    )
    return failures


def f0_guard_regression_failures():
    failures = []

    def expect_clean(label, actual):
        if actual:
            failures.append(
                "F0 guard regression baseline failed for {}: {}".format(
                    label, "; ".join(actual)
                )
            )

    def expect_failure(label, actual, assertion_id):
        if not actual or not any(assertion_id in failure for failure in actual):
            failures.append(
                "F0 guard mutation was not rejected by {}: {}".format(
                    assertion_id, label
                )
            )

    try:
        root_manifest = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
        hardware_manifest = (ROOT / F0_HARDWARE_MANIFEST).read_text(
            encoding="utf-8"
        )
        foundation_source = (ROOT / F0_FOUNDATION_SOURCE).read_text(
            encoding="utf-8"
        )
        windows_test = (ROOT / F0_FOUNDATION_WINDOWS_TEST).read_text(
            encoding="utf-8"
        )
        hardware_source = (ROOT / F0_HARDWARE_SOURCE).read_text(encoding="utf-8")
    except OSError as error:
        return ["F0 guard regression input is unavailable: {}".format(error)]

    foundation_dependency = {
        "name": "windows-sys",
        "req": "=0.61.2",
        "kind": None,
        "rename": None,
        "optional": False,
        "uses_default_features": False,
        "features": [
            "Win32_Foundation",
            "Win32_Storage_FileSystem",
        ],
        "target": "cfg(windows)",
        "path": None,
    }
    foundation_package = {
        "name": "easycon-file-identity",
        "license": "GPL-3.0-only",
        "publish": [],
        "rust_version": "1.97.1",
        "dependencies": [copy.deepcopy(foundation_dependency)],
    }
    hardware_dependency = {
        "name": "easycon-file-identity",
        "req": "*",
        "kind": None,
        "rename": "easycon-hardware-file-id",
        "optional": False,
        "uses_default_features": True,
        "features": [],
        "target": "cfg(windows)",
        "path": str((ROOT / "crates/easycon-file-identity").resolve()),
    }
    hardware_package = {
        "dependencies": [copy.deepcopy(hardware_dependency)],
    }

    expect_clean(
        "root manifest",
        f0_root_manifest_failures(root_manifest, set(EXPECTED_MEMBERS)),
    )
    expect_failure(
        "root member missing",
        f0_root_manifest_failures(
            root_manifest, EXPECTED_MEMBERS - {"crates/easycon-file-identity"}
        ),
        F0_ASSERTION_IDS[0],
    )
    expect_failure(
        "root member extra",
        f0_root_manifest_failures(
            root_manifest, EXPECTED_MEMBERS | {"crates/unadmitted-identity"}
        ),
        F0_ASSERTION_IDS[0],
    )
    expect_failure(
        "root unsafe lint weakened",
        f0_root_manifest_failures(
            root_manifest.replace('unsafe_code = "forbid"', 'unsafe_code = "deny"'),
            set(EXPECTED_MEMBERS),
        ),
        "F0 root workspace",
    )

    foundation_manifest = expected_f0_foundation_manifest()
    expect_clean(
        "foundation manifest",
        f0_foundation_manifest_failures(
            foundation_manifest, copy.deepcopy(foundation_package)
        ),
    )
    for field, value in (
        ("name", "easycon-detached-identity"),
        ("license", "MIT"),
        ("publish", None),
        ("rust_version", "1.96.0"),
    ):
        mutated = copy.deepcopy(foundation_package)
        mutated[field] = value
        expect_failure(
            "foundation package {}".format(field),
            f0_foundation_manifest_failures(foundation_manifest, mutated),
            F0_ASSERTION_IDS[0],
        )
    for field, value in (
        ("name", "windows"),
        ("req", "^0.61.2"),
        ("kind", "dev"),
        ("rename", "win32"),
        ("optional", True),
        ("uses_default_features", True),
        ("target", None),
        ("path", str((ROOT / "tests").resolve())),
    ):
        mutated = copy.deepcopy(foundation_package)
        mutated["dependencies"][0][field] = value
        expect_failure(
            "foundation dependency {}".format(field),
            f0_foundation_manifest_failures(foundation_manifest, mutated),
            F0_ASSERTION_IDS[0],
        )
    for feature in foundation_dependency["features"]:
        mutated = copy.deepcopy(foundation_package)
        mutated["dependencies"][0]["features"].remove(feature)
        expect_failure(
            "foundation dependency missing {}".format(feature),
            f0_foundation_manifest_failures(foundation_manifest, mutated),
            F0_ASSERTION_IDS[0],
        )
    mutated = copy.deepcopy(foundation_package)
    mutated["dependencies"][0]["features"].append("Win32_System_IO")
    expect_failure(
        "foundation dependency extra feature",
        f0_foundation_manifest_failures(foundation_manifest, mutated),
        F0_ASSERTION_IDS[0],
    )
    mutated = copy.deepcopy(foundation_package)
    mutated["dependencies"].append(
        {
            "name": "serde",
            "req": "*",
            "kind": None,
            "rename": None,
            "optional": False,
            "uses_default_features": True,
            "features": [],
            "target": None,
            "path": None,
        }
    )
    expect_failure(
        "foundation extra dependency",
        f0_foundation_manifest_failures(foundation_manifest, mutated),
        F0_ASSERTION_IDS[0],
    )
    expect_failure(
        "foundation manifest text drift",
        f0_foundation_manifest_failures(
            foundation_manifest.replace("publish = false", "publish = true"),
            copy.deepcopy(foundation_package),
        ),
        F0_ASSERTION_IDS[0],
    )

    expect_clean(
        "hardware manifest",
        f0_hardware_manifest_failures(
            hardware_manifest,
            copy.deepcopy(hardware_package),
            {"tests/hardware"},
        ),
    )
    for field, value in (
        ("name", "easycon-hardware-file-id"),
        ("rename", None),
        ("path", str((ROOT / "tests/hardware/file-id-handle").resolve())),
        ("target", None),
        ("kind", "dev"),
        ("optional", True),
        ("uses_default_features", False),
    ):
        mutated = copy.deepcopy(hardware_package)
        mutated["dependencies"][0][field] = value
        expect_failure(
            "hardware alias {}".format(field),
            f0_hardware_manifest_failures(
                hardware_manifest, mutated, {"tests/hardware"}
            ),
            F0_ASSERTION_IDS[4],
        )
    expect_failure(
        "hardware member missing",
        f0_hardware_manifest_failures(
            hardware_manifest, copy.deepcopy(hardware_package), set()
        ),
        F0_ASSERTION_IDS[4],
    )
    expect_failure(
        "hardware old helper member",
        f0_hardware_manifest_failures(
            hardware_manifest,
            copy.deepcopy(hardware_package),
            {"tests/hardware", "tests/hardware/file-id-handle"},
        ),
        F0_ASSERTION_IDS[4],
    )
    expect_failure(
        "hardware old helper path",
        f0_hardware_manifest_failures(
            hardware_manifest.replace(
                "../../crates/easycon-file-identity", "file-id-handle"
            ),
            copy.deepcopy(hardware_package),
            {"tests/hardware"},
        ),
        F0_ASSERTION_IDS[4],
    )
    for legacy_path in (F0_LEGACY_MANIFEST, F0_LEGACY_SOURCE):
        expect_failure(
            "legacy helper {}".format(legacy_path),
            f0_legacy_path_failures({legacy_path}),
            F0_ASSERTION_IDS[4],
        )

    lock_fixture = {
        "package": [
            {
                "name": "easycon-file-identity",
                "version": "0.1.0",
                "dependencies": ["windows-sys"],
            },
            copy.deepcopy(F0_EXPECTED_REGISTRY_PACKAGES["windows-link"]),
            copy.deepcopy(F0_EXPECTED_REGISTRY_PACKAGES["windows-sys"]),
        ]
    }

    def lock_package(lock, name):
        return next(package for package in lock["package"] if package["name"] == name)

    for lock_label in ("root", "hardware"):
        expect_clean(
            "{} lock".format(lock_label),
            f0_lock_failures(copy.deepcopy(lock_fixture), lock_label),
        )
        mutated = copy.deepcopy(lock_fixture)
        lock_package(mutated, "easycon-file-identity")["version"] = "0.1.1"
        expect_failure(
            "{} foundation lock version".format(lock_label),
            f0_lock_failures(mutated, lock_label),
            F0_ASSERTION_IDS[4],
        )
        mutated = copy.deepcopy(lock_fixture)
        lock_package(mutated, "easycon-file-identity")["dependencies"] = []
        expect_failure(
            "{} foundation lock dependency edge".format(lock_label),
            f0_lock_failures(mutated, lock_label),
            F0_ASSERTION_IDS[4],
        )
        for package_name in ("windows-link", "windows-sys"):
            for field in ("version", "source", "checksum"):
                mutated = copy.deepcopy(lock_fixture)
                lock_package(mutated, package_name)[field] += "-mutated"
                expect_failure(
                    "{} lock {} {}".format(lock_label, package_name, field),
                    f0_lock_failures(mutated, lock_label),
                    F0_ASSERTION_IDS[1],
                )
            mutated = copy.deepcopy(lock_fixture)
            package = lock_package(mutated, package_name)
            if package["dependencies"]:
                package["dependencies"].pop()
            else:
                package["dependencies"].append("unexpected-edge")
            expect_failure(
                "{} lock {} dependency edge".format(lock_label, package_name),
                f0_lock_failures(mutated, lock_label),
                F0_ASSERTION_IDS[1],
            )
            mutated = copy.deepcopy(lock_fixture)
            mutated["package"] = [
                package
                for package in mutated["package"]
                if package["name"] != package_name
            ]
            expect_failure(
                "{} lock missing {}".format(lock_label, package_name),
                f0_lock_failures(mutated, lock_label),
                F0_ASSERTION_IDS[1],
            )
            mutated = copy.deepcopy(lock_fixture)
            extra = copy.deepcopy(lock_package(mutated, package_name))
            extra["version"] = "0.0.0"
            mutated["package"].append(extra)
            expect_failure(
                "{} lock extra {} version".format(lock_label, package_name),
                f0_lock_failures(mutated, lock_label),
                F0_ASSERTION_IDS[1],
            )

    admission = expected_f0_admission()
    expect_clean("dependency admission", f0_admission_failures(copy.deepcopy(admission)))

    def scalar_paths(value, path=()):
        if isinstance(value, dict):
            for key in sorted(value):
                for item in scalar_paths(value[key], path + (key,)):
                    yield item
        elif isinstance(value, list):
            for index, item in enumerate(value):
                for nested in scalar_paths(item, path + (index,)):
                    yield nested
        else:
            yield path

    def mutate_scalar(document, path):
        mutated = copy.deepcopy(document)
        cursor = mutated
        for component in path[:-1]:
            cursor = cursor[component]
        original = cursor[path[-1]]
        if original is None:
            replacement = "unexpected"
        elif isinstance(original, bool):
            replacement = not original
        elif isinstance(original, int):
            replacement = original + 1
        else:
            replacement = "{}-mutated".format(original)
        cursor[path[-1]] = replacement
        return mutated

    for path in scalar_paths(admission):
        label = ".".join(str(component) for component in path)
        expect_failure(
            "dependency admission scalar {}".format(label),
            f0_admission_failures(mutate_scalar(admission, path)),
            F0_ASSERTION_IDS[1],
        )
    for index in range(len(admission["packages"])):
        mutated = copy.deepcopy(admission)
        del mutated["packages"][index]
        expect_failure(
            "dependency admission missing package {}".format(index),
            f0_admission_failures(mutated),
            F0_ASSERTION_IDS[1],
        )
    mutated = copy.deepcopy(admission)
    extra_package = copy.deepcopy(mutated["packages"][0])
    extra_package["name"] = "unexpected-package"
    mutated["packages"].append(extra_package)
    expect_failure(
        "dependency admission extra package",
        f0_admission_failures(mutated),
        F0_ASSERTION_IDS[1],
    )
    for field in admission:
        mutated = copy.deepcopy(admission)
        del mutated[field]
        expect_failure(
            "dependency admission missing top-level {}".format(field),
            f0_admission_failures(mutated),
            F0_ASSERTION_IDS[1],
        )
    mutated = copy.deepcopy(admission)
    mutated["unexpected"] = True
    expect_failure(
        "dependency admission extra field",
        f0_admission_failures(mutated),
        F0_ASSERTION_IDS[1],
    )

    ffi_sources = {
        F0_FOUNDATION_SOURCE: foundation_source,
        F0_FOUNDATION_WINDOWS_TEST: windows_test,
        F0_HARDWARE_SOURCE: hardware_source,
    }

    def foundation_failures(mutated_source=None, mutated_test=None, mutated_ffi=None):
        return f0_foundation_source_failures(
            foundation_source if mutated_source is None else mutated_source,
            windows_test if mutated_test is None else mutated_test,
            ffi_sources if mutated_ffi is None else mutated_ffi,
        )

    def insert_production(addition):
        return foundation_source.replace(
            "#[cfg(test)]", addition + "\n#[cfg(test)]", 1
        )

    expect_clean("foundation source", foundation_failures())
    for label, addition in (
        ("public identity struct", "\npub struct ExposedIdentity { pub volume: u64 }\n"),
        ("public identity tuple", "\npub type ExposedIdentity = (u64, u128);\n"),
        ("public identity value", "\npub const EXPOSED_IDENTITY: (u64, u128) = (1, 1);\n"),
        (
            "public identity function",
            "\npub fn exposed_identity(_: &File) -> io::Result<(u64, u128)> { todo!() }\n",
        ),
    ):
        expect_failure(
            label,
            foundation_failures(mutated_source=insert_production(addition)),
            F0_ASSERTION_IDS[0],
        )
    expect_failure(
        "public identity field",
        foundation_failures(
            mutated_source=foundation_source.replace(
                "    volume_serial_number: u64,",
                "    pub volume_serial_number: u64,",
                1,
            )
        ),
        F0_ASSERTION_IDS[0],
    )
    for delegation, replacement in (
        (
            "validate_file_object_with(file, &mut SystemFileIdentityQuery)",
            "Ok(())",
        ),
        (
            "same_file_object_with(left, right, &mut SystemFileIdentityQuery)",
            "Ok(false)",
        ),
    ):
        expect_failure(
            "public safe API delegation {}".format(delegation),
            foundation_failures(
                mutated_source=foundation_source.replace(delegation, replacement, 1)
            ),
            F0_ASSERTION_IDS[0],
        )
    for label, addition in (
        ("second unsafe allow", "\n#[allow(unsafe_code)]\n"),
        ("second unsafe block", "\nfn extra_unsafe() { unsafe {} }\n"),
        (
            "second native query call",
            "\nfn extra_query() { GetFileInformationByHandleEx(\n",
        ),
    ):
        expect_failure(
            label,
            foundation_failures(mutated_source=insert_production(addition)),
            F0_ASSERTION_IDS[1],
        )
    for safety_clause in (
        "SAFETY:",
        "borrowed from a live `File` for this one call",
        "selects the exact `FILE_ID_INFO` layout",
        "buffer of exactly `buffer_size` bytes",
        "retains neither the handle nor pointer",
    ):
        expect_failure(
            "missing SAFETY proof {}".format(safety_clause),
            foundation_failures(
                mutated_source=foundation_source.replace(safety_clause, "")
            ),
            F0_ASSERTION_IDS[1],
        )
    for quality_clause in (
        'identifier.ok_or_else(|| invalid_identity("FILE_ID_INFO result is incomplete"))?',
        "if volume_serial_number == 0 {",
        "if identifier_is_all_zero(&identifier) {",
        "identifier.iter().all(|byte| *byte == 0)",
        "if succeeded == 0 {",
        "return Err(io::Error::last_os_error());",
        "io::ErrorKind::Unsupported",
        "low_half_only",
        "comparison_uses_volume_and_all_128_identifier_bits",
    ):
        expect_failure(
            "quality clause {}".format(quality_clause),
            foundation_failures(
                mutated_source=foundation_source.replace(quality_clause, "")
            ),
            F0_ASSERTION_IDS[1],
        )
    expect_failure(
        "comparison truncates high identity bits",
        foundation_failures(
            mutated_source=foundation_source.replace(
                "left == right",
                "left.file_id as u64 == right.file_id as u64",
                1,
            )
        ),
        F0_ASSERTION_IDS[0],
    )
    expect_failure(
        "same-reference query shortcut",
        foundation_failures(
            mutated_source=foundation_source.replace(
                "    let left_identity = validated_identity_with(left, query)?;",
                "    if std::ptr::eq(left, right) { return Ok(true); }\n"
                "    let left_identity = validated_identity_with(left, query)?;",
                1,
            )
        ),
        F0_ASSERTION_IDS[0],
    )
    for assertion_id in F0_ASSERTION_IDS[:3]:
        expect_failure(
            "foundation stable ID removed {}".format(assertion_id),
            foundation_failures(
                mutated_source=foundation_source.replace(assertion_id, "")
            ),
            assertion_id,
        )
    expect_failure(
        "foundation assertion ignored",
        foundation_failures(mutated_source=foundation_source + "\n#[ignore]\n"),
        "F0 stable foundation assertions",
    )
    for trace_clause in (
        "left-query",
        "right-query",
        "compare",
        "return",
        "drop-current",
        "drop-retained",
        "assert_two_live_handle_trace(true, true)",
        "assert_two_live_handle_trace(false, false)",
        "same_reference_is_queried_twice_before_comparison",
    ):
        expect_failure(
            "two-live trace {}".format(trace_clause),
            foundation_failures(
                mutated_source=foundation_source.replace(trace_clause, "")
            ),
            F0_ASSERTION_IDS[2],
        )
    duplicated_ffi = dict(ffi_sources)
    duplicated_ffi["tests/hardware/src/duplicate_file_id.rs"] = "FILE_ID_INFO"
    expect_failure(
        "second FFI authority",
        foundation_failures(mutated_ffi=duplicated_ffi),
        F0_ASSERTION_IDS[1],
    )

    for test_clause in (
        F0_ASSERTION_IDS[3],
        "volume_or_share_root",
        "ordinary_directory",
        "regular_file",
        "root_retained",
        "directory_retained",
        "file_retained",
        "FILE_FLAG_OPEN_REPARSE_POINT",
        'File::open("NUL")',
        "same_file_object(retained, current)",
        "same_file_object(retained, distinct)",
        "same_file_object(retained, unsupported).is_err()",
    ):
        expect_failure(
            "root/directory/file clause {}".format(test_clause),
            foundation_failures(mutated_test=windows_test.replace(test_clause, "")),
            F0_ASSERTION_IDS[3],
        )
    expect_failure(
        "root/directory/file assertion ignored",
        foundation_failures(mutated_test=windows_test + "\n#[ignore]\n"),
        F0_ASSERTION_IDS[3],
    )

    expect_clean("hardware source", f0_hardware_source_failures(hardware_source))
    for label, mutation in (
        (
            "hardware detached field",
            hardware_source + "\nidentity: (u64, u128),\n",
        ),
        (
            "hardware detached type",
            hardware_source + "\nstruct HighResolutionFileIdentity;\n",
        ),
        (
            "hardware path provider",
            hardware_source.replace(
                "fn same_file_object(&self, left: &File, right: &File)",
                "fn same_file_object(&self, left: &Path, right: &Path)",
                1,
            ),
        ),
        (
            "hardware tuple provider",
            hardware_source.replace(
                "fn same_file_object(&self, left: &File, right: &File)",
                "fn same_file_object(&self, left: (u64, u128), right: (u64, u128))",
                1,
            ),
        ),
        (
            "GuardedDiskFile retained File removed",
            hardware_source.replace(
                "struct GuardedDiskFile {\n    bytes: Vec<u8>,\n    file: File,\n}",
                "struct GuardedDiskFile {\n    bytes: Vec<u8>,\n}",
                1,
            ),
        ),
    ):
        expect_failure(
            label,
            f0_hardware_source_failures(mutation),
            F0_ASSERTION_IDS[4],
        )
    for hardware_clause in (
        F0_ASSERTION_IDS[4],
        "validate_guarded_file_object(&file)",
        "easycon_hardware_file_id::validate_file_object(file)",
        'same_guarded_file_object(&first, &second, "checkpoint input")',
        '"manifest staging/final"',
        '"completion staging/final"',
        '"manifest same_file_as"',
        ".same_file_object(&left.file, &right.file)",
        ".same_file_object(&staging.file, &guard)",
    ):
        expect_failure(
            "hardware shared comparison {}".format(hardware_clause),
            f0_hardware_source_failures(
                hardware_source.replace(hardware_clause, "")
            ),
            F0_ASSERTION_IDS[4],
        )
    removed_shared = hardware_source.replace(
        "easycon_hardware_file_id::same_file_object", "removed_shared_comparison", 1
    )
    expect_failure(
        "hardware shared API delegation removed",
        f0_hardware_source_failures(removed_shared),
        F0_ASSERTION_IDS[4],
    )
    expect_failure(
        "hardware assertion ignored",
        f0_hardware_source_failures(hardware_source + "\n#[ignore]\n"),
        F0_ASSERTION_IDS[4],
    )
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
    failures.extend(f0_guard_regression_failures())
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
    if type(configuration["version"]) is not int or configuration["version"] != 5:
        raise ValueError("Windows build environment version must be the JSON integer 5")
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
    expected_fingerprint_paths = expected_windows_environment_fingerprint_paths()
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


def parse_windows_gate_policy(text):
    try:
        policy = json.loads(text, object_pairs_hook=_unique_json_object)
    except (json.JSONDecodeError, ValueError) as error:
        raise ValueError("invalid JSON: {}".format(error)) from error

    _require_json_keys(policy, WINDOWS_GATE_POLICY_KEYS, "Windows gate policy root")
    if type(policy["version"]) is not int or policy["version"] != 1:
        raise ValueError("Windows gate policy version must be the JSON integer 1")
    if type(policy["cargoJobs"]) is not int or policy["cargoJobs"] != 4:
        raise ValueError("Windows gate policy cargoJobs must be the JSON integer 4")
    gates = policy["gates"]
    if type(gates) is not list:
        raise ValueError("Windows gate policy gates must be a JSON array")

    expected = expected_windows_gate_policy_gates()
    if len(gates) != len(expected):
        raise ValueError("Windows gate policy gate set or order changed")
    identities = []
    for index, gate in enumerate(gates):
        _require_json_keys(
            gate, WINDOWS_GATE_POLICY_GATE_KEYS, "Windows gate policy gate {}".format(index)
        )
        name = _require_string(gate["name"], "Windows gate policy gate name")
        tool = _require_string(gate["tool"], "Windows gate policy gate tool")
        arguments = gate["arguments"]
        if type(arguments) is not list or any(type(value) is not str for value in arguments):
            raise ValueError("Windows gate policy gate arguments must be a JSON string array")
        identities.append(name.casefold())
        expected_name, expected_tool, expected_arguments = expected[index]
        if (
            name != expected_name
            or tool != expected_tool
            or arguments != expected_arguments
        ):
            raise ValueError("Windows gate policy gate set, path, case, or order changed")
    if len(identities) != len(set(identities)):
        raise ValueError("Windows gate policy contains duplicate Windows gate identity")
    return policy


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


def _powershell_function_text(module_text, name):
    marker = re.search(
        r"(?m)^function\s+{}\s*\{{".format(re.escape(name)), module_text
    )
    if marker is None:
        return None
    following = re.search(r"(?m)^function\s+[A-Za-z0-9-]+\s*\{", module_text[marker.end() :])
    end = len(module_text) if following is None else marker.end() + following.start()
    return module_text[marker.start() : end]


def windows_workspace_module_failures(module_text):
    failures = []
    layout = _powershell_function_text(
        module_text, "New-EasyConVcpkgWorkspaceLayout"
    )
    publisher = _powershell_function_text(
        module_text, "Publish-EasyConContentFileAtomically"
    )
    copier = _powershell_function_text(
        module_text, "Copy-EasyConContentFileAtomically"
    )
    writer = _powershell_function_text(module_text, "Write-EasyConUtf8FileAtomically")
    prepared_tree = _powershell_function_text(
        module_text, "Get-EasyConPreparedTreeVerification"
    )
    prepared_tree_assert = _powershell_function_text(
        module_text, "Assert-EasyConPreparedTreeDoesNotReferenceRepository"
    )
    prepared_physical_tree = _powershell_function_text(
        module_text, "Assert-EasyConPreparedPhysicalTree"
    )
    prepared_tree_auditor = _powershell_function_text(
        module_text, "Initialize-EasyConPreparedTreeAuditor"
    )
    vcpkg_checkout = _powershell_function_text(
        module_text, "Assert-EasyConVcpkgCheckout"
    )
    fingerprint = _powershell_function_text(module_text, "Get-EasyConTreeFingerprint")
    verify_core = _powershell_function_text(
        module_text, "Invoke-EasyConWindowsVerifyCore"
    )
    setup_install = _powershell_function_text(
        module_text, "Install-EasyConWindowsEnvironment"
    )
    for name, body in (
        ("workspace vcpkg layout", layout),
        ("atomic file publisher", publisher),
        ("atomic content copier", copier),
        ("atomic UTF-8 writer", writer),
        ("prepared tree verification", prepared_tree),
        ("prepared tree assertion", prepared_tree_assert),
        ("prepared physical tree assertion", prepared_physical_tree),
        ("prepared tree auditor", prepared_tree_auditor),
        ("vcpkg checkout assertion", vcpkg_checkout),
        ("prepared tree fingerprint", fingerprint),
        ("Windows Verify core", verify_core),
        ("Windows Setup install", setup_install),
    ):
        if body is None:
            failures.append("Windows workspace module is missing {}".format(name))
    if failures:
        return failures

    root_binding = layout.find('"set(Z_VCPKG_ROOT_DIR')
    prepared_include = layout.find('"include(`"')
    if (
        root_binding < 0
        or prepared_include < 0
        or root_binding >= prepared_include
        or "CACHE INTERNAL" not in layout[root_binding:prepared_include]
    ):
        failures.append(
            "workspace vcpkg wrapper must bind Z_VCPKG_ROOT_DIR as CACHE INTERNAL "
            "before including prepared scripts"
        )
    if layout.count("-ReplaceExisting") != 3:
        failures.append(
            "workspace vcpkg tool, marker, and manifest must each use fresh replacement"
        )
    if "Write-EasyConUtf8FileAtomically -Path $wrapper" not in layout:
        failures.append("workspace vcpkg wrapper must use atomic UTF-8 publication")
    if "Copy-Item -Force" in layout or "WriteAllText($wrapper" in layout:
        failures.append(
            "workspace vcpkg final files must not be opened or overwritten in place"
        )
    for exact_shape in (
        'Allowed = @(".vcpkg-root", "scripts", "vcpkg.exe")',
        'Expected = @(".vcpkg-root", "scripts", "vcpkg.exe")',
        'Allowed = @("buildsystems")',
        'Expected = @("buildsystems")',
    ):
        if exact_shape not in layout:
            failures.append(
                "workspace vcpkg applocal root exact-shape guard is missing: {}".format(
                    exact_shape
                )
            )
    if "[System.IO.File]::Move($temporary, $destinationPath, $true)" not in publisher:
        failures.append(
            "workspace atomic file publication must replace the final with one overwrite move"
        )
    if "[switch]$ReplaceExisting" not in copier or "-not $ReplaceExisting" not in copier:
        failures.append(
            "atomic content copy must expose a private fresh-replacement mode"
        )
    if "[System.IO.FileMode]::CreateNew" not in copier:
        failures.append("fresh content copies must create a unique new temporary file")
    if "[System.IO.FileMode]::CreateNew" not in writer:
        failures.append("generated UTF-8 files must create a unique new temporary file")
    for forbidden in ("VCPKG_APPLOCAL_DEPS", "X_VCPKG_APPLOCAL_DEPS_INSTALL"):
        if forbidden in module_text:
            failures.append(
                "Windows workspace module must not disable or switch applocal mode: {}".format(
                    forbidden
                )
            )

    if "PreparedTreeAuditor]::AuditTree(" not in prepared_tree:
        failures.append(
            "prepared tree verification must delegate to the unified C# tree auditor"
        )
    if "Get-EasyConPreparedTreeVerification -Path $Path" not in prepared_tree_assert:
        failures.append(
            "prepared tree assertion must delegate to the unified tree verification"
        )
    if "PreparedTreeAuditor]::ValidatePhysicalTree(" not in prepared_physical_tree:
        failures.append(
            "prepared physical tree assertion must use the controlled C# traversal"
        )
    if "PreparedTreeAuditor]::FingerprintTree(" not in fingerprint:
        failures.append(
            "prepared tree fingerprint must use the controlled C# tree scanner"
        )
    for required in (
        "PreparedTreeAuditor]::BindVcpkgCheckout(",
        "$binding.AssertCurrent()",
        "PhysicalEntriesBound",
        "$binding.Dispose()",
        "ControlledEnumerationPasses",
    ):
        if required not in vcpkg_checkout:
            failures.append(
                "vcpkg checkout assertion is missing its full-tree physical binding: {}".format(
                    required
                )
            )
    binding_start = vcpkg_checkout.find("PreparedTreeAuditor]::BindVcpkgCheckout(")
    binding_current = vcpkg_checkout.find("$binding.AssertCurrent()")
    first_git = vcpkg_checkout.find('"vcpkg scripts commit check"')
    final_git = vcpkg_checkout.find('"vcpkg scripts cleanliness check"')
    binding_dispose = vcpkg_checkout.find("$binding.Dispose()")
    if not (
        0 <= binding_start < binding_current < first_git < final_git < binding_dispose
    ):
        failures.append(
            "vcpkg full-tree binding must cover every Git validation from audit through status"
        )
    for required in (
        "Directory.EnumerateFileSystemEntries",
        "File.GetAttributes",
        "FileOptions.SequentialScan",
        "IncrementalHash.CreateHash",
        "StringComparer.OrdinalIgnoreCase",
        "ValidatePhysicalTree",
        "BindVcpkgCheckout",
        "ScanAndBindPhysicalDirectory",
        "CreateFile",
        "FileReadData",
        "FileListDirectory",
        "FileShareRead",
        "GetFileInformationByHandleEx",
        "GetFinalPathNameByHandle",
        "GetFileInformationByHandle",
        "GetBoundBasicInformation",
        "PhysicalReadDataLockCalls",
        "PhysicalBasicInformationQueries",
        "PhysicalIdentityQueries",
        "PhysicalFinalPathQueries",
        "GetWin32ExtendedPath",
        "GetLogicalPathInput",
        "GetBoundLogicalFinalPath",
        "ComparePowerShellFullName",
        "vcpkg checkout binding path",
        "current.Handle.Dispose()",
    ):
        if required not in prepared_tree_auditor:
            failures.append(
                "prepared tree auditor is missing its single-scan primitive: {}".format(
                    required
                )
            )

    def csharp_region(start_marker, end_marker, description):
        start = prepared_tree_auditor.find(start_marker)
        end = prepared_tree_auditor.find(end_marker, start + len(start_marker))
        if start < 0 or end < 0:
            failures.append(
                "prepared tree auditor is missing its {} region".format(description)
            )
            return ""
        return prepared_tree_auditor[start:end]

    audit_tree = csharp_region(
        "public static PreparedTreeAuditResult AuditTree(",
        "private static void ScanPhysicalDirectory(",
        "content audit",
    )
    physical_scan = csharp_region(
        "private static void ScanPhysicalDirectory(",
        "private static void ScanAndBindPhysicalDirectory(",
        "physical-only scan",
    )
    binding_scan = csharp_region(
        "private static void ScanAndBindPhysicalDirectory(",
        "private static void AssertVcpkgCriticalEntriesBound(",
        "vcpkg binding scan",
    )
    open_bound_entry = csharp_region(
        "private static PreparedVcpkgBoundPhysicalEntry OpenBoundPhysicalEntry(",
        "private static string GetBoundLogicalFinalPath(",
        "bound entry open",
    )
    basic_query = csharp_region(
        "private static FileBasicInformation GetBoundBasicInformation(",
        "private static void AssertBoundPhysicalEntryState(",
        "bound basic information query",
    )
    content_scan = csharp_region(
        "private static bool ScanDirectory(",
        "private static FileAuditResult ScanFile(",
        "content scan",
    )
    ordinal_entry_sort = "entries.Sort(StringComparer.OrdinalIgnoreCase);"
    if content_scan and ordinal_entry_sort in content_scan:
        failures.append(
            "prepared content scan must not pre-sort entries before the legacy digest sort"
        )
    if audit_tree and audit_tree.count(
        "files.Sort(TreeFileRecordComparer.Instance);"
    ) != 1:
        failures.append(
            "prepared content digest must perform exactly one legacy-compatible final sort"
        )
    for description, region in (
        ("physical-only scan", physical_scan),
        ("vcpkg binding scan", binding_scan),
    ):
        if region and region.count(ordinal_entry_sort) != 1:
            failures.append(
                "prepared {} must retain exactly one deterministic ordinal entry sort".format(
                    description
                )
            )
    if binding_scan:
        if "File.GetAttributes(" in binding_scan:
            failures.append(
                "vcpkg binding scan must classify each entry only from its bound handle"
            )
        for required in (
            "OpenBoundPhysicalEntry(\n                    entry,\n                    null,",
            "if (bound.IsDirectory)",
        ):
            if required not in binding_scan:
                failures.append(
                    "vcpkg binding scan is missing handle-first classification: {}".format(
                        required
                    )
                )
    if open_bound_entry:
        for required in (
            "bool? expectedDirectory",
            "uint flags = FileFlagOpenReparsePoint | FileFlagBackupSemantics;",
            "uint desiredAccess = FileReadData | FileListDirectory;",
            "FileBasicInformation basicInformation = GetBoundBasicInformation(",
            "IsDirectory = isDirectory,",
        ):
            if required not in open_bound_entry:
                failures.append(
                    "bound entry open is missing single-query type binding: {}".format(
                        required
                    )
                )
        if open_bound_entry.count("GetBoundBasicInformation(") != 1:
            failures.append(
                "bound entry open must issue exactly one basic information query"
            )
    if basic_query and (
        "result.PhysicalBasicInformationQueries++;\n"
        "            if (!GetFileInformationByHandleEx(" not in basic_query
    ):
        failures.append(
            "vcpkg basic-query metric must increment immediately before the real OS query"
        )
    for required in (
        "CreateFile(\n                GetWin32ExtendedPath(logicalPath),",
        'return @"\\\\?\\UNC\\" + logicalPath.Substring(2);',
        "return NormalizeFullPath(buffer.ToString());",
    ):
        if required not in prepared_tree_auditor:
            failures.append(
                "prepared tree auditor is missing extended Win32 path normalization: {}".format(
                    required
                )
            )
    if "NoDesiredAccess" in prepared_tree_auditor:
        failures.append(
            "prepared vcpkg binding must not use a zero-access handle as its delete/rename lock"
        )
    if (
        "uint desiredAccess = FileReadData | FileListDirectory;"
        not in prepared_tree_auditor
        or "GetWin32ExtendedPath(logicalPath),\n                desiredAccess,\n                FileShareRead,"
        not in prepared_tree_auditor
    ):
        failures.append(
            "prepared vcpkg binding must retain each audited entry with its minimal read-data/list read-share lock"
        )
    if "Get-ChildItem" in prepared_tree or "Assert-EasyConPhysicalPath" in prepared_tree:
        failures.append(
            "prepared tree verification must not reintroduce PowerShell per-entry traversal"
        )

    verify_calls = re.findall(
        r"Get-EasyConPreparedTreeVerification\s+-Path\s+"
        r"\$preparedPaths\.(cargoVendor|vcpkgInstalled)",
        verify_core,
    )
    if sorted(verify_calls) != ["cargoVendor", "vcpkgInstalled"]:
        failures.append(
            "Windows Verify must scan each prepared Cargo/native tree exactly once"
        )
    if verify_core.count("Assert-EasyConPreparedPhysicalTree") != 3:
        failures.append(
            "Windows Verify must reserve the physical-only scanner for shared cache, Rust home, and OCR"
        )
    if "vcpkgScriptsAudit" in verify_core:
        failures.append(
            "Windows Verify must not leave a pre-Git vcpkg physical audit window"
        )
    if not re.search(
        r"Assert-EasyConVcpkgCheckout\s+-VcpkgRoot "
        r"\$preparedPaths\.vcpkgScriptsRoot\s+`\s*\r?\n\s*"
        r"-Configuration \$configuration -VcpkgExecutable \$tools\.vcpkg\s+`\s*\r?\n\s*"
        r"-TrustedRoot \$location\.EnvironmentRoot",
        verify_core,
    ):
        failures.append(
            "Windows Verify must create the vcpkg full-tree binding at the prepared root"
        )
    for legacy in (
        "Get-EasyConTreeFingerprint -Path $preparedPaths.",
        "Assert-EasyConPreparedTreeDoesNotReferenceRepository -Path $preparedPaths.",
    ):
        if legacy in verify_core:
            failures.append(
                "Windows Verify must not reintroduce a duplicate prepared-tree scan: {}".format(
                    legacy
                )
            )
    for name in ("cargoVendor", "vcpkgInstalled"):
        if re.search(
            r"Assert-EasyConPhysicalTree\s+`\s*\r?\n\s*"
            r"-Path \(\[string\]\$stamp\.paths\.{}\)".format(name),
            verify_core,
        ):
            failures.append(
                "Windows Verify must leave {} physical-tree traversal to the unified scanner".format(
                    name
                )
            )

    setup_calls = re.findall(
        r"Get-EasyConPreparedTreeVerification\s+-Path\s+"
        r"(\$cargoSources\.VendorRoot|\$vcpkgLayout\.Installed)",
        setup_install,
    )
    if sorted(setup_calls) != ["$cargoSources.VendorRoot", "$vcpkgLayout.Installed"]:
        failures.append(
            "Windows Setup must produce each prepared tree digest and audit from one scan"
        )
    return failures


def windows_gate_policy_failures(environment_module, policy_module, runner):
    failures = []
    public_workspace = _powershell_function_text(
        policy_module, "Invoke-EasyConWindowsWorkspace"
    )
    targeted = _powershell_function_text(
        policy_module, "Get-EasyConTargetedCargoArguments"
    )
    policy_parser = _powershell_function_text(
        policy_module, "Get-EasyConWindowsGatePolicy"
    )
    policy_hash = _powershell_function_text(policy_module, "Get-EasyConGatePolicyHash")
    evidence_writer = _powershell_function_text(
        policy_module, "Publish-EasyConWorkspaceEvidenceNoReplace"
    )
    evidence_record = _powershell_function_text(
        policy_module, "New-EasyConWorkspaceEvidenceRecord"
    )
    gate_runner = _powershell_function_text(
        policy_module, "Invoke-EasyConWindowsWorkspaceGates"
    )
    for name, body in (
        ("public Workspace policy", public_workspace),
        ("Targeted Cargo policy", targeted),
        ("strict gate policy parser", policy_parser),
        ("gate policy hash", policy_hash),
        ("no-replace evidence publisher", evidence_writer),
        ("v2 evidence record", evidence_record),
        ("Workspace gate runner", gate_runner),
    ):
        if body is None:
            failures.append("Windows gate policy is missing {}".format(name))
    if failures:
        return failures

    if "function Invoke-EasyConWindowsWorkspace" in environment_module:
        failures.append("environment module must not retain the public Workspace gate policy")
    if "windows_gate_policy.ps1" not in environment_module:
        failures.append("environment module must dot-source the gate policy module")
    if "GateInvoker" in public_workspace:
        failures.append("public Workspace must not expose a GateInvoker bypass")
    if "IgnoreCase = $false" not in public_workspace or "-cnotin" not in public_workspace:
        failures.append("public Workspace GateMode must be exact-case fail closed")
    if (
        "$assertGatePolicyContextCurrent = ${function:Assert-EasyConGatePolicyContextCurrent}"
        not in public_workspace
        or public_workspace.count("& $assertGatePolicyContextCurrent") != 4
    ):
        failures.append(
            "Workspace must recheck the policy hash before and after each gate mode"
        )
    post_policy = public_workspace.find('"after the final gate"')
    candidate_recheck = public_workspace.find(
        "& $assertWorkspaceCandidateBindingCurrent"
    )
    publisher = public_workspace.find("& $publishWorkspaceEvidence", candidate_recheck)
    if (
        "$assertWorkspaceCandidateBindingCurrent = "
        "${function:Assert-EasyConWorkspaceCandidateBindingCurrent}"
        not in public_workspace
        or not (0 <= post_policy < candidate_recheck < publisher)
    ):
        failures.append(
            "Workspace publication order must be post-policy check, candidate recheck, publish, then record"
        )
    for required in (
        '"tools/windows_gate_policy.json"',
        '"tools/windows_gate_policy.ps1"',
        '"tools/run_windows_workspace.ps1"',
        "Get-EasyConFingerprintInputHash",
    ):
        if required not in policy_hash:
            failures.append("gate policy hash misses required input: {}".format(required))
    for required in (
        "Get-EasyConGatePolicyStrictObject",
        "Get-EasyConGatePolicyStrictString",
        "TryGetInt32",
        "cargoJobs must be the JSON integer 4",
        "gate set, path, case, or order changed",
        "OrdinalIgnoreCase",
    ):
        if required not in policy_parser:
            failures.append("strict gate policy parser is missing: {}".format(required))
    if "--jobs" not in targeted or "CargoJobs" not in targeted:
        failures.append("Targeted Cargo must inject the fixed Cargo jobs budget")
    for rejected in (
        '"--jobs"',
        'StartsWith("--jobs=")',
        '"-j"',
        'StartsWith("-j")',
    ):
        if rejected not in targeted:
            failures.append("Targeted Cargo must reject jobs override form: {}".format(rejected))
    if "if (-not $cargoOptionSection)" not in targeted:
        failures.append("Targeted Cargo must leave test-binary arguments after -- untouched")
    for required in (
        "[System.IO.FileMode]::CreateNew",
        "[System.IO.File]::Move($temporary, $destination)",
        "already exists and will not be replaced",
        "Complete-EasyConTemporaryFileCleanup",
    ):
        if required not in evidence_writer:
            failures.append("v2 evidence no-replace publication is missing: {}".format(required))
    if "Write-EasyConUtf8FileAtomically" in evidence_writer:
        failures.append("v2 evidence must not use overwrite atomic publication")
    for required in (
        "schemaVersion = 2",
        "runId = $RunId",
        "environmentFingerprint",
        "gatePolicyHash",
        "verifyDurationMs",
        "gates = $gateRecords.ToArray()",
        "totalDurationMs",
        "evidenceFile",
    ):
        if required not in evidence_record:
            failures.append("v2 evidence record is missing: {}".format(required))
    for required in (
        '"--jobs"',
        "Invoke-EasyConTimedPolicyGate",
        '"git diff --cached --check"',
        '"git diff base...HEAD --check"',
        "Gates = $records.ToArray()",
    ):
        if required not in gate_runner:
            failures.append("Workspace gate runner is missing: {}".format(required))
    for required in (
        "IgnoreCase = $false",
        "-Mode must use one exact supported case",
        "switch -CaseSensitive ($Mode)",
    ):
        if required not in runner:
            failures.append("runner Mode case handling is missing: {}".format(required))
    return failures


def main():
    metadata = cargo_metadata()
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
    windows_gate_policy_text = (ROOT / "tools/windows_gate_policy.json").read_text(
        encoding="utf-8"
    )
    try:
        parse_windows_gate_policy(windows_gate_policy_text)
    except ValueError as error:
        failures.append("Windows gate policy is invalid: {}".format(error))
    required_ci = (ROOT / ".github/workflows/required-ci.yml").read_text(encoding="utf-8")
    failures.extend(required_ci_failures(required_ci))
    tracked = git_files()
    tracked_set = set(tracked)
    failures.extend(f0_contract_failures(tracked, metadata))
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
    windows_gate_policy_module = (ROOT / "tools/windows_gate_policy.ps1").read_text(
        encoding="utf-8"
    )
    windows_runner = (ROOT / "tools/run_windows_workspace.ps1").read_text(
        encoding="utf-8"
    )
    failures.extend(windows_workspace_module_failures(windows_module))
    failures.extend(
        windows_gate_policy_failures(
            windows_module, windows_gate_policy_module, windows_runner
        )
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
