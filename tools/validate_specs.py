#!/usr/bin/env python3
"""Validate the milestone JSON schemas and fixtures without third-party packages."""

import json
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


def validate_conformance():
    conformance = load_json("conformance/runtime-controller-v1.json")
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
    ]
    require(scenario_ids == required, "conformance scenarios changed or were reordered")
    for scenario in conformance["scenarios"]:
        require(scenario["steps"], "{} has no steps".format(scenario["id"]))
        require(scenario["assertions"], "{} has no assertions".format(scenario["id"]))


def main():
    validate_schemas()
    validate_validator_regressions()
    validate_behavior()
    validate_controller_fixture()
    validate_traces()
    validate_conformance()
    print("validated 4 schemas, 1 behavior spec, 2 controller fixtures, and 6 conformance scenarios")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ValidationError as error:
        print("spec validation failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
