#!/usr/bin/env python3
"""Validate the milestone JSON schemas and fixtures without third-party packages."""

import copy
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path, PurePosixPath


ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "spec"


class ValidationError(Exception):
    """Raised when a tracked specification violates the milestone contract."""


def reject_duplicate_object(pairs):
    result = {}
    for name, value in pairs:
        if name in result:
            raise ValidationError("duplicate JSON object key: {}".format(name))
        result[name] = value
    return result


def load_json_text(text, source):
    try:
        return json.loads(text, object_pairs_hook=reject_duplicate_object)
    except json.JSONDecodeError as error:
        raise ValidationError("{}: {}".format(source, error))


def load_json(relative_path):
    path = SPEC / relative_path
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as error:
        raise ValidationError("{}: {}".format(path, error))
    return load_json_text(text, path)


def require(condition, message):
    if not condition:
        raise ValidationError(message)


def json_equal(left, right):
    """Implement JSON Schema equality without Python's True == 1 aliasing."""
    if isinstance(left, bool) or isinstance(right, bool):
        return isinstance(left, bool) and isinstance(right, bool) and left == right
    if (isinstance(left, (int, float)) and
            isinstance(right, (int, float))):
        return left == right
    if type(left) is not type(right):
        return False
    if isinstance(left, list):
        return (len(left) == len(right) and
                all(json_equal(a, b) for a, b in zip(left, right)))
    if isinstance(left, dict):
        return (left.keys() == right.keys() and
                all(json_equal(value, right[name])
                    for name, value in left.items()))
    return left == right


SUPPORTED_SCHEMA_KEYS = {
    "$schema", "$id", "title", "description", "type", "additionalProperties",
    "required", "properties", "const", "enum", "items", "minItems", "maxItems",
    "uniqueItems", "minimum", "maximum", "oneOf",
}


def validate_schema_keywords(schema, path="$"):
    unknown = set(schema) - SUPPORTED_SCHEMA_KEYS
    require(not unknown,
            "{} uses unsupported schema keywords: {}".format(path, sorted(unknown)))
    for name, child in schema.get("properties", {}).items():
        validate_schema_keywords(child, "{}.properties.{}".format(path, name))
    additional = schema.get("additionalProperties")
    if isinstance(additional, dict):
        validate_schema_keywords(additional, "{}.additionalProperties".format(path))
    items = schema.get("items")
    if isinstance(items, dict):
        validate_schema_keywords(items, "{}.items".format(path))
    for index, child in enumerate(schema.get("oneOf", [])):
        validate_schema_keywords(child, "{}.oneOf[{}]".format(path, index))


def validate_instance(instance, schema, path="$",
                      schema_name="schema"):
    """Validate the JSON Schema subset used by the tracked v1 fixtures."""
    if "oneOf" in schema:
        matches = 0
        for option in schema["oneOf"]:
            try:
                validate_instance(instance, option, path, schema_name)
                matches += 1
            except ValidationError:
                pass
        require(matches == 1,
                "{}: {} must match exactly one oneOf branch".format(schema_name, path))

    if "const" in schema:
        require(json_equal(instance, schema["const"]),
                "{}: {} must equal {!r}".format(schema_name, path, schema["const"]))
    if "enum" in schema:
        require(any(json_equal(instance, candidate)
                    for candidate in schema["enum"]),
                "{}: {} is not one of {!r}".format(schema_name, path, schema["enum"]))

    expected_type = schema.get("type")
    type_checks = {
        "object": lambda value: isinstance(value, dict),
        "array": lambda value: isinstance(value, list),
        "string": lambda value: isinstance(value, str),
        "integer": lambda value: isinstance(value, int) and not isinstance(value, bool),
        "boolean": lambda value: isinstance(value, bool),
        "null": lambda value: value is None,
    }
    if expected_type:
        require(expected_type in type_checks,
                "{}: unsupported schema type {}".format(schema_name, expected_type))
        require(type_checks[expected_type](instance),
                "{}: {} must be {}".format(schema_name, path, expected_type))

    if isinstance(instance, dict):
        properties = schema.get("properties", {})
        for name in schema.get("required", []):
            require(name in instance,
                    "{}: {} is missing required property {}".format(schema_name, path, name))
        additional = schema.get("additionalProperties", True)
        for name, value in instance.items():
            child_path = "{}.{}".format(path, name)
            if name in properties:
                validate_instance(value, properties[name], child_path, schema_name)
            elif additional is False:
                raise ValidationError(
                    "{}: {} is not an allowed property".format(schema_name, child_path)
                )
            elif isinstance(additional, dict):
                validate_instance(value, additional, child_path, schema_name)

    if isinstance(instance, list):
        if "minItems" in schema:
            require(len(instance) >= schema["minItems"],
                    "{}: {} has too few items".format(schema_name, path))
        if "maxItems" in schema:
            require(len(instance) <= schema["maxItems"],
                    "{}: {} has too many items".format(schema_name, path))
        if schema.get("uniqueItems"):
            require(all(not json_equal(item, previous)
                        for index, item in enumerate(instance)
                        for previous in instance[:index]),
                    "{}: {} items must be unique".format(schema_name, path))
        item_schema = schema.get("items")
        if isinstance(item_schema, dict):
            for index, value in enumerate(instance):
                validate_instance(value, item_schema,
                                  "{}[{}]".format(path, index), schema_name)

    if isinstance(instance, int) and not isinstance(instance, bool):
        if "minimum" in schema:
            require(instance >= schema["minimum"],
                    "{}: {} is below minimum".format(schema_name, path))
        if "maximum" in schema:
            require(instance <= schema["maximum"],
                    "{}: {} exceeds maximum".format(schema_name, path))


def encode_report(state):
    button = state["button_mask"]
    serialized = button.to_bytes(2, byteorder="big") + bytes(
        [state["hat"], state["lx"], state["ly"], state["rx"], state["ry"]]
    )
    accumulator = 0
    bit_count = 0
    packet = []
    for value in serialized:
        accumulator = (accumulator << 8) | value
        bit_count += 8
        while bit_count >= 7:
            bit_count -= 7
            packet.append(accumulator >> bit_count)
            accumulator &= (1 << bit_count) - 1
    packet[-1] |= 0x80
    return packet


def validate_schemas():
    mappings = [
        ("schemas/behavior-v1.schema.json", "behavior/runtime-controller-v1.json"),
        (
            "schemas/runtime-r0-v2-fixture-v1.schema.json",
            "fixtures/runtime/r0-v2-contract-v1.json",
        ),
        ("schemas/controller-fixture-v1.schema.json", "fixtures/controller/reports-v1.json"),
        ("schemas/conformance-v1.schema.json", "conformance/runtime-controller-v1.json"),
        ("schemas/sequence-trace-v1.schema.json", "fixtures/controller/sequence-traces-v1.json"),
        ("schemas/latency-result-v1.schema.json", "fixtures/controller/phase2a-latency-result-v1.json"),
        ("schemas/ecs-provenance-manifest-v1.schema.json", "fixtures/ecs/manifest.json"),
    ]
    for schema_name, instance_name in mappings:
        schema = load_json(schema_name)
        require(
            schema.get("$schema") == "https://json-schema.org/draft/2020-12/schema",
            "{} does not declare JSON Schema Draft 2020-12".format(schema_name),
        )
        require(
            schema.get("$id", "").endswith(Path(schema_name).name),
            "{} has an invalid $id".format(schema_name),
        )
        require(schema.get("type") == "object", "{} must validate an object".format(schema_name))
        validate_schema_keywords(schema, schema_name)
        validate_instance(load_json(instance_name), schema, schema_name=schema_name)


def require_rejected(instance, schema, message):
    try:
        validate_instance(instance, schema, schema_name="validator regression")
    except ValidationError:
        return
    raise ValidationError(message)


def require_json_rejected(text, message):
    try:
        load_json_text(text, "validator regression")
    except ValidationError:
        return
    raise ValidationError(message)


def validate_validator_regressions():
    require_json_rejected(
        '{"fixture_id":"first","fixture_id":"second"}',
        "duplicate JSON object keys were not rejected",
    )
    sequence_schema = load_json("schemas/sequence-trace-v1.schema.json")
    trace_schema = sequence_schema["properties"]["traces"]["items"]
    step_schema = trace_schema["properties"]["steps"]["items"]
    invalid_steps = [
        {"offset_ns": 0, "action": "button_down"},
        {"offset_ns": 0, "action": "reset", "button": "A"},
        {"offset_ns": 0, "action": "hat", "x": 0, "y": 0},
    ]
    for step in invalid_steps:
        require_rejected(step, step_schema,
                         "sequence action fields were not rejected: {!r}".format(step))
    require_rejected(True, {"const": 1},
                     "JSON boolean must not satisfy a numeric const")


ECS_FIXTURE_ROOT = SPEC / "fixtures" / "ecs"
ECS_LEGACY_COMMIT = "11c4b992b9bce0ff977e9c587a6c0bb0d302853e"
LOWER_SHA256 = re.compile(r"[0-9a-f]{64}\Z")
LOWER_SHA1 = re.compile(r"[0-9a-f]{40}\Z")

ECS_RECORDS = [
    ("legacy-exact.integer-operators", "Legacy Exact", "production-source"),
    ("corrected.print.cli", "Corrected", "production-cli"),
    ("corrected.print.winforms", "Corrected", "production-winforms"),
    ("corrected.print.winforms-lite", "Corrected", "production-winforms-lite"),
    ("corrected.print.avalonia", "Corrected", "production-avalonia"),
    ("corrected.print.test-mock", "Corrected", "test-oracle"),
    ("corrected.label.cli", "Corrected", "production-cli"),
    ("corrected.label.avalonia", "Corrected", "production-avalonia"),
    ("corrected.label.winforms", "Corrected", "production-winforms"),
    ("v1-native.heap-order.left-medium", "v1-native", "adr-v1-contract"),
    ("v1-native.heap-order.medium-left", "v1-native", "adr-v1-contract"),
]

ECS_PROGRAM_HASHES = {
    "legacy-exact.integer-operators": (
        "d9106739fd6e1f664fc9807a4b2549d83362350e4626a15c9e0f48a0a241678b"
    ),
    "corrected.print.cli": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.winforms": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.winforms-lite": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.avalonia": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "corrected.print.test-mock": (
        "bb9121e382ac1bb02bb4b1ca6f3e826c7a59defd3358e1b67bd6061ac5de80d8"
    ),
    "v1-native.heap-order.left-medium": (
        "c000c49eceea2d592bc1787347b10aef88d3834624b9c64ac8df3866dd7ff8bb"
    ),
    "v1-native.heap-order.medium-left": (
        "83734a419b0df485cee09a365952e0f3b255948285eb220202740121c7c5a570"
    ),
}

