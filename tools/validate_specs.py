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
    names = [
        "schemas/behavior-v1.schema.json",
        "schemas/controller-fixture-v1.schema.json",
        "schemas/conformance-v1.schema.json",
    ]
    for name in names:
        schema = load_json(name)
        require(
            schema.get("$schema") == "https://json-schema.org/draft/2020-12/schema",
            "{} does not declare JSON Schema Draft 2020-12".format(name),
        )
        require(schema.get("$id", "").endswith(Path(name).name), "{} has an invalid $id".format(name))
        require(schema.get("type") == "object", "{} must validate an object".format(name))


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
        behavior["controller"]["default_minimum_report_interval_ns"] == 30_000_000,
        "controller interval must remain 30 ms",
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
    required = ["vertical-slice", "timeout-separation", "transport-faults", "event-overflow"]
    require(scenario_ids == required, "conformance scenarios changed or were reordered")
    for scenario in conformance["scenarios"]:
        require(scenario["steps"], "{} has no steps".format(scenario["id"]))
        require(scenario["assertions"], "{} has no assertions".format(scenario["id"]))


def main():
    validate_schemas()
    validate_behavior()
    validate_controller_fixture()
    validate_traces()
    validate_conformance()
    print("validated 3 schemas, 1 behavior spec, 2 controller fixtures, and 4 conformance scenarios")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except ValidationError as error:
        print("spec validation failed: {}".format(error), file=sys.stderr)
        sys.exit(1)
