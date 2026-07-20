#!/usr/bin/env python3
"""Validate the milestone JSON schemas and fixtures without third-party packages."""

import copy
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SPEC = ROOT / "spec"


class ValidationError(Exception):
    """Raised when a tracked specification violates the milestone contract."""


def load_json(relative_path):
    path = SPEC / relative_path
    try:
        with path.open("r", encoding="utf-8") as stream:
            return json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        raise ValidationError("{}: {}".format(path, error))


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
        ("schemas/controller-fixture-v1.schema.json", "fixtures/controller/reports-v1.json"),
        ("schemas/conformance-v1.schema.json", "conformance/runtime-controller-v1.json"),
        ("schemas/sequence-trace-v1.schema.json", "fixtures/controller/sequence-traces-v1.json"),
        ("schemas/latency-result-v1.schema.json", "fixtures/controller/phase2a-latency-result-v1.json"),
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


def validate_validator_regressions():
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
            "seal child admission",
            "close and cancel the admitted cancellation subtree",
            "complete owner cleanup",
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
        "enter Closing and reject admission",
        "publish RuntimeClosing and cancel the root tree",
        "close resources in Runtime ID order while isolating each failure",
        "wait for owner cleanup and join ordinary supervised tasks",
        "finish non-terminal operations only after their owners exit",
        "join internal workers and every retained task handle",
        "verify operation, resource, active-task, and join registries are empty",
        "publish RuntimeClosed and close producers",
        "save the Closed outcome, enter Closed, and notify close waiters",
    ]
    require(behavior["runtime"]["close_order"] == expected_close_order,
            "Runtime close order changed")
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


def main():
    validate_schemas()
    validate_validator_regressions()
    validate_behavior()
    validate_controller_fixture()
    validate_traces()
    validate_latency_result()
    test_count = validate_conformance()
    print(
        "validated 5 schemas, 1 behavior spec, 3 controller fixtures, "
        "9 conformance scenarios, and {} exact Rust tests".format(test_count)
    )
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ValidationError as error:
        print("spec validation failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