ECS_INPUT_HASHES = {
    "legacy-exact.integer-operators": (
        "5d8276eab558b32820d8616027bf194667bca4c19ac62f9f1b40036a359c4263"
    ),
    "corrected.print.cli": (
        "f7162011fb86fdd1dc9f673a8d7f7dbbc2ce955e8ca4f9b1a1ab8318d306995a"
    ),
    "corrected.print.winforms": (
        "f7162011fb86fdd1dc9f673a8d7f7dbbc2ce955e8ca4f9b1a1ab8318d306995a"
    ),
    "corrected.print.winforms-lite": (
        "f7162011fb86fdd1dc9f673a8d7f7dbbc2ce955e8ca4f9b1a1ab8318d306995a"
    ),
    "corrected.print.avalonia": (
        "f7162011fb86fdd1dc9f673a8d7f7dbbc2ce955e8ca4f9b1a1ab8318d306995a"
    ),
    "corrected.print.test-mock": (
        "f7162011fb86fdd1dc9f673a8d7f7dbbc2ce955e8ca4f9b1a1ab8318d306995a"
    ),
    "v1-native.heap-order.left-medium": (
        "12e61485f770a624f4575fc5c99b2d079afce6a1ea25d0e6c73a25cdaef82d4f"
    ),
    "v1-native.heap-order.medium-left": (
        "1094bfede150bafc6d9396487c705e46973999000b640a3ecebc4a6d9e51c922"
    ),
}

ECS_LIMITS = [
    (1, "profile_version", "u32", 1),
    (2, "source_units", "u32", 64),
    (3, "per_source_bytes", "u64", 262144),
    (4, "bundle_bytes", "u64", 1048576),
    (5, "source_id_bytes", "u64", 256),
    (6, "identifier_bytes", "u64", 128),
    (7, "parameters", "u32", 32),
    (8, "arguments", "u32", 32),
    (9, "syntax_nesting", "u32", 64),
    (10, "functions", "u32", 256),
    (11, "symbols", "u32", 4096),
    (12, "tokens", "u32", 262144),
    (13, "ast_nodes", "u32", 131072),
    (14, "bound_nodes", "u32", 262144),
    (15, "lowered_nodes", "u32", 262144),
    (16, "instructions", "u32", 262144),
    (17, "diagnostics_per_source", "u32", 64),
    (18, "diagnostics_total", "u32", 512),
    (19, "reserved_limit_diagnostics", "u32", 1),
    (20, "call_depth", "u32", 128),
    (21, "array_cells", "u32", 16384),
    (22, "string_bytes", "u64", 262144),
    (23, "live_logical_heap_bytes", "u64", 33554432),
    (24, "output_fragment_bytes", "u64", 32768),
    (25, "output_queue_pending", "u32", 32),
    (26, "output_payload_bytes", "u64", 1048576),
    (27, "production_instruction_fuel_present", "u8", 0),
    (28, "production_output_count_present", "u8", 0),
]

ECS_CLASSIFICATIONS = {
    "Legacy Exact": [
        "round-div-midpoint-away-from-zero",
        "integer-xor",
        "logical-and-or-short-circuit",
        "ordinary-i32-wrap-and-shift",
        "import-bind-nop",
        "bundle-libs-loaded-without-import",
        "shared-lib-scope",
        "lib-main-visibility-and-lib-globals-first",
        "reachable-array-string-control-flow-success",
    ],
    "Corrected": [
        "for-i32-max-stop-after-upper",
        "boolean-literals-executable",
        "libs-raw-utf8-sort",
        "lf-crlf-cr-newline",
        "typed-diagnostic-failure",
        "utf8-byte-span-and-unicode-scalar-string",
        "print-continuation",
        "label-floor",
        "effect-checkpoints-and-five-way-cleanup-before-terminal",
    ],
    "v1-native": [
        "source-bundle-and-restricted-loader",
        "bom-and-program-hash",
        "pcg-and-replay",
        "monotonic-time-and-absolute-wait",
        "ecs-limits",
        "immutable-program-and-run-completion",
        "typed-and-recording-ports",
        "generic-error-projection",
        "runtime-five-way-terminal-race",
    ],
}

ECS_REQUIRED_SOURCES = {
    "legacy-exact.integer-operators": {
        "src/EasyCon.Script/Binding/BoundBinaryOperator.cs": {
            "BoundBinaryOperator._operators"
        },
        "src/EasyCon.Script/Evaluator.cs": {
            "Evaluator.EvaluateBinaryExpression"
        },
    },
    "corrected.print.cli": {
        "src/EasyCon.Script/Binding/BuiltinCallable.cs": {
            "BuiltinCallable.ImplPrint"
        },
        "src/EasyCon2.CLI/ConsoleOutAdapter.cs": {
            "ConsoleOutAdapter.Print",
            "ColorfulConsole.Write",
            "AnsiColors.Reset",
            "AnsiColors.White",
            "AnsiColors.Gray",
        },
    },
    "corrected.print.winforms": {
        "src/EasyCon.Script/Binding/BuiltinCallable.cs": {
            "BuiltinCallable.ImplPrint"
        },
        "src/EasyCon2/App/EasyConForm.cs": {"EasyConForm.Print"},
        "src/EasyCon2/Controls/RichLogBox.cs": {"RichLogBox.Print"},
    },
    "corrected.print.winforms-lite": {
        "src/EasyCon.Script/Binding/BuiltinCallable.cs": {
            "BuiltinCallable.ImplPrint"
        },
        "src/EasyCon2/Program.cs": {"Program.Main"},
        "src/EasyCon2/App/MainForm.cs": {
            "MainForm.runStopBtn_Click",
            "MainForm.Print",
        },
        "src/EasyCon2/Controls/RichLogBox.cs": {"RichLogBox.Print"},
    },
    "corrected.print.avalonia": {
        "src/EasyCon.Script/Binding/BuiltinCallable.cs": {
            "BuiltinCallable.ImplPrint"
        },
        "src/EasyCon2.Avalonia/Services/LogService.cs": {
            "LogService.Print",
            "LogService.Append",
            "LogService.Flush",
        },
        "src/EasyCon2.Avalonia.Core/Services/LogService.cs": {
            "LogService.Print",
            "LogService.Flush",
        },
    },
    "corrected.print.test-mock": {
        "src/EasyCon.Script/Binding/BuiltinCallable.cs": {
            "BuiltinCallable.ImplPrint"
        },
        "test/EasyCon.Tests/EvaluatorTests.cs": {"MockOutputAdapter.Print"},
    },
    "corrected.label.cli": {
        "src/EasyCon.Capture/ImgLabel.cs": {"ImgLabel.Search"},
        "src/EasyCon2.CLI/Program.cs": {"externalGetters"},
    },
    "corrected.label.avalonia": {
        "src/EasyCon.Capture/ImgLabel.cs": {"ImgLabel.Search"},
        "src/EasyCon2.Avalonia/Services/ScriptService.cs": {
            "ScriptService.Run externalGetters"
        },
    },
    "corrected.label.winforms": {
        "src/EasyCon.Capture/ImgLabel.cs": {"ImgLabel.Search"},
        "src/EasyCon2/Services/CaptureService.cs": {
            "CaptureService.BuildExternalGetters"
        },
    },
}

ECS_PRINT_IMPLEMENTATIONS = {
    "corrected.print.cli": {
        "src/EasyCon2.CLI/ConsoleOutAdapter.cs::ConsoleOutAdapter.Print"
    },
    "corrected.print.winforms": {
        "src/EasyCon2/App/EasyConForm.cs::EasyConForm.Print"
    },
    "corrected.print.winforms-lite": {
        "src/EasyCon2/App/MainForm.cs::MainForm.Print"
    },
    "corrected.print.avalonia": {
        "src/EasyCon2.Avalonia/Services/LogService.cs::LogService.Print",
        "src/EasyCon2.Avalonia.Core/Services/LogService.cs::LogService.Print",
    },
    "corrected.print.test-mock": {
        "test/EasyCon.Tests/EvaluatorTests.cs::MockOutputAdapter.Print"
    },
}


def require_keys(value, expected, label):
    require(isinstance(value, dict), "{} must be an object".format(label))
    actual = set(value)
    require(
        actual == set(expected),
        "{} fields differ: missing={!r}, unknown={!r}".format(
            label, sorted(set(expected) - actual), sorted(actual - set(expected))
        ),
    )


def validate_relative_path(value, label, prefix=None):
    require(isinstance(value, str) and value, "{} must be a non-empty string".format(label))
    require("\\" not in value and "\0" not in value, "{} is not slash-relative".format(label))
    require("://" not in value and ":" not in value, "{} is not a local relative path".format(label))
    parsed = PurePosixPath(value)
    require(not parsed.is_absolute(), "{} must not be absolute".format(label))
    require(
        all(part not in ("", ".", "..") for part in parsed.parts),
        "{} contains an unsafe path segment".format(label),
    )
    require(parsed.as_posix() == value, "{} is not normalized".format(label))
    if prefix is not None:
        require(value.startswith(prefix), "{} must remain under {}".format(label, prefix))
    return parsed


def validate_artifact_payload(reference, data, label):
    require(
        LOWER_SHA256.fullmatch(reference["sha256"]) is not None,
        "{} SHA-256 must be lowercase hexadecimal".format(label),
    )
    require(len(data) == reference["bytes"], "{} byte count mismatch".format(label))
    require(
        hashlib.sha256(data).hexdigest() == reference["sha256"],
        "{} SHA-256 mismatch".format(label),
    )


def read_artifact(reference, fixture_root, seen, label):
    relative = validate_relative_path(reference["path"], label + ".path", "data/")
    current = fixture_root
    for part in relative.parts:
        current = current / part
        require(not current.is_symlink(), "{} traverses a symlink".format(label))
    resolved_root = fixture_root.resolve()
    resolved = current.resolve()
    try:
        resolved.relative_to(resolved_root)
    except ValueError:
        raise ValidationError("{} escapes the SDK fixture root".format(label))
    require(resolved.is_file(), "{} does not name an SDK-local file".format(label))
    try:
        data = resolved.read_bytes()
    except OSError as error:
        raise ValidationError("{}: {}".format(label, error))
    validate_artifact_payload(reference, data, label)
    previous = seen.get(reference["path"])
    identity = (reference["bytes"], reference["sha256"])
    require(
        previous is None or previous == identity,
        "conflicting artifact identity for {}".format(reference["path"]),
    )
    seen[reference["path"]] = identity
    return data


def load_artifact_json(data, label):
    try:
        text = data.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValidationError("{} is not UTF-8: {}".format(label, error))
    return load_json_text(text, label)


def expected_profile_document():
    return {
        "schema": "easycon-sdk:ecs-limits-profile:v1",
        "schema_version": 1,
        "hash_format_version": 1,
        "ecs_semantics_version": 1,
        "limits": [
            {"order": order, "name": name, "encoding": encoding, "value": value}
            for order, name, encoding, value in ECS_LIMITS
        ],
    }


def validate_source_catalog(document):
    require_keys(
        document,
        {"schema", "schema_version", "legacy_commit", "entries"},
        "ECS legacy source catalog",
    )
    require(
        document["schema"] == "easycon-sdk:legacy-source-snapshots:v1"
        and document["schema_version"] == 1
        and document["legacy_commit"] == ECS_LEGACY_COMMIT,
        "ECS legacy source catalog identity changed",
    )
    require(isinstance(document["entries"], list) and document["entries"],
            "ECS legacy source catalog has no entries")
    snapshots = {}
    files = {}
    for index, entry in enumerate(document["entries"]):
        label = "ECS source catalog entry {}".format(index)
        require_keys(
            entry,
            {
                "snapshot_id", "repo_relative_path", "symbols", "source_blob_bytes",
                "source_blob_sha256", "git_blob_sha1", "line_start", "line_end",
                "content", "content_sha256",
            },
            label,
        )
        snapshot_id = entry["snapshot_id"]
        require(isinstance(snapshot_id, str) and snapshot_id,
                "{} has an invalid snapshot ID".format(label))
        require(snapshot_id not in snapshots,
                "duplicate ECS source snapshot ID: {}".format(snapshot_id))
        validate_relative_path(entry["repo_relative_path"], label + ".repo_relative_path")
        require(
            entry["repo_relative_path"].startswith(("src/", "test/")),
            "{} is not legacy repository metadata".format(label),
        )
        require(
            isinstance(entry["symbols"], list)
            and entry["symbols"]
            and all(isinstance(symbol, str) and symbol for symbol in entry["symbols"])
            and len(entry["symbols"]) == len(set(entry["symbols"])),
            "{} symbols are invalid or duplicated".format(label),
        )
        require(
            isinstance(entry["source_blob_bytes"], int)
            and not isinstance(entry["source_blob_bytes"], bool)
            and entry["source_blob_bytes"] > 0,
            "{} source byte count is invalid".format(label),
        )
        require(LOWER_SHA256.fullmatch(entry["source_blob_sha256"]) is not None,
                "{} source SHA-256 is malformed".format(label))
        require(LOWER_SHA1.fullmatch(entry["git_blob_sha1"]) is not None,
                "{} Git blob SHA-1 is malformed".format(label))
        require(
            isinstance(entry["line_start"], int)
            and not isinstance(entry["line_start"], bool)
            and isinstance(entry["line_end"], int)
            and not isinstance(entry["line_end"], bool)
            and 1 <= entry["line_start"] <= entry["line_end"],
            "{} line range is invalid".format(label),
        )
        require(isinstance(entry["content"], str), "{} content must be text".format(label))
        content = entry["content"].encode("utf-8")
        require(
            content.count(b"\n") == entry["line_end"] - entry["line_start"] + 1,
            "{} content does not match its line range".format(label),
        )
        require(
            LOWER_SHA256.fullmatch(entry["content_sha256"]) is not None
            and hashlib.sha256(content).hexdigest() == entry["content_sha256"],
            "{} content SHA-256 mismatch".format(label),
        )
        file_identity = (
            entry["source_blob_bytes"],
            entry["source_blob_sha256"],
            entry["git_blob_sha1"],
        )
        prior = files.get(entry["repo_relative_path"])
        require(
            prior is None or prior == file_identity,
            "conflicting legacy source identity for {}".format(entry["repo_relative_path"]),
        )
        files[entry["repo_relative_path"]] = file_identity
        snapshots[snapshot_id] = entry
    return snapshots, files


def expected_print_source():
    slash = "\\"
    return (
        "\n".join(
            [
                'PRINT "A{}"'.format(slash),
                'PRINT "B{}"'.format(slash),
                'PRINT "C"',
                'PRINT "D"',
            ]
        )
        + "\n"
    ).encode("utf-8")


def expected_print_fragments(fixture_id):
    return {
        "schema": "easycon-sdk:print-fragments:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "fragments": [
            {"text": "A", "starts_new_line": True},
            {"text": "B", "starts_new_line": False},
            {"text": "C", "starts_new_line": False},
            {"text": "D", "starts_new_line": True},
        ],
    }


def validate_print_artifacts(record, input_data, observed_data, expected_data):
    fixture_id = record["fixture_id"]
    require(input_data == expected_print_source(),
            "{} PRINT input bytes changed".format(fixture_id))
    expected = load_artifact_json(expected_data, fixture_id + " v1 expected")
    require(expected == expected_print_fragments(fixture_id),
            "{} v1 PrintFragment trace changed".format(fixture_id))
    observed = load_artifact_json(observed_data, fixture_id + " legacy observed")
    require_keys(
        observed,
        {"schema", "schema_version", "fixture_id", "implementations"},
        fixture_id + " legacy observed",
    )
    require(
        observed["schema"] == "easycon-sdk:legacy-print-token-trace:v1"
        and observed["schema_version"] == 1
        and observed["fixture_id"] == fixture_id,
        "{} legacy PRINT trace identity changed".format(fixture_id),
    )
    require(isinstance(observed["implementations"], list),
            "{} implementations must be an array".format(fixture_id))
    names = []
    token_kinds = set()
    transports = set()
    for implementation in observed["implementations"]:
        require_keys(
            implementation,
            {"implementation", "adapter_calls", "flush"},
            fixture_id + " implementation",
        )
        names.append(implementation["implementation"])
        calls = implementation["adapter_calls"]
        require(isinstance(calls, list) and len(calls) == 4,
                "{} must preserve four adapter calls".format(fixture_id))
        observed_calls = []
        for call in calls:
            require_keys(
                call,
                {"ordinal", "message", "newline", "emissions"},
                fixture_id + " adapter call",
            )
            observed_calls.append((call["ordinal"], call["message"], call["newline"]))
            require(isinstance(call["emissions"], list) and call["emissions"],
                    "{} adapter call has no frontend emissions".format(fixture_id))
            for item in call["emissions"]:
                validate_print_emission(item, fixture_id, token_kinds, transports)
        require(
            observed_calls == [(1, "A", True), (2, "B", False),
                               (3, "C", False), (4, "D", True)],
            "{} adapter call sequence changed".format(fixture_id),
        )
        require(isinstance(implementation["flush"], list),
                "{} flush trace must be an array".format(fixture_id))
        for item in implementation["flush"]:
            validate_print_emission(item, fixture_id, token_kinds, transports)
    require(len(names) == len(set(names)),
            "{} duplicates a production implementation".format(fixture_id))
    require(set(names) == ECS_PRINT_IMPLEMENTATIONS[fixture_id],
            "{} production/test implementation set is incomplete".format(fixture_id))

    if fixture_id == "corrected.print.cli":
        require({"Timestamp", "Ansi", "HostNewLine", "Payload"} <= token_kinds,
                "CLI PRINT tokens are incomplete")
        require({"Console.Write", "Console.WriteLine"} <= transports,
                "CLI PRINT rendering trace is incomplete")
    elif fixture_id in ("corrected.print.winforms", "corrected.print.winforms-lite"):
        require({"Timestamp", "HostNewLine", "Payload", "WakeSignal"} <= token_kinds,
                "WinForms PRINT tokens are incomplete")
        require("UiQueueEnqueue" in transports,
                "WinForms PRINT UI queue trace is missing")
    elif fixture_id == "corrected.print.avalonia":
        require({"Timestamp", "LF", "Payload", "UiPost", "Callback"} <= token_kinds,
                "Avalonia PRINT tokens are incomplete")
        require({"BufferAppend", "EntryEnqueue", "Dispatcher.UIThread.Post"} <= transports,
                "both Avalonia production traces are required")
    else:
        require(token_kinds == {"Payload", "LF"} and transports == {"ListAppend"},
                "test mock must remain a trailing-LF list oracle")


def validate_print_emission(item, fixture_id, token_kinds, transports):
    require_keys(item, {"transport", "tokens"}, fixture_id + " emission")
    require(isinstance(item["transport"], str) and item["transport"],
            "{} emission transport is invalid".format(fixture_id))
    require(isinstance(item["tokens"], list) and item["tokens"],
            "{} emission has no typed tokens".format(fixture_id))
    transports.add(item["transport"])
    for item_token in item["tokens"]:
        require_keys(item_token, {"kind", "value"}, fixture_id + " token")
        require(
            item_token["kind"] in {
                "Timestamp", "Ansi", "Payload", "HostNewLine", "LF",
                "WakeSignal", "UiPost", "Callback",
            }
            and isinstance(item_token["value"], str),
            "{} contains an unknown or malformed token".format(fixture_id),
        )
        if item_token["kind"] == "Timestamp":
            require(item_token["value"] in {"[HH:mm:ss.fff] ", "[HH:mm:ss] "},
                    "{} timestamp token was concretized or changed".format(fixture_id))
        token_kinds.add(item_token["kind"])


def validate_label_artifacts(record, input_data, observed_data, expected_data):
    fixture_id = record["fixture_id"]
    input_document = load_artifact_json(input_data, fixture_id + " input")
    observed = load_artifact_json(observed_data, fixture_id + " legacy observed")
    expected = load_artifact_json(expected_data, fixture_id + " v1 expected")
    require(
        input_document
        == {
            "schema": "easycon-sdk:label-score-input:v1",
            "schema_version": 1,
            "fixture_id": fixture_id,
            "normalized_score": 0.4225,
            "legacy_scale_operation": "md *= 100",
            "legacy_scaled_score": 42.25,
        },
        "{} normalized and legacy-scaled input changed".format(fixture_id),
    )
    conversion, observed_integer = (
        ("ceiling", 43)
        if fixture_id == "corrected.label.winforms"
        else ("truncate-toward-zero", 42)
    )
    require(
        observed
        == {
            "schema": "easycon-sdk:legacy-label-projection:v1",
            "schema_version": 1,
            "fixture_id": fixture_id,
            "legacy_scaled_score": 42.25,
            "conversion": conversion,
            "observed_integer": observed_integer,
        },
        "{} legacy label projection changed".format(fixture_id),
    )
    require(
        expected
        == {
            "schema": "easycon-sdk:label-floor:v1",
            "schema_version": 1,
            "fixture_id": fixture_id,
            "normalized_score": 0.4225,
            "formula": "floor(clamp(score, 0, 1) * 100)",
            "expected_integer": 42,
        },
        "{} v1 label floor projection changed".format(fixture_id),
    )


def validate_exact_artifacts(record, observed_data, expected_data):
    fixture_id = record["fixture_id"]
    observed = load_artifact_json(observed_data, fixture_id + " legacy observed")
    expected = load_artifact_json(expected_data, fixture_id + " v1 expected")
    base = {
        "schema": "easycon-sdk:ecs-value-trace:v1",
        "schema_version": 1,
        "fixture_id": fixture_id,
        "result": [3, -3, 6, False, True],
        "short_circuit_rhs_evaluated": [False, False],
    }
    require(observed == base, "Legacy Exact observed values changed")
    expected_base = dict(base)
    expected_base["classification"] = "Legacy Exact"
    require(expected == expected_base, "Legacy Exact v1 expected values changed")


def validate_heap_artifact(record, expected_data):
    fixture_id = record["fixture_id"]
    document = load_artifact_json(expected_data, fixture_id + " v1 expected")
    common_keys = {
        "schema", "schema_version", "fixture_id", "limit", "baseline",
        "source_order", "checkpoints", "outcome",
    }
    if fixture_id.endswith("left-medium"):
        required = common_keys | {"canonical_peak", "counterfactual_right_first"}
    else:
        required = common_keys | {"canonical_peak_attempt"}
    require_keys(document, required, fixture_id + " heap ledger")
    require(
        document["schema"] == "easycon-sdk:ecs-heap-ledger:v1"
        and document["schema_version"] == 1
        and document["fixture_id"] == fixture_id
        and document["limit"] == 33554432,
        "{} heap ledger identity or limit changed".format(fixture_id),
    )
    require(
        document["baseline"]
        == {
            "distinct_string_allocations": 132,
            "string_bytes_each": 251000,
            "string_bytes_total": 33132000,
            "array_direct_cells": 132,
            "array_direct_cell_bytes": 1056,
            "live_bytes": 33133056,
        },
        "{} heap baseline changed".format(fixture_id),
    )
    require(isinstance(document["checkpoints"], list) and document["checkpoints"],
            "{} has no heap ledger checkpoints".format(fixture_id))
    for checkpoint in document["checkpoints"]:
        require_keys(checkpoint, {"id", "live_bytes", "decision"},
                     fixture_id + " heap checkpoint")
        require(
            isinstance(checkpoint["live_bytes"], int)
            and not isinstance(checkpoint["live_bytes"], bool),
            "{} checkpoint bytes have the wrong type".format(fixture_id),
        )
    if fixture_id == "v1-native.heap-order.left-medium":
        require(
            document["source_order"] == ["LEFT($template)", "MEDIUM($template)"]
            and document["canonical_peak"] == 33533058
            and document["canonical_peak"] < document["limit"]
            and document["outcome"]
            == {"kind": "success", "result_utf8_bytes": 200001},
            "left-medium canonical success changed",
        )
        require(
            document["counterfactual_right_first"]
            == {
                "classification": "forbidden-negative-oracle",
                "medium_live_bytes": 33333056,
                "left_full_reserve_attempt_bytes": 33584056,
                "decision": "reject-live-logical-heap-limit",
            },
            "right-first counterfactual must remain a forbidden negative oracle",
        )
    else:
        require(
            document["source_order"] == ["MEDIUM($template)", "LEFT($template)"]
            and document["canonical_peak_attempt"] == 33584056
            and document["canonical_peak_attempt"] > document["limit"]
            and document["outcome"]
            == {"kind": "limit-failure", "limit": "live_logical_heap_bytes"},
            "medium-left canonical limit failure changed",
        )


def validate_record_sources(record, snapshots, source_files, used_snapshots):
    fixture_id = record["fixture_id"]
    observed_sources = {}
    for source in record["legacy_sources"]:
        path = source["repo_relative_path"]
        validate_relative_path(path, fixture_id + " legacy source")
        require(path.startswith(("src/", "test/")),
                "{} source metadata escaped the legacy repository".format(fixture_id))
        require(path not in observed_sources,
                "{} duplicates legacy source {}".format(fixture_id, path))
        require(LOWER_SHA256.fullmatch(source["source_blob_sha256"]) is not None,
                "{} source SHA-256 is malformed".format(fixture_id))
        require(LOWER_SHA1.fullmatch(source["git_blob_sha1"]) is not None,
                "{} Git blob identity is malformed".format(fixture_id))
        identity = (
            source["source_blob_bytes"],
            source["source_blob_sha256"],
            source["git_blob_sha1"],
        )
        require(source_files.get(path) == identity,
                "{} source identity conflicts with its SDK snapshot".format(fixture_id))
        require(len(source["symbols"]) == len(set(source["symbols"])),
                "{} duplicates a source symbol".format(fixture_id))
        require(len(source["snapshot_ids"]) == len(set(source["snapshot_ids"])),
                "{} duplicates a source snapshot".format(fixture_id))
        snapshot_symbols = set()
        for snapshot_id in source["snapshot_ids"]:
            require(snapshot_id in snapshots,
                    "{} references a missing source snapshot".format(fixture_id))
            snapshot = snapshots[snapshot_id]
            require(snapshot["repo_relative_path"] == path,
                    "{} source snapshot path conflicts".format(fixture_id))
            snapshot_symbols.update(snapshot["symbols"])
            used_snapshots.add(snapshot_id)
        require(set(source["symbols"]) == snapshot_symbols,
                "{} source symbols are not fully backed by snapshot content".format(fixture_id))
        observed_sources[path] = set(source["symbols"])
    require(observed_sources == ECS_REQUIRED_SOURCES[fixture_id],
            "{} required production/test oracle sources are incomplete".format(fixture_id))


def validate_source_identity(record, input_reference, profile_reference, hash_identities):
    fixture_id = record["fixture_id"]
    identity = record["source_identity"]
    require(
        identity == {"role": "Main", "source_id": "main.ecs"},
        "{} source identity changed".format(fixture_id),
    )
    source_id = identity["source_id"].encode("utf-8")
    require(0 < len(source_id) <= 256,
            "{} source ID violates the v1 profile".format(fixture_id))
    program_hash = record["program_hash_v1"]
    require(LOWER_SHA256.fullmatch(program_hash) is not None,
            "{} ProgramHash is malformed".format(fixture_id))
    require(program_hash == ECS_PROGRAM_HASHES[fixture_id],
            "{} static ProgramHash golden changed".format(fixture_id))
    require(record["program_hash_profile"] == profile_reference,
            "{} ProgramHash profile identity changed".format(fixture_id))
    program_identity = (
        identity["role"],
        identity["source_id"],
        input_reference["sha256"],
        profile_reference["sha256"],
    )
    previous = hash_identities.get(program_hash)
    require(
        previous is None or previous == program_identity,
        "conflicting source identity shares ProgramHash {}".format(program_hash),
    )
    hash_identities[program_hash] = program_identity


def validate_ecs_provenance_document(manifest, schema):
    validate_instance(
        manifest,
        schema,
        schema_name="schemas/ecs-provenance-manifest-v1.schema.json",
    )
    require(manifest["legacy_reference_commit"] == ECS_LEGACY_COMMIT,
            "ECS legacy reference commit changed")
    require(manifest["classification_contracts"] == ECS_CLASSIFICATIONS,
            "ECS provenance classifications changed or were mixed")

    seen_artifacts = {}
    profile_data = read_artifact(
        manifest["program_hash_profile"],
        ECS_FIXTURE_ROOT,
        seen_artifacts,
        "ECS ProgramHash profile",
    )
    profile = load_artifact_json(profile_data, "ECS ProgramHash profile")
    require(profile == expected_profile_document(),
            "EcsLimitsV1 profile or canonical field order changed")

    catalog_data = read_artifact(
        manifest["legacy_source_catalog"],
        ECS_FIXTURE_ROOT,
        seen_artifacts,
        "ECS legacy source catalog",
    )
    snapshots, source_files = validate_source_catalog(
        load_artifact_json(catalog_data, "ECS legacy source catalog")
    )

    identities = [(item["fixture_id"], item["provenance_class"], item["oracle_kind"])
                  for item in manifest["records"]]
    require(identities == ECS_RECORDS,
            "ECS fixture obligations changed, were reordered, or are incomplete")
    require(len({item[0] for item in identities}) == len(identities),
            "duplicate ECS fixture identity")

    used_snapshots = set()
    owned_artifact_paths = set()
    hash_identities = {}
    source_inputs = {}
    for record in manifest["records"]:
        fixture_id = record["fixture_id"]
        provenance_class = record["provenance_class"]
        require(isinstance(record["revision_reason"], str) and record["revision_reason"].strip(),
                "{} has no revision/classification reason".format(fixture_id))
        require(
            set(record["covered_contracts"]).issubset(
                set(manifest["classification_contracts"][provenance_class])
            ),
            "{} maps a contract from another provenance class".format(fixture_id),
        )

        is_program = fixture_id in ECS_PROGRAM_HASHES
        is_print = fixture_id.startswith("corrected.print.")
        is_label = fixture_id.startswith("corrected.label.")
        if provenance_class in ("Legacy Exact", "Corrected"):
            require(record.get("legacy_commit") == ECS_LEGACY_COMMIT,
                    "{} lost its full legacy commit".format(fixture_id))
            require("legacy_sources" in record and "legacy_observed" in record,
                    "{} lost legacy source or observed evidence".format(fixture_id))
            require("contract_source" not in record and "expected_outcome" not in record,
                    "{} mixes legacy provenance with v1-native fields".format(fixture_id))
            validate_record_sources(record, snapshots, source_files, used_snapshots)
        else:
            require(
                "legacy_commit" not in record
                and "legacy_sources" not in record
                and "legacy_observed" not in record,
                "{} falsely claims differential legacy evidence".format(fixture_id),
            )
            require(
                record.get("contract_source")
                == {
                    "path": "docs/decisions/0017-phase-4-ecs-automation-target.md",
                    "section": "v1 live logical heap ledger",
                },
                "{} ADR contract source changed".format(fixture_id),
            )
            contract_path = validate_relative_path(
                record["contract_source"]["path"], fixture_id + " contract source"
            )
            require((ROOT / contract_path).resolve().is_file(),
                    "{} contract source is not SDK-local".format(fixture_id))
            expected_outcome = (
                "success" if fixture_id.endswith("left-medium") else "limit-failure"
            )
            require(record.get("expected_outcome") == expected_outcome,
                    "{} has an ambiguous or wrong outcome".format(fixture_id))

        if is_program:
            require(
                all(name in record for name in
                    ("source_identity", "program_hash_v1", "program_hash_profile")),
                "{} lost source identity or ProgramHash provenance".format(fixture_id),
            )
            require(record["input"]["sha256"] == ECS_INPUT_HASHES[fixture_id],
                    "{} exact source SHA-256 changed".format(fixture_id))
            validate_source_identity(
                record,
                record["input"],
                manifest["program_hash_profile"],
                hash_identities,
            )
        else:
            require(
                all(name not in record for name in
                    ("source_identity", "program_hash_v1", "program_hash_profile")),
                "{} label provenance acquired ECS source identity fields".format(fixture_id),
            )

        role_names = ["input", "v1_expected"]
        if "legacy_observed" in record:
            role_names.append("legacy_observed")
        role_paths = [record[name]["path"] for name in role_names]
        require(len(role_paths) == len(set(role_paths)),
                "{} reuses one artifact for conflicting roles".format(fixture_id))
        require(not (set(role_paths) & owned_artifact_paths),
                "{} reuses another fixture's artifact".format(fixture_id))
        owned_artifact_paths.update(role_paths)

        input_data = read_artifact(
            record["input"], ECS_FIXTURE_ROOT, seen_artifacts, fixture_id + " input"
        )
        expected_data = read_artifact(
            record["v1_expected"],
            ECS_FIXTURE_ROOT,
            seen_artifacts,
            fixture_id + " v1 expected",
        )
        observed_data = None
        if "legacy_observed" in record:
            observed_data = read_artifact(
                record["legacy_observed"],
                ECS_FIXTURE_ROOT,
                seen_artifacts,
                fixture_id + " legacy observed",
            )

        if is_program:
            require(record["input"]["path"].endswith(".input.ecs"),
                    "{} source artifact extension changed".format(fixture_id))
            require(len(input_data) <= 262144,
                    "{} exceeds EcsLimitsV1 per_source_bytes".format(fixture_id))
            try:
                input_data.decode("utf-8")
            except UnicodeDecodeError as error:
                raise ValidationError("{} source is not UTF-8: {}".format(fixture_id, error))
            source_inputs[fixture_id] = input_data
        elif is_label:
            require(record["input"]["path"].endswith(".input.json"),
                    "{} label input extension changed".format(fixture_id))

        if fixture_id == "legacy-exact.integer-operators":
            validate_exact_artifacts(record, observed_data, expected_data)
        elif is_print:
            validate_print_artifacts(
                record, input_data, observed_data, expected_data
            )
        elif is_label:
            validate_label_artifacts(
                record, input_data, observed_data, expected_data
            )
        else:
            validate_heap_artifact(record, expected_data)

    require(used_snapshots == set(snapshots),
            "legacy source catalog contains missing or unreferenced snapshot evidence")
    left = source_inputs["v1-native.heap-order.left-medium"]
    right = source_inputs["v1-native.heap-order.medium-left"]
    require(len(left) == 251321 and len(right) == 251321,
            "heap-order exact source size changed")
    require(left.splitlines()[:-1] == right.splitlines()[:-1],
            "heap-order pair differs outside the swapped operand line")
    require(
        left.splitlines()[-1] == b"$result = LEFT($template) + MEDIUM($template)"
        and right.splitlines()[-1] == b"$result = MEDIUM($template) + LEFT($template)",
        "heap-order pair no longer swaps only LEFT and MEDIUM",
    )

    actual_files = {
        path.relative_to(ECS_FIXTURE_ROOT).as_posix()
        for path in ECS_FIXTURE_ROOT.rglob("*")
        if path.is_file()
    }
    expected_files = {"manifest.json"} | set(seen_artifacts)
    require(actual_files == expected_files,
            "ECS fixture file set is not exactly manifest-owned")
    require(len(seen_artifacts) == 33,
            "ECS manifest must own exactly 33 self-contained artifacts")


def require_ecs_provenance_rejected(manifest, schema, message):
    try:
        validate_ecs_provenance_document(manifest, schema)
    except ValidationError:
        return
    raise ValidationError(message)


def require_artifact_payload_rejected(reference, data, message):
    try:
        validate_artifact_payload(reference, data, "validator regression")
    except ValidationError:
        return
    raise ValidationError(message)


def validate_ecs_provenance_regressions():
    schema = load_json("schemas/ecs-provenance-manifest-v1.schema.json")
    manifest = load_json("fixtures/ecs/manifest.json")

    missing = copy.deepcopy(manifest)
    del missing["records"][0]["revision_reason"]
    require_ecs_provenance_rejected(
        missing, schema, "a missing ECS provenance field was not rejected"
    )

    unknown = copy.deepcopy(manifest)
    unknown["records"][0]["unexpected"] = True
    require_ecs_provenance_rejected(
        unknown, schema, "an unknown ECS provenance field was not rejected"
    )

    unknown_enum = copy.deepcopy(manifest)
    unknown_enum["records"][0]["provenance_class"] = "LegacyExact"
    require_ecs_provenance_rejected(
        unknown_enum, schema, "an unknown ECS provenance class was not rejected"
    )

    wrong_type = copy.deepcopy(manifest)
    wrong_type["records"][0]["input"]["bytes"] = True
    require_ecs_provenance_rejected(
        wrong_type, schema, "a wrong ECS provenance field type was not rejected"
    )

    tampered = copy.deepcopy(manifest)
    tampered["records"][0]["input"]["sha256"] = "0" * 64
    require_ecs_provenance_rejected(
        tampered,
        schema,
        "an ECS provenance artifact hash mismatch was not rejected",
    )

    input_reference = manifest["records"][0]["input"]
    input_data = (ECS_FIXTURE_ROOT / input_reference["path"]).read_bytes()
    require_artifact_payload_rejected(
        input_reference,
        input_data + b"tamper",
        "tampered ECS artifact bytes were not rejected",
    )

    duplicate = copy.deepcopy(manifest)
    duplicate["records"][1]["fixture_id"] = duplicate["records"][0]["fixture_id"]
    require_ecs_provenance_rejected(
        duplicate, schema, "a duplicate ECS fixture identity was not rejected"
    )

    conflicting_source = copy.deepcopy(manifest)
    conflicting_source["records"][1]["legacy_sources"][0][
        "source_blob_sha256"
    ] = "1" * 64
    require_ecs_provenance_rejected(
        conflicting_source,
        schema,
        "a conflicting legacy source identity was not rejected",
    )

    traversal = copy.deepcopy(manifest)
    traversal["records"][0]["input"]["path"] = "../outside.ecs"
    require_ecs_provenance_rejected(
        traversal, schema, "an ECS artifact path traversal was not rejected"
    )

    external = copy.deepcopy(manifest)
    external["records"][0]["input"]["path"] = "EasyCon/input.ecs"
    require_ecs_provenance_rejected(
        external, schema, "a non-SDK-local ECS dependency was not rejected"
    )

    missing_oracle = copy.deepcopy(manifest)
    missing_oracle["records"][8]["fixture_id"] = "corrected.label.missing"
    require_ecs_provenance_rejected(
        missing_oracle, schema, "an omitted corrected oracle was not rejected"
    )

    missing_production_source = copy.deepcopy(manifest)
    missing_production_source["records"][1]["legacy_sources"].pop()
    require_ecs_provenance_rejected(
        missing_production_source,
        schema,
        "a corrected production source oracle was not rejected when omitted",
    )

    shared_observed = copy.deepcopy(manifest)
    shared_observed["records"][3]["legacy_observed"] = copy.deepcopy(
        shared_observed["records"][2]["legacy_observed"]
    )
    require_ecs_provenance_rejected(
        shared_observed,
        schema,
        "WinForms-lite reused the WinForms legacy observed artifact",
    )


def validate_generated_ecs_fixtures(runner=subprocess.run):
    generator = ROOT / "tools" / "generate_ecs_provenance_fixtures.py"
    completed = runner(
        [sys.executable, str(generator), "--check"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    require(
        completed.returncode == 0,
        "ECS provenance fixture validation failed:\n{}".format(
            completed.stderr.strip() or completed.stdout.strip()
        ),
    )


def validate_ecs_fixture_validator_regression():
    class FailedResult:
        returncode = 1
        stdout = ""
        stderr = "synthetic generator failure"

    def failing_runner(*_args, **_kwargs):
        return FailedResult()

    try:
        validate_generated_ecs_fixtures(runner=failing_runner)
    except ValidationError:
        return
    raise ValidationError("ECS fixture validator accepted a failed generator")


def validate_ecs_provenance():
    validate_ecs_fixture_validator_regression()
    validate_generated_ecs_fixtures()
    schema = load_json("schemas/ecs-provenance-manifest-v1.schema.json")
    manifest = load_json("fixtures/ecs/manifest.json")
    validate_ecs_provenance_document(manifest, schema)
    validate_ecs_provenance_regressions()
    return len(manifest["records"])


def validate_behavior():
    behavior = load_json("behavior/runtime-controller-v1.json")
    expected_states = ["Pending", "Running", "Cancelling", "Succeeded", "Failed", "Cancelled"]
    require(behavior.get("schema_version") == 1, "behavior schema_version must be 1")
    require(behavior.get("license") == "GPL-3.0-only", "behavior license must remain GPL-3.0-only")
    require(
        behavior.get("milestone") == "phase-2a-controller-serial-candidate-v1",
        "behavior milestone must identify the Phase 2A candidate",
    )
    require(
        behavior["phase2a"]
        == {
            "status": "Hardware Unverified",
            "hardware_verified": False,
            "open_risks": ["O-01", "O-02", "O-04"],
            "excluded_scope": behavior["phase2a"]["excluded_scope"],
        },
        "Phase 2A status or open hardware risks changed",
    )
    require(behavior["operations"]["states"] == expected_states, "operation states changed")
    require(
        behavior["operations"]["terminal_states"] == ["Succeeded", "Failed", "Cancelled"],
        "operation terminal states changed",
    )
    require(
        behavior["operations"]["terminal_transaction"]["order"]
        == [
            "consume mutually exclusive settlement evidence and claim one winner",
            "seal child admission",
            "close and cancel the admitted cancellation subtree",
            "complete fallible owner cleanup outside operation and arbiter locks",
            "record cleanup settlement outside operation and arbiter locks",
            "commit one immutable terminal state and terminal event",
            "unlink the operation registry entry",
            "notify every waiter unconditionally",
        ],
        "operation terminal transaction order changed",
    )
    transitions = {(item["from"], item["to"]) for item in behavior["operations"]["transitions"]}
    required = {
        ("Pending", "Running"),
        ("Pending", "Cancelling"),
        ("Pending", "Failed"),
        ("Running", "Cancelling"),
        ("Running", "Succeeded"),
        ("Running", "Failed"),
        ("Cancelling", "Cancelled"),
    }
    require(transitions == required, "operation transition set changed")
    require(
        behavior["runtime"]["closed_counts"]
        == {"active_tasks": 0, "active_resources": 0, "active_operations": 0},
        "closed registry counts must all be zero",
    )
    require(
        behavior["runtime"]["states"]
        == ["Active", "Closing", "Closed", "CloseFailed"],
        "Runtime states changed",
    )
    expected_close_order = [
        "enter Closing and seal work plus deadline admission",
        "publish RuntimeClosing and cancel the root tree",
        "close resources in Runtime ID order while the deadline worker remains active",
        "join ordinary supervised and external owner tasks while the deadline worker remains active",
        "settle operations through their joined legitimate owner or one pre-held transferable handoff and preserve ownership loss",
        "resolve remaining deadlines as RuntimeClosed and join the internal deadline worker",
        "verify operation, resource, and active-task registries after internal worker join",
        "publish RuntimeClosed and close producers",
        "save the Closed outcome, enter Closed, and notify close waiters",
    ]
    require(behavior["runtime"]["close_order"] == expected_close_order,
            "Runtime close order changed")
    require(
        behavior["runtime"]["generic_deadlines"]["states"]
        == ["Armed", "Fired", "Disarmed", "RuntimeClosed"],
        "Runtime generic deadline states changed",
    )
    require(
        behavior["controller"]["default_minimum_report_interval_ns"] == 30_000_000,
        "controller interval must remain 30 ms",
    )
    require(
        behavior["controller"]["write_timeout_ns"] == 1_000_000_000,
        "controller write timeout must remain 1 s",
    )
    require(
        behavior["controller"]["serial"]["system_leaf"] is True,
        "serial must remain a system leaf",
    )
    amiibo = behavior["controller"]["amiibo"]
    require(
        {
            "chunk_size": amiibo["chunk_size"],
            "save_command": amiibo["save_command"],
            "select_command": amiibo["select_command"],
            "ack": amiibo["ack"],
            "generation_matched": amiibo["generation_matched"],
        }
        == {
            "chunk_size": 20,
            "save_command": 0x90,
            "select_command": 0x91,
            "ack": 0xFF,
            "generation_matched": True,
        },
        "Amiibo source-exact values or matcher contract changed",
    )
    require(
        behavior["controller"]["timing"]["stages"]
        == [
            "command_admitted",
            "lane_wake",
            "lane_dispatch",
            "transport_write_entered",
            "transport_accepted",
        ],
        "Controller timing stages changed",
    )
    require(
        behavior["controller"]["timing"]["eligible_direct_samples"] == 10_000
        and behavior["controller"]["timing"]["target_p99_ns"] == 1_000_000
        and behavior["controller"]["timing"]["target_max_ns"] == 5_000_000
        and behavior["controller"]["timing"]["outlier_filtering"] is False
        and behavior["controller"]["timing"]["permanent_busy_wait"] is False,
        "Phase 2A latency acceptance changed",
    )
    require(
        behavior["controller"]["sequence"]["maximum_steps"] == 10_000,
        "Controller sequence step ceiling changed",
    )
    classes = {item["classification"] for item in behavior["classifications"]}
    require(classes == {"source-exact", "corrected"}, "behavior classifications are incomplete")


def validate_runtime_r0_fixture():
    fixture = load_json("fixtures/runtime/r0-v2-contract-v1.json")
    deadline = fixture["deadline_registration"]
    require(
        deadline["terminal_resolutions"] == ["Fired", "Disarmed", "RuntimeClosed"],
        "Runtime deadline terminal resolutions changed",
    )
    require(
        deadline["same_target_registration_ids"] == deadline["same_target_fire_order"],
        "same-target Runtime deadlines must fire in registration-ID order",
    )
    require(
        deadline["same_target_registration_ids"]
        == sorted(set(deadline["same_target_registration_ids"])),
        "Runtime deadline fixture IDs must be unique and increasing",
    )

    evidence = {
        "Success": "EffectAccepted",
        "Failure": "ExecutionFailed",
        "Requested": "NotDelivered",
        "Deadline": "NotDelivered",
        "ParentClose": "NotDelivered",
    }
    terminal = {
        "Success": {"Ok": ("Succeeded", "None"),
                    "Err": ("Failed", "Cleanup"),
                    "Panic": ("Failed", "Cleanup")},
        "Failure": {cleanup: ("Failed", "Primary")
                    for cleanup in ["Ok", "Err", "Panic"]},
        "Requested": {cleanup: ("Cancelled", "FirstCancellationReason")
                      for cleanup in ["Ok", "Err", "Panic"]},
        "Deadline": {cleanup: ("Cancelled", "FirstCancellationReason")
                     for cleanup in ["Ok", "Err", "Panic"]},
        "ParentClose": {cleanup: ("Cancelled", "FirstCancellationReason")
                        for cleanup in ["Ok", "Err", "Panic"]},
    }
    expected_matrix = []
    for primary in ["Success", "Failure", "Requested", "Deadline", "ParentClose"]:
        for cleanup in ["Ok", "Err", "Panic"]:
            state, error_source = terminal[primary][cleanup]
            expected_matrix.append(
                {
                    "primary": primary,
                    "evidence": evidence[primary],
                    "cleanup": cleanup,
                    "transition_outcome": (
                        "Applied" if cleanup == "Ok" else "CleanupFailed"
                    ),
                    "terminal_state": state,
                    "error_source": error_source,
                }
            )
    require(
        fixture["terminal_cleanup_matrix"] == expected_matrix,
        "Runtime five-path cleanup projection matrix changed",
    )

    expected_close_cases = [
        {
            "id": "transferable-task-panic",
            "task_panicked": True,
            "preheld_transfer_owner": True,
            "shared_evidence": "NotDelivered",
            "deadline_resolution": "NotApplicable",
            "close_phase": "TaskJoin",
            "operation_state": "Cancelled",
            "active_operations": 0,
            "terminal_events": 1,
        },
        {
            "id": "exclusive-task-panic",
            "task_panicked": True,
            "preheld_transfer_owner": False,
            "shared_evidence": "None",
            "deadline_resolution": "NotApplicable",
            "close_phase": "TaskJoin",
            "operation_state": "Running",
            "active_operations": 1,
            "terminal_events": 0,
        },
        {
            "id": "exclusive-owner-exit",
            "task_panicked": False,
            "preheld_transfer_owner": False,
            "shared_evidence": "None",
            "deadline_resolution": "NotApplicable",
            "close_phase": "OperationFinalization",
            "operation_state": "Running",
            "active_operations": 1,
            "terminal_events": 0,
        },
        {
            "id": "resource-failure-deadline-owner",
            "task_panicked": False,
            "preheld_transfer_owner": False,
            "shared_evidence": "None",
            "deadline_resolution": "Fired",
            "close_phase": "ResourceCleanup",
            "operation_state": "NotApplicable",
            "active_operations": 0,
            "terminal_events": 0,
        },
    ]
    require(
        fixture["close_cases"] == expected_close_cases,
        "Runtime close ownership and deadline cases changed",
    )


def validate_controller_fixture():
    fixture = load_json("fixtures/controller/reports-v1.json")
    require(fixture.get("classification") == "source-exact", "controller fixture must be source-exact")
    require(fixture.get("license") == "GPL-3.0-only", "controller fixture license changed")
    expected_buttons = {
        "Y": 1,
        "B": 2,
        "A": 4,
        "X": 8,
        "L": 16,
        "R": 32,
        "ZL": 64,
        "ZR": 128,
        "MINUS": 256,
        "PLUS": 512,
        "LCLICK": 1024,
        "RCLICK": 2048,
        "HOME": 4096,
        "CAPTURE": 8192,
    }
    expected_hats = {
        "TOP": 0,
        "TOP_RIGHT": 1,
        "RIGHT": 2,
        "BOTTOM_RIGHT": 3,
        "BOTTOM": 4,
        "BOTTOM_LEFT": 5,
        "LEFT": 6,
        "TOP_LEFT": 7,
        "CENTER": 8,
    }
    require(fixture["buttons"] == expected_buttons, "source-exact button values changed")
    require(fixture["hats"] == expected_hats, "source-exact HAT values changed")
    require(fixture["sticks"] == {"minimum": 0, "center": 128, "maximum": 255}, "stick values changed")
    require(fixture["handshake"]["auto_baud_order"] == [115200, 9600], "baud fallback order changed")
    require(fixture["handshake"]["request"] == [165, 165, 129], "handshake request changed")
    require(fixture["handshake"]["success_reply"] == [128], "handshake reply changed")
    require(
        fixture["amiibo"]
        == {
            "save": {
                "ready": 0xA5,
                "command": 0x90,
                "chunk_size": 20,
                "header_example": [0xA5, 12, 1, 7, 0, 3, 0x90],
                "ack": 0xFF,
                "ack_timeout_ms": 1000,
            },
            "select": {
                "ready": 0xA5,
                "command": 0x91,
                "request_example": [0xA5, 3, 0x91],
                "ack": 0xFF,
                "ack_timeout_ms": 200,
            },
            "reset": {
                "request": [0xA5, 0x81, 0xA5, 0x81, 0xA5, 0x81],
                "reply": 0x80,
                "timeout_ms": 50,
            },
            "hardware_limits": {
                "slot_count": None,
                "maximum_data_len": None,
                "verified": False,
            },
        },
        "source-exact Amiibo fixture changed",
    )
    names = set()
    for report in fixture["reports"]:
        require(report["name"] not in names, "duplicate report name: {}".format(report["name"]))
        names.add(report["name"])
        encoded = report["encoded"]
        require(len(encoded) == 8, "{} must contain eight 7-bit chunks".format(report["name"]))
        require(all(value < 0x80 for value in encoded[:-1]), "{} has an early end flag".format(report["name"]))
        require(encoded[-1] & 0x80, "{} is missing its final end flag".format(report["name"]))
        require(encode_report(report["state"]) == encoded, "{} bytes do not encode its state".format(report["name"]))
    corrected = {item["id"] for item in fixture["corrected_behaviors"]}
    require(
        corrected == {"keystroke-future-time", "cancel-missing-release", "competing-ack-listeners"},
        "corrected controller behavior set changed",
    )


def validate_traces():
    trace_file = load_json("fixtures/controller/sequence-traces-v1.json")
    require(trace_file.get("clock") == "virtual-monotonic-nanoseconds", "sequence clock must be virtual")
    ids = set()
    for trace in trace_file["traces"]:
        require(trace["id"] not in ids, "duplicate trace id: {}".format(trace["id"]))
        ids.add(trace["id"])
        offsets = [step["offset_ns"] for step in trace["steps"]]
        require(offsets == sorted(offsets), "{} offsets are not monotonic".format(trace["id"]))
        timestamps = [report["timestamp_ns"] for report in trace["expected_reports"]]
        require(timestamps == sorted(timestamps), "{} dispatch timestamps are not monotonic".format(trace["id"]))
        require(all(len(report["bytes"]) == 8 for report in trace["expected_reports"]), "trace report length changed")
        require(
            all(timestamp >= trace["lane_start_ns"] for timestamp in timestamps),
            "{} dispatches before its lane start".format(trace["id"]),
        )
    cancel = next(item for item in trace_file["traces"] if item["id"] == "cancel-before-future-step-neutralizes")
    require(cancel["operation_states"][-2:] == ["Cancelling", "Cancelled"], "cancel trace commits too early")
    require(cancel["expected_reports"][-1].get("purpose") == "neutralize", "cancel trace lacks neutralization")


def validate_latency_result():
    result = load_json("fixtures/controller/phase2a-latency-result-v1.json")
    require(result["status"] == "Hardware Unverified", "latency result lost Hardware Unverified status")
    harness = result["harness"]
    require(harness["eligible_samples"] >= 10_000, "latency sample population is too small")
    require(
        harness["raw_csv_rows"] == harness["eligible_samples"] + 1,
        "latency CSV row count must include one header and every eligible sample",
    )
    require(
        re.fullmatch(r"[0-9A-F]{64}", harness["raw_csv_sha256"]) is not None,
        "latency CSV SHA-256 is malformed",
    )
    for name, metric in result["metrics"].items():
        require(
            metric["p50"] <= metric["p95"] <= metric["p99"] <= metric["max"],
            "{} percentiles are not monotonic".format(name),
        )
    target = result["target"]
    measured = result["metrics"][target["metric"]]
    computed_pass = measured["p99"] <= target["p99_ns"] and measured["max"] <= target["max_ns"]
    require(target["passed"] is computed_pass, "latency pass result does not match measured values")
    require(
        "not UART" in result["scope"] and "Switch" in result["scope"],
        "latency result must retain its software-only scope",
    )


CONFORMANCE_MARKER = re.compile(
    r"^[ \t]*// conformance: ([a-z0-9][a-z0-9.-]*)[ \t]*$", re.MULTILINE
)
CONFORMANCE_TEST_BLOCK = re.compile(
    r"(?P<markers>(?:^[ \t]*// conformance: [a-z0-9][a-z0-9.-]*[ \t]*\r?\n)+)"
    r"^[ \t]*#\[test\][ \t]*\r?\n"
    r"^[ \t]*fn[ \t]+(?P<test>[A-Za-z_][A-Za-z0-9_]*)[ \t]*\(",
    re.MULTILINE,
)
TEST_RESULT = re.compile(
    r"^test result: (?P<status>ok|FAILED)\. "
    r"(?P<passed>[0-9]+) passed; (?P<failed>[0-9]+) failed; "
    r"(?P<ignored>[0-9]+) ignored; (?P<measured>[0-9]+) measured; "
    r"(?P<filtered>[0-9]+) filtered out(?:;.*)?$",
    re.MULTILINE,
)


def conformance_test_markers():
    markers = {}
    for source_root in (ROOT / "crates", ROOT / "tests"):
        for path in sorted(source_root.rglob("*.rs")):
            text = path.read_text(encoding="utf-8")
            raw_ids = CONFORMANCE_MARKER.findall(text)
            captured_ids = []
            relative = path.relative_to(ROOT).as_posix()
            for match in CONFORMANCE_TEST_BLOCK.finditer(text):
                test_ref = "{}::{}".format(relative, match.group("test"))
                for assertion_id in CONFORMANCE_MARKER.findall(match.group("markers")):
                    require(
                        assertion_id not in markers,
                        "duplicate conformance marker: {}".format(assertion_id),
                    )
                    markers[assertion_id] = test_ref
                    captured_ids.append(assertion_id)
            require(
                sorted(raw_ids) == sorted(captured_ids),
                "{} has conformance markers not attached to a Rust #[test]".format(relative),
            )
    return markers


def cargo_executable():
    cargo = shutil.which("cargo")
    if cargo:
        return cargo
    fallback = Path.home() / ".cargo" / ("cargo.exe" if os.name == "nt" else "cargo")
    if fallback.is_file():
        return str(fallback)
    raise ValidationError("cargo was not found in PATH or $HOME/.cargo/bin")


def cargo_metadata():
    command = [
        cargo_executable(),
        "metadata",
        "--no-deps",
        "--format-version",
        "1",
    ]
    try:
        result = subprocess.run(
            command,
            cwd=str(ROOT),
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            errors="replace",
        )
    except OSError as error:
        raise ValidationError("could not read Cargo metadata: {}".format(error))
    require(
        result.returncode == 0,
        "cargo metadata failed: {}".format(result.stderr.strip()),
    )
    try:
        metadata = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise ValidationError("cargo metadata returned invalid JSON: {}".format(error))
    require(metadata.get("version") == 1, "cargo metadata format version changed")
    return metadata


def executable_test_name(test_ref):
    source, function = test_ref.rsplit("::", 1)
    if "/src/" not in source:
        return function
    module = source.split("/src/", 1)[1]
    require(module.endswith(".rs"), "invalid Rust source test mapping: {}".format(test_ref))
    module = module[:-3]
    if module == "lib":
        return "tests::{}".format(function)
    if module.endswith("/mod"):
        module = module[:-len("/mod")]
    return "{}::tests::{}".format(module.replace("/", "::"), function)


def path_relative_to(path, parent):
    try:
        return path.relative_to(parent)
    except ValueError:
        return None


def workspace_test_targets(metadata):
    workspace_members = set(metadata.get("workspace_members", []))
    targets = []
    for package in metadata.get("packages", []):
        if package.get("id") not in workspace_members:
            continue
        for target in package.get("targets", []):
            kinds = set(target.get("kind", []))
            if "lib" in kinds:
                target_kind = "lib"
            elif "test" in kinds:
                target_kind = "test"
            else:
                continue
            targets.append(
                {
                    "package": package["name"],
                    "target_kind": target_kind,
                    "target_name": target["name"],
                    "source": Path(target["src_path"]).resolve(),
                }
            )
    require(targets, "cargo metadata contains no workspace Rust test targets")
    return targets


def target_owns_source(target, source):
    if target["target_kind"] == "test":
        return source == target["source"]
    return path_relative_to(source, target["source"].parent) is not None


def conformance_test_targets(markers, metadata):
    cargo_targets = workspace_test_targets(metadata)
    resolved = []
    for test_ref in sorted(set(markers.values())):
        source, _ = test_ref.rsplit("::", 1)
        source_path = (ROOT / source).resolve()
        candidates = [
            target for target in cargo_targets
            if target_owns_source(target, source_path)
        ]
        require(
            len(candidates) == 1,
            "{} must map to exactly one Cargo package/target, found {!r}".format(
                test_ref,
                [
                    (target["package"], target["target_kind"], target["target_name"])
                    for target in candidates
                ],
            ),
        )
        test = dict(candidates[0])
        test["test_ref"] = test_ref
        test["executable"] = executable_test_name(test_ref)
        resolved.append(test)
    return resolved


def exact_test_command(test, cargo=None):
    command = [
        cargo or cargo_executable(),
        "test",
        "--color",
        "never",
        "-p",
        test["package"],
        "--all-features",
    ]
    if test["target_kind"] == "lib":
        command.append("--lib")
    else:
        command.extend(["--test", test["target_name"]])
    command.extend(
        [test["executable"], "--", "--exact", "--format", "terse"]
    )
    return command


def test_identity(test):
    return "{}/{}:{}::{}".format(
        test["package"],
        test["target_kind"],
        test["target_name"],
        test["executable"],
    )


def validate_exact_test_result(test, result):
    identity = test_identity(test)
    require(
        result.returncode == 0,
        "conformance test {} failed:\n{}\n{}".format(
            identity, result.stdout.strip(), result.stderr.strip()
        ).rstrip(),
    )
    summaries = list(TEST_RESULT.finditer(result.stdout))
    require(
        len(summaries) == 1,
        "conformance test {} returned no unique libtest summary: {!r}".format(
            identity, result.stdout.strip()
        ),
    )
    summary = summaries[0]
    counts = {
        name: int(summary.group(name))
        for name in ("passed", "failed", "ignored", "measured")
    }
    require(
        summary.group("status") == "ok"
        and counts == {"passed": 1, "failed": 0, "ignored": 0, "measured": 0},
        "conformance test {} must execute exactly once and pass without being ignored; "
        "observed status={} counts={!r}".format(
            identity, summary.group("status"), counts
        ),
    )


def execute_conformance_tests(tests, cargo=None, runner=subprocess.run):
    for test in tests:
        command = exact_test_command(test, cargo=cargo)
        try:
            result = runner(
                command,
                cwd=str(ROOT),
                check=False,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                encoding="utf-8",
                errors="replace",
            )
        except OSError as error:
            raise ValidationError(
                "could not execute conformance test {}: {}".format(
                    test_identity(test), error
                )
            )
        validate_exact_test_result(test, result)


def require_conformance_execution_rejected(tests, runner, message):
    try:
        execute_conformance_tests(tests, cargo="cargo", runner=runner)
    except ValidationError:
        return
    raise ValidationError(message)


def synthetic_test_result(command, passed=0, ignored=0):
    stdout = (
        "running {} test\n\n"
        "test result: ok. {} passed; 0 failed; {} ignored; 0 measured; "
        "0 filtered out; finished in 0.00s\n"
    ).format(passed + ignored, passed, ignored)
    return subprocess.CompletedProcess(command, 0, stdout, "")


def validate_conformance_execution_regressions():
    unit = {
        "package": "package-a",
        "target_kind": "lib",
        "target_name": "package_a",
        "test_ref": "package-a/src/lib.rs::same_name",
        "executable": "tests::same_name",
    }
    integration = {
        "package": "package-b",
        "target_kind": "test",
        "target_name": "collision",
        "test_ref": "package-b/tests/collision.rs::same_name",
        "executable": "tests::same_name",
    }
    require(
        exact_test_command(unit, cargo="cargo")
        != exact_test_command(integration, cargo="cargo"),
        "same-name tests lost their Cargo package/target identity",
    )

    def collision_runner(command, **_kwargs):
        if "--lib" in command:
            return synthetic_test_result(command)
        return synthetic_test_result(command, passed=1)

    require_conformance_execution_rejected(
        [unit, integration],
        collision_runner,
        "a same-name test in the wrong Cargo target satisfied conformance",
    )

    def ignored_runner(command, **_kwargs):
        return synthetic_test_result(command, ignored=1)

    require_conformance_execution_rejected(
        [unit], ignored_runner, "an ignored conformance test was accepted"
    )


def validate_conformance_document(conformance, markers):
    require(conformance.get("schema_version") == 1, "conformance schema_version must be 1")
    require(conformance.get("license") == "GPL-3.0-only", "conformance license changed")
    scenario_ids = [scenario["id"] for scenario in conformance["scenarios"]]
    required = [
        "vertical-slice",
        "runtime-stabilization",
        "operation-terminal-transaction",
        "timeout-separation",
        "transport-faults",
        "event-overflow",
        "phase2a-serial",
        "phase2a-amiibo",
        "phase2a-acceptance",
    ]
    require(scenario_ids == required, "conformance scenarios changed or were reordered")
    require(len(scenario_ids) == len(set(scenario_ids)), "duplicate conformance scenario ID")
    scenario_tests = [scenario["test"] for scenario in conformance["scenarios"]]
    require(
        len(scenario_tests) == len(set(scenario_tests)),
        "duplicate conformance scenario test mapping",
    )
    known_tests = set(markers.values())
    step_ids = set()
    assertion_ids = set()
    for scenario in conformance["scenarios"]:
        require(scenario["steps"], "{} has no steps".format(scenario["id"]))
        require(scenario["assertions"], "{} has no assertions".format(scenario["id"]))
        require(scenario["tests"], "{} has no executable test suite".format(scenario["id"]))
        require(
            len(scenario["tests"]) == len(set(scenario["tests"])),
            "{} has duplicate executable tests".format(scenario["id"]),
        )
        require(
            scenario["test"] in known_tests,
            "{} maps to a missing or unmarked Rust test: {}".format(
                scenario["id"], scenario["test"]
            ),
        )
        require(
            scenario["test"] in scenario["tests"],
            "{} primary test is absent from its executable suite".format(scenario["id"]),
        )
        for test_ref in scenario["tests"]:
            require(
                test_ref in known_tests,
                "{} suite maps to a missing or unmarked Rust test: {}".format(
                    scenario["id"], test_ref
                ),
            )
        for step in scenario["steps"]:
            require(step["id"], "{} has an empty step ID".format(scenario["id"]))
            require(step["action"], "{} has an empty action".format(step["id"]))
            require(step["id"] not in step_ids,
                    "duplicate conformance step ID: {}".format(step["id"]))
            step_ids.add(step["id"])
        expected_scenario_tests = []
        for assertion in scenario["assertions"]:
            assertion_id = assertion["id"]
            require(assertion_id, "{} has an empty assertion ID".format(scenario["id"]))
            require(assertion["expect"], "{} has an empty expectation".format(assertion_id))
            require(
                assertion_id not in assertion_ids,
                "duplicate conformance assertion ID: {}".format(assertion_id),
            )
            assertion_ids.add(assertion_id)
            require(
                markers.get(assertion_id) == assertion["test"],
                "{} has a missing, stale, or invalid Rust test mapping: {}".format(
                    assertion_id, assertion["test"]
                ),
            )
            if assertion["test"] not in expected_scenario_tests:
                expected_scenario_tests.append(assertion["test"])
        require(
            scenario["tests"] == expected_scenario_tests,
            "{} executable suite does not exactly cover its assertions".format(scenario["id"]),
        )
    require(
        assertion_ids == set(markers),
        "conformance assertion and Rust marker sets differ: spec_only={!r}, rust_only={!r}".format(
            sorted(assertion_ids - set(markers)), sorted(set(markers) - assertion_ids)
        ),
    )


def require_conformance_rejected(conformance, markers, message):
    try:
        validate_conformance_document(conformance, markers)
    except ValidationError:
        return
    raise ValidationError(message)


def validate_conformance_regressions(conformance, markers):
    duplicate = copy.deepcopy(conformance)
    duplicate["scenarios"][0]["assertions"].append(
        copy.deepcopy(duplicate["scenarios"][0]["assertions"][0])
    )
    require_conformance_rejected(
        duplicate, markers, "duplicate conformance assertion was not rejected"
    )

    missing = copy.deepcopy(conformance)
    missing["scenarios"][0]["assertions"][0]["test"] = (
        "tests/support/tests/missing.rs::missing_test"
    )
    require_conformance_rejected(
        missing, markers, "missing conformance test mapping was not rejected"
    )

    stale_markers = dict(markers)
    stale_markers["stale.assertion"] = next(iter(markers.values()))
    require_conformance_rejected(
        conformance, stale_markers, "stale Rust conformance marker was not rejected"
    )

    incomplete_suite = copy.deepcopy(conformance)
    incomplete_suite["scenarios"][0]["tests"].pop()
    require_conformance_rejected(
        incomplete_suite, markers, "incomplete conformance scenario suite was not rejected"
    )


def validate_conformance():
    conformance = load_json("conformance/runtime-controller-v1.json")
    markers = conformance_test_markers()
    validate_conformance_document(conformance, markers)
    validate_conformance_regressions(conformance, markers)
    tests = conformance_test_targets(markers, cargo_metadata())
    validate_conformance_execution_regressions()
    execute_conformance_tests(tests)
    return len(tests)


def validate_generated_vision_fixtures(generator_name, label,
                                       runner=subprocess.run):
    generator = ROOT / "tools" / generator_name
    completed = runner(
        [sys.executable, str(generator), "--check"],
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    require(
        completed.returncode == 0,
        "{} fixture validation failed:\n{}".format(
            label, completed.stderr.strip() or completed.stdout.strip()
        ),
    )


def validate_vision_codec_fixtures(runner=subprocess.run):
    validate_generated_vision_fixtures(
        "generate_vision_codec_fixtures.py", "vision codec", runner
    )


def validate_vision_operation_fixtures(runner=subprocess.run):
    validate_generated_vision_fixtures(
        "generate_vision_operation_fixtures.py", "vision operation", runner
    )


def validate_vision_ocr_fixtures(runner=subprocess.run):
    validate_generated_vision_fixtures(
        "generate_vision_ocr_fixtures.py", "vision OCR", runner
    )


def validate_vision_label_fixtures(runner=subprocess.run):
    validate_generated_vision_fixtures(
        "generate_vision_label_fixtures.py", "vision label", runner
    )


def validate_vision_capture_fixtures(runner=subprocess.run):
    validate_generated_vision_fixtures(
        "generate_vision_capture_fixtures.py", "vision capture", runner
    )
    manifest = load_json("fixtures/vision/capture/manifest.json")
    require(manifest.get("version") == 1, "capture fixture version changed")
    require(manifest.get("license") == "GPL-3.0-only", "capture fixture license changed")
    require(manifest.get("hardware_support") == [], "capture hardware support must remain empty")
    require(
        manifest.get("native_admission")
        == {
            "file": "unsupported-before-path-access",
            "directshow": "unsupported-before-device-access",
            "media_foundation": "unsupported-before-device-access",
        },
        "unqualified native capture admission changed",
    )
    sequence = manifest.get("sequence")
    require(
        sequence
        == {
            "pattern": "frame-%02d.bmp",
            "frame_count": 2,
            "source_fixture": "../codec/bgr-2x2.bmp.hex",
            "encoded_bytes_per_frame": 70,
            "encoded_sha256": (
                "32595ac4ac54ae42c4f31d77fce001599dc10f5452f7c2de5482f0ed5f0a074d"
            ),
            "width": 2,
            "height": 2,
            "stride": 6,
            "pixel_format": "BGR8",
            "decoded_sha256": (
                "3d335acc3b7c9d3edcf42098e24ad875a2c0ba87223c640b1d535be39677009c"
            ),
        },
        "capture sequence contract changed",
    )
    encoded = bytes.fromhex(
        (SPEC / "fixtures/vision/codec/bgr-2x2.bmp.hex").read_text(encoding="ascii")
    )
    require(len(encoded) == sequence["encoded_bytes_per_frame"], "capture BMP size changed")
    require(
        hashlib.sha256(encoded).hexdigest() == sequence["encoded_sha256"],
        "capture BMP hash changed",
    )


def validate_vision_model_provisioner_regressions():
    import provision_vision_test_model as model

    manifest = model.load_manifest(model.EXPECTED_MANIFEST)
    attempted_downloads = []
    original_download = model.download
    model.download = lambda entry, destination: attempted_downloads.append(destination)
    try:
        try:
            model.provision(manifest, ROOT)
        except model.ProvisionError:
            pass
    finally:
        model.download = original_download
    require(
        not attempted_downloads,
        "OCR provisioner must reject output outside ignored .tools/vision-models before writing",
    )

    with tempfile.TemporaryDirectory(prefix="easycon-ocr-contract-") as temporary:
        controlled_root = Path(temporary) / "controlled"
        model.download = lambda entry, destination: attempted_downloads.append(destination)
        try:
            attempted_downloads.clear()
            try:
                model.provision(
                    manifest,
                    controlled_root / "model",
                    allowed_root=controlled_root,
                )
            except model.ProvisionError:
                pass
            require(
                attempted_downloads,
                "OCR provisioner must accept an explicit controlled root before downloading",
            )

            attempted_downloads.clear()
            try:
                model.provision(
                    manifest,
                    Path(temporary) / "outside",
                    allowed_root=controlled_root,
                )
            except model.ProvisionError:
                pass
            require(
                not attempted_downloads,
                "OCR provisioner must reject output escaping an explicit controlled root",
            )
        finally:
            model.download = original_download

    require(
        hasattr(model, "FrozenRedirectHandler"),
        "OCR provisioner must validate every HTTP redirect hop",
    )
    handler = model.FrozenRedirectHandler()
    request = model.urllib.request.Request(model.FROZEN_MODEL["url"])
    try:
        handler.redirect_request(
            request,
            None,
            302,
            "Found",
            {},
            "https://example.com/intermediate",
        )
    except model.ProvisionError:
        pass
    else:
        require(False, "OCR provisioner accepted an off-host intermediate redirect")


def validate_vision_ocr_model_manifest():
    manifest = load_json("fixtures/vision/ocr-model.json")
    require(manifest.get("version") == 1, "OCR model manifest version must be 1")
    require(
        manifest.get("model")
        == {
            "language": "eng",
            "path": "eng.traineddata",
            "url": (
                "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/"
                "refs/tags/4.1.0/eng.traineddata"
            ),
            "bytes": 4113088,
            "sha256": (
                "7d4322bd2a7749724879683fc3912cb542f19906c83bcc1a52132556427170b2"
            ),
        },
        "OCR test model source, size, or hash changed",
    )
    require(
        manifest.get("license")
        == {
            "spdx": "Apache-2.0",
            "path": "LICENSE",
            "url": (
                "https://raw.githubusercontent.com/tesseract-ocr/tessdata_fast/"
                "refs/tags/4.1.0/LICENSE"
            ),
            "bytes": 11358,
            "sha256": (
                "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30"
            ),
        },
        "OCR test model license, size, or hash changed",
    )


def validate_vision_fixture_validator_regressions():
    class FailedResult:
        returncode = 1
        stdout = ""
        stderr = "synthetic generator failure"

    def failing_runner(*_args, **_kwargs):
        return FailedResult()

    for validator, label in (
        (validate_vision_codec_fixtures, "codec"),
        (validate_vision_operation_fixtures, "operation"),
        (validate_vision_ocr_fixtures, "OCR"),
        (validate_vision_label_fixtures, "label"),
        (validate_vision_capture_fixtures, "capture"),
    ):
        try:
            validator(runner=failing_runner)
        except ValidationError:
            continue
        raise ValidationError(
            "{} fixture validator accepted a failed generator".format(label)
        )


def main():
    validate_schemas()
    validate_validator_regressions()
    ecs_record_count = validate_ecs_provenance()
    validate_behavior()
    validate_runtime_r0_fixture()
    validate_controller_fixture()
    validate_traces()
    validate_latency_result()
    validate_vision_fixture_validator_regressions()
    validate_vision_ocr_model_manifest()
    validate_vision_codec_fixtures()
    validate_vision_operation_fixtures()
    validate_vision_ocr_fixtures()
    validate_vision_label_fixtures()
    validate_vision_capture_fixtures()
    validate_vision_model_provisioner_regressions()
    test_count = validate_conformance()
    print(
        "validated 7 schemas, 1 behavior spec, 1 Runtime fixture, 3 controller fixtures, "
        "15 vision binary fixtures, 1 capture manifest, 24 label corpus entries, "
        "{} ECS provenance records with 33 SDK-local artifacts, "
        "9 conformance scenarios, and {} exact Rust tests".format(
            ecs_record_count, test_count
        )
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ValidationError as error:
        print("spec validation failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
