#![forbid(unsafe_code)]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use easycon_controller::{
    AckFrame, AckRequest, AmiiboLimits, AmiiboSaveOptions, AmiiboSelectOptions, ConnectOptions,
    ControllerAction, ControllerOptions, ControllerSession, ControllerTransport, HandshakeRequest,
    PreciseSequence, SequenceStep, TransportError, WriteKind, WriteRequest,
};
use easycon_hardware_qualification::{distribution, option_value, required_value, value_or};
use easycon_model::{Button, Hat, StickPosition};
use easycon_runtime::{
    Clock, CloseOutcome, CloseRejection, Operation, OperationState, Runtime, RuntimeCounts,
    SystemClock, WaitResult, WaitTimeout,
};
use easycon_serial::{
    SerialControllerTransport, SerialPortDescriptor, WindowsByteIoFactory, discover_system_ports,
};
use serde_json::{Value, json};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const MINIMUM_REPORT_INTERVAL_NS: u64 = 30_000_000;

#[derive(Clone, Debug)]
struct HandshakeAttempt {
    baud: u32,
    succeeded: bool,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct TimingSample {
    sequence: u64,
    command_admitted_ns: Option<u64>,
    lane_wake_ns: Option<u64>,
    dispatch_ns: u64,
    write_entered_ns: u64,
    transport_accepted_ns: u64,
}

#[derive(Default)]
struct Telemetry {
    actual_baud: Option<u32>,
    handshake_attempts: Vec<HandshakeAttempt>,
    timings: Vec<TimingSample>,
}

struct ObservedTransport {
    inner: SerialControllerTransport,
    clock: Arc<dyn Clock>,
    telemetry: Arc<Mutex<Telemetry>>,
}

impl ControllerTransport for ObservedTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        let baud = request.baud_rate;
        let result = self.inner.handshake(request);
        let mut telemetry = self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if result.is_ok() {
            telemetry.actual_baud = Some(baud);
        }
        telemetry.handshake_attempts.push(HandshakeAttempt {
            baud,
            succeeded: result.is_ok(),
            error: result.as_ref().err().map(ToString::to_string),
        });
        result
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        let context = request.context;
        let remaining = request.bytes.len();
        let entered = self.clock.now_ns();
        let result = self.inner.write(request);
        let accepted_at = self.clock.now_ns();
        if let Ok(written) = result
            && context.kind == WriteKind::Report
            && context
                .total_len
                .saturating_sub(remaining)
                .saturating_add(written)
                == context.total_len
        {
            let (command_admitted_ns, lane_wake_ns) =
                context.direct_timing.map_or((None, None), |timing| {
                    (Some(timing.command_admitted_ns), Some(timing.lane_wake_ns))
                });
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .timings
                .push(TimingSample {
                    sequence: context.sequence,
                    command_admitted_ns,
                    lane_wake_ns,
                    dispatch_ns: context.timestamp_ns,
                    write_entered_ns: entered,
                    transport_accepted_ns: accepted_at,
                });
        }
        result
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        self.inner.wait_for_ack(request)
    }

    fn close(&mut self) {
        self.inner.close();
    }
}

struct Harness {
    runtime: Runtime,
    clock: Arc<dyn Clock>,
    controller: ControllerSession,
    telemetry: Arc<Mutex<Telemetry>>,
    descriptor: SerialPortDescriptor,
}

struct ArtifactReservation {
    output: PathBuf,
    marker: PathBuf,
    temporary: PathBuf,
}

impl ArtifactReservation {
    fn begin(command: &str, artifact_dir: &Path) -> Result<Self, String> {
        if command.is_empty()
            || !command
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'-')
        {
            return Err("command is not safe for an artifact file name".to_owned());
        }

        let output = artifact_dir.join(format!("{command}.json"));
        let temporary = output.with_extension("json.tmp");
        let marker = artifact_dir.join(format!(".{command}.in-progress.json"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
            .map_err(|error| format!("cannot reserve {}: {error}", marker.display()))?;
        let marker_document = json!({
            "command": command,
            "execution_status": "running",
        });
        file.write_all(
            &serde_json::to_vec_pretty(&marker_document).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        file.flush().map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);

        let validation = (|| {
            ensure_artifact_absent(&output)?;
            ensure_artifact_absent(&temporary)?;
            if command == "sequence" {
                ensure_artifact_absent(&artifact_dir.join("sequence-timings.csv"))?;
            }
            Ok(())
        })();
        if let Err(error) = validation {
            return match fs::remove_file(&marker) {
                Ok(()) => Err(error),
                Err(cleanup) => Err(format!(
                    "{error}; cannot remove owned reservation {}: {cleanup}",
                    marker.display()
                )),
            };
        }

        Ok(Self {
            output,
            marker,
            temporary,
        })
    }

    fn commit(self, document: &Value) -> Result<PathBuf, String> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&self.temporary)
            .map_err(|error| format!("cannot create {}: {error}", self.temporary.display()))?;
        let write_result = (|| {
            file.write_all(
                &serde_json::to_vec_pretty(document).map_err(|error| error.to_string())?,
            )
            .map_err(|error| error.to_string())?;
            file.flush().map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())
        })();
        drop(file);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&self.temporary);
            return Err(error);
        }

        if let Err(error) = fs::hard_link(&self.temporary, &self.output) {
            let _ = fs::remove_file(&self.temporary);
            return Err(format!(
                "cannot publish {} as {} without replacement: {error}",
                self.temporary.display(),
                self.output.display()
            ));
        }
        fs::remove_file(&self.temporary).map_err(|error| {
            format!(
                "cannot remove linked temporary artifact {}: {error}",
                self.temporary.display()
            )
        })?;
        fs::remove_file(&self.marker)
            .map_err(|error| format!("cannot remove {}: {error}", self.marker.display()))?;
        Ok(self.output)
    }
}

fn ensure_artifact_absent(path: &Path) -> Result<(), String> {
    if path.exists() {
        Err(format!(
            "refusing to overwrite existing qualification artifact {}",
            path.display()
        ))
    } else {
        Ok(())
    }
}

impl Harness {
    fn new(port_name: &str, options: ControllerOptions) -> Result<Self, String> {
        let descriptor = find_port(port_name)?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let serial = SerialControllerTransport::new(
            clock.clone(),
            descriptor.clone(),
            Box::new(WindowsByteIoFactory),
        );
        let observed = ObservedTransport {
            inner: serial,
            clock: clock.clone(),
            telemetry: telemetry.clone(),
        };
        let runtime = Runtime::new(clock.clone());
        let controller = ControllerSession::new(&runtime, Box::new(observed), options)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            runtime,
            clock,
            controller,
            telemetry,
            descriptor,
        })
    }

    fn connect(&self, options: ConnectOptions) -> Result<Value, String> {
        let operation = self
            .controller
            .connect(options)
            .map_err(|error| error.to_string())?;
        let snapshot = wait_terminal(&operation, OPERATION_TIMEOUT)?;
        if snapshot.state != OperationState::Succeeded {
            return Err(operation_failure(&operation));
        }
        Ok(operation_json(&operation))
    }

    fn close(&self) -> Value {
        self.controller.close();
        runtime_close_json(self.runtime.close(), self.runtime.counts())
    }

    fn actual_baud(&self) -> Option<u32> {
        self.telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .actual_baud
    }

    fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }
}

fn main() {
    let exit_code = match real_main() {
        Ok(exit_code) => exit_code,
        Err(error) => {
            eprintln!("hardware qualification failed: {error}");
            1
        }
    };
    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn real_main() -> Result<i32, String> {
    if !cfg!(windows) {
        return Err("the hardware qualification CLI requires Windows".to_owned());
    }
    let arguments: Vec<String> = env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_help();
        return Ok(0);
    };
    if matches!(command, "--help" | "-h" | "help") {
        print_help();
        return Ok(0);
    }
    let artifact_dir = artifact_dir(&arguments)?;
    fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let reservation = ArtifactReservation::begin(command, &artifact_dir)?;
    let execution = match command {
        "discover" => run_discover(&arguments),
        "handshake" => run_handshake(&arguments),
        "smoke" => run_smoke(&arguments),
        "home-wake" => run_home_wake(&arguments),
        "faults" => run_faults(&arguments),
        "hotplug" => run_hotplug(&arguments),
        "lifecycle" => run_lifecycle(&arguments),
        "sequence" => run_sequence(&arguments, &artifact_dir),
        "amiibo" => run_amiibo(&arguments),
        _ => Err(format!("unknown command: {command}")),
    };
    let (document, failure) = finalize_result(command, execution);
    let exit_code = document_exit_code(&document);
    let output = reservation.commit(&document)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "artifact": output,
            "document": document,
        }))
        .map_err(|error| error.to_string())?
    );
    if let Some(error) = failure {
        eprintln!("hardware qualification failed: {error}");
    }
    Ok(exit_code)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QualificationStatus {
    Passed,
    Failed,
    Unverified,
    NotRun,
}

impl QualificationStatus {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Unverified => "unverified",
            Self::NotRun => "not_run",
        }
    }
}

struct QualificationDecision {
    status: QualificationStatus,
    checks: Vec<Value>,
    failure: Option<String>,
}

fn finalize_result(command: &str, execution: Result<Value, String>) -> (Value, Option<String>) {
    match execution {
        Ok(result) if !cleanup_contract_succeeded(command, &result) => {
            let error = "deterministic cleanup did not complete".to_owned();
            (
                json!({
                    "schema_version": 1,
                    "command": command,
                    "status": "failed",
                    "execution_status": "failed",
                    "qualification_status": "failed",
                    "checks": [qualification_check("cleanup", "failed")],
                    "error": error,
                    "result": result,
                }),
                Some(error),
            )
        }
        Ok(result) => {
            let decision = qualification_decision(command, &result);
            let status = decision.status.as_str();
            (
                json!({
                    "schema_version": 1,
                    "command": command,
                    "status": status,
                    "execution_status": "completed",
                    "qualification_status": status,
                    "checks": decision.checks,
                    "result": result,
                }),
                decision.failure,
            )
        }
        Err(error) => (
            json!({
                "schema_version": 1,
                "command": command,
                "status": "failed",
                "execution_status": "failed",
                "qualification_status": "failed",
                "checks": [],
                "error": error,
            }),
            Some(error),
        ),
    }
}

fn qualification_decision(command: &str, result: &Value) -> QualificationDecision {
    match command {
        "discover" => discovery_qualification(result),
        "handshake" => required_checks(&[
            (
                "operation_succeeded",
                result["operation"]["state"] == "Succeeded",
            ),
            ("actual_baud_recorded", result["actual_baud"].is_u64()),
        ]),
        "smoke" | "home-wake" => {
            let neutral =
                result["final_snapshot"]["desired_report_neutral"].as_bool() == Some(true);
            if neutral {
                QualificationDecision {
                    status: QualificationStatus::Unverified,
                    checks: vec![
                        qualification_check("final_report_neutral", "passed"),
                        qualification_check("operator_observation", "unverified"),
                    ],
                    failure: None,
                }
            } else {
                required_checks(&[("final_report_neutral", false)])
            }
        }
        "faults" => required_checks(&[
            (
                "port_occupied_failed",
                result["port_occupied_expected_failed"].as_bool() == Some(true),
            ),
            (
                "cancel_reached_cancelled",
                result["cancel_expected_cancelled"].as_bool() == Some(true),
            ),
            (
                "deadline_reached_cancelled",
                result["deadline_expected_cancelled"].as_bool() == Some(true),
            ),
        ]),
        "hotplug" => required_checks(&[
            (
                "disconnect_detected",
                result["disconnect_detected"].as_bool() == Some(true),
            ),
            (
                "reconnect_succeeded",
                result["reconnect_operation"]["state"] == "Succeeded",
            ),
            (
                "stable_identity_preserved",
                result["stable_id"].as_str()
                    == result["reconnected_identity"]["stable_id"].as_str(),
            ),
        ]),
        "lifecycle" => {
            let cycles = result["cycles"].as_u64().unwrap_or_default();
            let records = result["records"].as_array();
            required_checks(&[
                ("at_least_100_cycles", cycles >= 100),
                (
                    "record_count_matches_cycles",
                    records.is_some_and(|records| records.len() as u64 == cycles),
                ),
                (
                    "no_positive_process_growth",
                    result["no_positive_growth"].as_bool() == Some(true),
                ),
                (
                    "port_present_after_every_cycle",
                    records.is_some_and(|records| {
                        records.iter().all(|record| record["port_present"] == true)
                    }),
                ),
            ])
        }
        "sequence" => {
            let steps = result["requested_steps"].as_u64().unwrap_or_default();
            let mut decision = required_checks(&[
                ("exactly_10000_steps", steps == 10_000),
                (
                    "operation_succeeded",
                    result["functional_success"].as_bool() == Some(true),
                ),
                (
                    "accepted_report_count_matches_steps",
                    result["accepted_report_count_before_final_reset"].as_u64() == Some(steps),
                ),
                (
                    "complete_write_count_includes_final_reset",
                    result["recorded_complete_writes"].as_u64() == steps.checked_add(1),
                ),
            ]);
            if decision.status == QualificationStatus::Passed {
                decision.status = QualificationStatus::Unverified;
                decision.checks.push(qualification_check(
                    "physical_order_analyzer_or_firmware_trace",
                    "unverified",
                ));
            }
            decision
        }
        "amiibo" if result["write_performed"].as_bool() != Some(true) => QualificationDecision {
            status: QualificationStatus::NotRun,
            checks: vec![qualification_check("authorized_write_performed", "not_run")],
            failure: None,
        },
        "amiibo" => QualificationDecision {
            status: QualificationStatus::Unverified,
            checks: vec![
                qualification_check("save_and_select_completed", "passed"),
                qualification_check("capacity_and_payload_evidence", "unverified"),
            ],
            failure: None,
        },
        _ => required_checks(&[("known_qualification_command", false)]),
    }
}

fn discovery_qualification(result: &Value) -> QualificationDecision {
    let samples = result["samples"].as_u64();
    let snapshots = result["snapshots"].as_array();
    let sample_count_matches = samples
        .zip(snapshots)
        .is_some_and(|(samples, snapshots)| usize::try_from(samples).ok() == Some(snapshots.len()));
    let snapshots_are_arrays = snapshots.is_some_and(|snapshots| {
        snapshots
            .iter()
            .all(|snapshot| snapshot.as_array().is_some())
    });
    let descriptors_are_valid = snapshots.is_some_and(|snapshots| {
        snapshots.iter().all(|snapshot| {
            snapshot.as_array().is_some_and(|descriptors| {
                descriptors.iter().all(|descriptor| {
                    descriptor.as_object().is_some_and(|descriptor| {
                        descriptor
                            .get("stable_id")
                            .and_then(Value::as_str)
                            .is_some_and(|stable_id| !stable_id.trim().is_empty())
                            && descriptor
                                .get("port")
                                .and_then(Value::as_str)
                                .is_some_and(|port| !port.trim().is_empty())
                    })
                })
            })
        })
    });
    let evidence_is_stable = snapshots.is_some_and(|snapshots| {
        !snapshots.is_empty() && snapshots.windows(2).all(|pair| pair[0] == pair[1])
    });
    let mut decision = required_checks(&[
        (
            "at_least_three_samples",
            samples.is_some_and(|samples| samples >= 3),
        ),
        ("sample_count_matches", sample_count_matches),
        ("snapshots_are_arrays", snapshots_are_arrays),
        ("descriptors_are_valid", descriptors_are_valid),
        (
            "stable_across_samples",
            result["stable_across_samples"].as_bool() == Some(true) && evidence_is_stable,
        ),
    ]);
    if decision.status != QualificationStatus::Passed {
        return decision;
    }

    let devices_discovered = snapshots.is_some_and(|snapshots| {
        snapshots.iter().all(|snapshot| {
            snapshot
                .as_array()
                .is_some_and(|descriptors| !descriptors.is_empty())
        })
    });
    if devices_discovered {
        decision
            .checks
            .push(qualification_check("devices_discovered", "passed"));
    } else {
        decision.status = QualificationStatus::NotRun;
        decision
            .checks
            .push(qualification_check("devices_discovered", "not_run"));
    }
    decision
}

fn required_checks(checks: &[(&str, bool)]) -> QualificationDecision {
    let failed: Vec<_> = checks
        .iter()
        .filter_map(|(name, passed)| (!passed).then_some(*name))
        .collect();
    QualificationDecision {
        status: if failed.is_empty() {
            QualificationStatus::Passed
        } else {
            QualificationStatus::Failed
        },
        checks: checks
            .iter()
            .map(|(name, passed)| {
                qualification_check(name, if *passed { "passed" } else { "failed" })
            })
            .collect(),
        failure: (!failed.is_empty())
            .then(|| format!("qualification checks failed: {}", failed.join(", "))),
    }
}

fn qualification_check(name: &str, status: &str) -> Value {
    json!({"name": name, "status": status})
}

fn cleanup_contract_succeeded(command: &str, result: &Value) -> bool {
    let expected_count = match command {
        "handshake" | "smoke" | "home-wake" | "sequence" => 1,
        "faults" => 3,
        "hotplug" => 2,
        "lifecycle" => result["records"].as_array().map_or(0, Vec::len),
        "amiibo" if result["write_performed"].as_bool() == Some(true) => 1,
        "discover" | "amiibo" => 0,
        _ => 0,
    };
    if cleanup_slot_count(result) != expected_count
        || runtime_cleanup_count(result) != expected_count
    {
        return false;
    }

    match command {
        "handshake" | "smoke" | "home-wake" | "sequence" => cleanup_succeeded(&result["cleanup"]),
        "faults" => {
            cleanup_succeeded(&result["cleanup"])
                && cleanup_succeeded(&result["secondary_cleanup"])
                && cleanup_succeeded(&result["deadline_cleanup"])
        }
        "hotplug" => {
            cleanup_succeeded(&result["cleanup"])
                && cleanup_succeeded(&result["disconnect_cleanup"])
        }
        "lifecycle" => result["records"].as_array().is_some_and(|records| {
            records
                .iter()
                .all(|record| cleanup_succeeded(&record["cleanup"]))
        }),
        "amiibo" if result["write_performed"].as_bool() == Some(true) => {
            cleanup_succeeded(&result["cleanup"])
        }
        "discover" | "amiibo" => true,
        _ => true,
    }
}

fn cleanup_succeeded(value: &Value) -> bool {
    value["kind"] == "runtime_cleanup"
        && value["succeeded"].as_bool() == Some(true)
        && value["outcome"] == "Closed"
        && value["counts"]["active_operations"].as_u64() == Some(0)
        && value["counts"]["active_resources"].as_u64() == Some(0)
        && value["counts"]["active_tasks"].as_u64() == Some(0)
}

fn cleanup_slot_count(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(cleanup_slot_count).sum(),
        Value::Object(object) => object
            .iter()
            .map(|(key, value)| {
                usize::from(key == "cleanup" || key.ends_with("_cleanup"))
                    + cleanup_slot_count(value)
            })
            .sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
    }
}

fn runtime_cleanup_count(value: &Value) -> usize {
    match value {
        Value::Array(values) => values.iter().map(runtime_cleanup_count).sum(),
        Value::Object(object) => {
            usize::from(object.get("kind").and_then(Value::as_str) == Some("runtime_cleanup"))
                + object.values().map(runtime_cleanup_count).sum::<usize>()
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => 0,
    }
}

fn document_exit_code(document: &Value) -> i32 {
    if document["execution_status"] == "cancelled" {
        return 130;
    }
    match document["qualification_status"].as_str() {
        Some("passed") => 0,
        Some("unverified" | "not_run") => 2,
        Some("failed") | None | Some(_) => 1,
    }
}

fn run_discover(arguments: &[String]) -> Result<Value, String> {
    let samples = value_or(arguments, "--samples", 3_usize)?;
    if samples == 0 {
        return Err("--samples must be non-zero".to_owned());
    }
    let mut snapshots = Vec::with_capacity(samples);
    for index in 0..samples {
        let ports = discover_system_ports().map_err(|error| error.to_string())?;
        snapshots.push(Value::Array(ports.iter().map(port_json).collect()));
        if index + 1 != samples {
            thread::sleep(Duration::from_millis(250));
        }
    }
    let stable = snapshots.windows(2).all(|pair| pair[0] == pair[1]);
    Ok(json!({
        "command": "discover",
        "samples": samples,
        "stable_across_samples": stable,
        "snapshots": snapshots,
        "capability_inference": "none",
    }))
}

fn run_handshake(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    let operation = harness.connect(ConnectOptions::default())?;
    let result = json!({
        "command": "handshake",
        "port": port_json(&harness.descriptor),
        "operation": operation,
        "actual_baud": harness.actual_baud(),
        "attempts": handshake_json(&harness.telemetry),
    });
    let cleanup = harness.close();
    Ok(with_cleanup(result, cleanup))
}

fn run_smoke(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let full = arguments.iter().any(|argument| argument == "--full");
    let wake_left_stick = arguments
        .iter()
        .any(|argument| argument == "--wake-left-stick");
    let wake_home = arguments.iter().any(|argument| argument == "--wake-home");
    let hold_ms = value_or(arguments, "--hold-ms", 0_u64)?;
    validate_hold_ms(hold_ms)?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let mut actions = Vec::new();
    if wake_home {
        for (label, action) in home_wake_actions() {
            exercise_action(&harness, label, action, &mut actions)?;
        }
        thread::sleep(Duration::from_secs(3));
    }
    if wake_left_stick {
        for (label, action) in left_stick_wake_actions() {
            exercise_action(&harness, label, action, &mut actions)?;
        }
    }
    exercise_action(
        &harness,
        "button.A.down",
        ControllerAction::ButtonDown(Button::A),
        &mut actions,
    )?;
    if hold_ms != 0 {
        thread::sleep(Duration::from_millis(hold_ms));
    }
    exercise_action(
        &harness,
        "button.A.up",
        ControllerAction::ButtonUp(Button::A),
        &mut actions,
    )?;
    exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;

    if full {
        for button in Button::ALL {
            if button != Button::A {
                exercise_action(
                    &harness,
                    &format!("button.{button:?}.down"),
                    ControllerAction::ButtonDown(button),
                    &mut actions,
                )?;
                exercise_action(
                    &harness,
                    &format!("button.{button:?}.up"),
                    ControllerAction::ButtonUp(button),
                    &mut actions,
                )?;
                exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
            }
        }
        for hat in Hat::ALL.into_iter().filter(|hat| *hat != Hat::Center) {
            exercise_action(
                &harness,
                &format!("hat.{hat:?}"),
                ControllerAction::Hat(hat),
                &mut actions,
            )?;
            exercise_action(
                &harness,
                "hat.Center",
                ControllerAction::Hat(Hat::Center),
                &mut actions,
            )?;
            exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
        }
        let boundaries = [
            StickPosition::new(0, 0),
            StickPosition::new(0, 255),
            StickPosition::new(255, 0),
            StickPosition::new(255, 255),
        ];
        for (side, constructor) in [
            (
                "left",
                ControllerAction::LeftStick as fn(StickPosition) -> ControllerAction,
            ),
            (
                "right",
                ControllerAction::RightStick as fn(StickPosition) -> ControllerAction,
            ),
        ] {
            for position in boundaries {
                exercise_action(
                    &harness,
                    &format!("stick.{side}.{},{}", position.x, position.y),
                    constructor(position),
                    &mut actions,
                )?;
                exercise_action(
                    &harness,
                    &format!("stick.{side}.center"),
                    constructor(StickPosition::CENTER),
                    &mut actions,
                )?;
                exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
            }
        }
    }
    let result = json!({
        "command": "smoke",
        "stage": if full { "full" } else { "a_only" },
        "wake_left_stick": wake_left_stick,
        "wake_home": wake_home,
        "wake_home_to_a_delay_ms": wake_home.then_some(3_000),
        "a_hold_ms": hold_ms,
        "port": port_json(&harness.descriptor),
        "actual_baud": harness.actual_baud(),
        "actions": actions,
        "final_snapshot": snapshot_json(&harness.controller),
        "latency": latency_json(&harness.telemetry, harness.actual_baud()),
        "switch_observation": "requires operator confirmation",
    });
    let cleanup = harness.close();
    Ok(with_cleanup(result, cleanup))
}

fn run_home_wake(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let attempts = value_or(arguments, "--attempts", 20_usize)?;
    let interval_seconds = value_or(arguments, "--interval-seconds", 3_u64)?;
    validate_home_wake(attempts, interval_seconds)?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let started = Instant::now();
    let mut records = Vec::with_capacity(attempts);
    for attempt in 0..attempts {
        let target = Duration::from_secs(
            u64::try_from(attempt)
                .expect("bounded attempt index fits u64")
                .saturating_mul(interval_seconds),
        );
        if let Some(remaining) = target.checked_sub(started.elapsed()) {
            thread::sleep(remaining);
        }
        let attempt_started_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut actions = Vec::with_capacity(3);
        exercise_action(
            &harness,
            "button.Home.down",
            ControllerAction::ButtonDown(Button::Home),
            &mut actions,
        )?;
        println!("{}", home_attempt_marker(attempt + 1));
        std::io::stdout()
            .flush()
            .map_err(|error| error.to_string())?;
        exercise_action(
            &harness,
            "button.Home.up",
            ControllerAction::ButtonUp(Button::Home),
            &mut actions,
        )?;
        exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
        records.push(json!({
            "attempt": attempt + 1,
            "started_after_ms": attempt_started_ms,
            "actions": actions,
        }));
    }
    let result = json!({
        "command": "home-wake",
        "attempts": attempts,
        "interval_seconds": interval_seconds,
        "actual_baud": harness.actual_baud(),
        "port": port_json(&harness.descriptor),
        "records": records,
        "final_snapshot": snapshot_json(&harness.controller),
        "latency": latency_json(&harness.telemetry, harness.actual_baud()),
        "switch_observation": "requires operator confirmation",
    });
    let cleanup = harness.close();
    Ok(with_cleanup(result, cleanup))
}

fn run_faults(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let primary = Harness::new(&port, ControllerOptions::default())?;
    primary.connect(ConnectOptions::default())?;

    let occupied = Harness::new(&port, ControllerOptions::default())?;
    let occupied_operation = occupied
        .controller
        .connect(ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 300_000_000,
        })
        .map_err(|error| error.to_string())?;
    wait_terminal(&occupied_operation, OPERATION_TIMEOUT)?;
    let occupied_result = operation_json(&occupied_operation);
    let occupied_cleanup = occupied.close();

    let sequence = PreciseSequence::new(vec![
        SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
        SequenceStep::new(5_000_000_000, ControllerAction::ButtonUp(Button::A)),
    ])
    .map_err(|error| error.to_string())?;
    let cancelled = primary
        .controller
        .precise_sequence(sequence)
        .map_err(|error| error.to_string())?;
    let _ = cancelled.cancel();
    wait_terminal(&cancelled, OPERATION_TIMEOUT)?;

    let deadline = Harness::new(&port, ControllerOptions::default())?;
    let deadline_operation = deadline
        .controller
        .connect(ConnectOptions {
            operation_deadline_ns: Some(deadline.now_ns()),
            protocol_timeout_ns: 300_000_000,
        })
        .map_err(|error| error.to_string())?;
    wait_terminal(&deadline_operation, OPERATION_TIMEOUT)?;
    let deadline_result = operation_json(&deadline_operation);
    let deadline_cleanup = deadline.close();

    let result = json!({
        "command": "faults",
        "port_occupied": occupied_result,
        "port_occupied_expected_failed": occupied_operation.snapshot().state == OperationState::Failed,
        "cancel": operation_json(&cancelled),
        "cancel_expected_cancelled": cancelled.snapshot().state == OperationState::Cancelled,
        "post_cancel_snapshot": snapshot_json(&primary.controller),
        "deadline": deadline_result,
        "deadline_expected_cancelled": deadline_operation.snapshot().state == OperationState::Cancelled,
        "secondary_cleanup": occupied_cleanup,
        "deadline_cleanup": deadline_cleanup,
    });
    let cleanup = primary.close();
    Ok(with_cleanup(result, cleanup))
}

fn run_hotplug(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let timeout_seconds = value_or(arguments, "--timeout-seconds", 180_u64)?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let stable_id = harness.descriptor.stable_id().to_owned();
    println!("HOTPLUG_READY: unplug {port} now");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    wait_for_presence(&stable_id, false, Duration::from_secs(timeout_seconds))?;
    let disconnected = harness
        .controller
        .reset()
        .map_err(|error| error.to_string())?;
    wait_terminal(&disconnected, OPERATION_TIMEOUT)?;
    let disconnect_cleanup = harness.close();

    println!("HOTPLUG_DISCONNECTED: reconnect the same device now");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    wait_for_presence(&stable_id, true, Duration::from_secs(timeout_seconds))?;
    let reconnected = Harness::new(&port, ControllerOptions::default())?;
    let reconnect_operation = reconnected.connect(ConnectOptions::default())?;
    let result = json!({
        "command": "hotplug",
        "stable_id": stable_id,
        "disconnect_operation": operation_json(&disconnected),
        "disconnect_detected": disconnected.snapshot().state == OperationState::Failed,
        "disconnect_cleanup": disconnect_cleanup,
        "reconnect_operation": reconnect_operation,
        "reconnect_baud": reconnected.actual_baud(),
        "reconnected_identity": port_json(&reconnected.descriptor),
    });
    let cleanup = reconnected.close();
    Ok(with_cleanup(result, cleanup))
}

fn run_lifecycle(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let cycles = value_or(arguments, "--cycles", 100_usize)?;
    if cycles == 0 {
        return Err("--cycles must be non-zero".to_owned());
    }
    let before = process_metrics()?;
    let mut records = Vec::with_capacity(cycles);
    for cycle in 1..=cycles {
        let harness = Harness::new(&port, ControllerOptions::default())?;
        harness.connect(ConnectOptions::default())?;
        let baud = harness.actual_baud();
        let cleanup = harness.close();
        let metrics = process_metrics()?;
        records.push(json!({
            "cycle": cycle,
            "baud": baud,
            "cleanup": cleanup,
            "process": metrics,
            "port_present": find_port(&port).is_ok(),
        }));
    }
    let after = process_metrics()?;
    let handle_delta = after["handles"].as_i64().unwrap_or_default()
        - before["handles"].as_i64().unwrap_or_default();
    let thread_delta = after["threads"].as_i64().unwrap_or_default()
        - before["threads"].as_i64().unwrap_or_default();
    Ok(json!({
        "command": "lifecycle",
        "cycles": cycles,
        "before": before,
        "after": after,
        "handle_delta": handle_delta,
        "thread_delta": thread_delta,
        "no_positive_growth": handle_delta <= 0 && thread_delta <= 0,
        "records": records,
    }))
}

fn run_sequence(arguments: &[String], artifact_dir: &Path) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let steps = value_or(arguments, "--steps", 10_000_usize)?;
    if !(1..=10_000).contains(&steps) {
        return Err("--steps must be in 1..=10000".to_owned());
    }
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let sequence_steps: Vec<_> = (0..steps)
        .map(|index| {
            let offset = u64::try_from(index)
                .expect("step index fits u64")
                .saturating_mul(MINIMUM_REPORT_INTERVAL_NS);
            let action = if index.is_multiple_of(2) {
                ControllerAction::ButtonDown(Button::A)
            } else {
                ControllerAction::ButtonUp(Button::A)
            };
            SequenceStep::new(offset, action)
        })
        .collect();
    let sequence = PreciseSequence::new(sequence_steps).map_err(|error| error.to_string())?;
    let started = harness.now_ns();
    let operation = harness
        .controller
        .precise_sequence(sequence)
        .map_err(|error| error.to_string())?;
    let expected_seconds = u64::try_from(steps)
        .expect("step count fits u64")
        .saturating_mul(30)
        .div_ceil(1_000)
        .saturating_add(60);
    wait_terminal(&operation, Duration::from_secs(expected_seconds))?;
    let ended = harness.now_ns();
    let reset = harness
        .controller
        .reset()
        .map_err(|error| error.to_string())?;
    wait_succeeded(&reset, OPERATION_TIMEOUT)?;
    fs::create_dir_all(artifact_dir).map_err(|error| error.to_string())?;
    let csv = artifact_dir.join("sequence-timings.csv");
    write_timing_csv(&csv, &harness.telemetry)?;
    let timing_count = harness
        .telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .timings
        .len();
    let result = json!({
        "command": "sequence",
        "requested_steps": steps,
        "operation": operation_json(&operation),
        "functional_success": operation.snapshot().state == OperationState::Succeeded,
        "accepted_report_count_before_final_reset": harness.controller.snapshot().accepted_report_count.saturating_sub(1),
        "recorded_complete_writes": timing_count,
        "elapsed_ns": ended.saturating_sub(started),
        "actual_baud": harness.actual_baud(),
        "latency": latency_json(&harness.telemetry, harness.actual_baud()),
        "timing_csv": csv,
        "physical_order_evidence": "open: no logic analyzer or firmware trace",
    });
    let cleanup = harness.close();
    Ok(with_cleanup(result, cleanup))
}

fn run_amiibo(arguments: &[String]) -> Result<Value, String> {
    if !arguments
        .iter()
        .any(|argument| argument == "--authorize-write")
    {
        return Ok(json!({
            "command": "amiibo",
            "capability": "unknown",
            "write_performed": false,
            "reason": "the v1 protocol has no safe capacity query; explicit limits and write authorization are required",
        }));
    }
    if !arguments
        .iter()
        .any(|argument| argument == "--confirm-disposable")
    {
        return Err("Amiibo write also requires --confirm-disposable".to_owned());
    }
    let port: String = required_value(arguments, "--port")?;
    let slot: u8 = required_value(arguments, "--slot")?;
    let slot_count: u16 = required_value(arguments, "--slot-count")?;
    let maximum_data_len: usize = required_value(arguments, "--maximum-data-len")?;
    let data_path = PathBuf::from(
        option_value(arguments, "--data")?
            .ok_or_else(|| "missing required option --data".to_owned())?,
    );
    let data = fs::read(&data_path).map_err(|error| error.to_string())?;
    let limits =
        AmiiboLimits::new(slot_count, maximum_data_len).map_err(|error| error.to_string())?;
    let harness = Harness::new(
        &port,
        ControllerOptions {
            amiibo_limits: Some(limits),
            ..ControllerOptions::default()
        },
    )?;
    harness.connect(ConnectOptions::default())?;
    let save = harness
        .controller
        .save_amiibo(slot, data, AmiiboSaveOptions::default())
        .map_err(|error| error.to_string())?;
    wait_succeeded(&save, Duration::from_secs(60))?;
    let select = harness
        .controller
        .select_amiibo(slot, AmiiboSelectOptions::default())
        .map_err(|error| error.to_string())?;
    wait_succeeded(&select, OPERATION_TIMEOUT)?;
    let result = json!({
        "command": "amiibo",
        "write_performed": true,
        "slot": slot,
        "slot_count": slot_count,
        "maximum_data_len": maximum_data_len,
        "data_path": data_path,
        "save": operation_json(&save),
        "select": operation_json(&select),
    });
    let cleanup = harness.close();
    Ok(with_cleanup(result, cleanup))
}

fn exercise_action(
    harness: &Harness,
    label: &str,
    action: ControllerAction,
    output: &mut Vec<Value>,
) -> Result<(), String> {
    let operation = harness
        .controller
        .direct(action)
        .map_err(|error| error.to_string())?;
    wait_succeeded(&operation, OPERATION_TIMEOUT)?;
    output.push(json!({"label": label, "operation": operation_json(&operation)}));
    Ok(())
}

fn validate_hold_ms(hold_ms: u64) -> Result<(), String> {
    if hold_ms <= 5_000 {
        Ok(())
    } else {
        Err("--hold-ms must not exceed 5000".to_owned())
    }
}

fn left_stick_wake_actions() -> [(&'static str, ControllerAction); 4] {
    [
        (
            "wake.left_stick.right",
            ControllerAction::LeftStick(StickPosition::new(255, 128)),
        ),
        (
            "wake.left_stick.left",
            ControllerAction::LeftStick(StickPosition::new(0, 128)),
        ),
        (
            "wake.left_stick.center",
            ControllerAction::LeftStick(StickPosition::CENTER),
        ),
        ("wake.neutral", ControllerAction::Reset),
    ]
}

fn home_wake_actions() -> [(&'static str, ControllerAction); 3] {
    [
        ("wake.Home.down", ControllerAction::ButtonDown(Button::Home)),
        ("wake.Home.up", ControllerAction::ButtonUp(Button::Home)),
        ("wake.Home.neutral", ControllerAction::Reset),
    ]
}

fn validate_home_wake(attempts: usize, interval_seconds: u64) -> Result<(), String> {
    if !(1..=100).contains(&attempts) {
        return Err("--attempts must be in 1..=100".to_owned());
    }
    if !(1..=60).contains(&interval_seconds) {
        return Err("--interval-seconds must be in 1..=60".to_owned());
    }
    Ok(())
}

fn home_attempt_marker(attempt: usize) -> String {
    attempt.to_string()
}

fn find_port(port_name: &str) -> Result<SerialPortDescriptor, String> {
    discover_system_ports()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|port| port.port_name().eq_ignore_ascii_case(port_name))
        .ok_or_else(|| format!("system discovery did not report {port_name}"))
}

fn wait_for_presence(stable_id: &str, present: bool, timeout: Duration) -> Result<(), String> {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        let found = discover_system_ports()
            .map_err(|error| error.to_string())?
            .iter()
            .any(|port| port.stable_id() == stable_id);
        if found == present {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "timed out waiting for stable identity to become {}",
        if present { "present" } else { "absent" }
    ))
}

fn wait_succeeded(operation: &Operation, timeout: Duration) -> Result<(), String> {
    let snapshot = wait_terminal(operation, timeout)?;
    if snapshot.state == OperationState::Succeeded {
        Ok(())
    } else {
        Err(operation_failure(operation))
    }
}

fn wait_terminal(
    operation: &Operation,
    timeout: Duration,
) -> Result<easycon_runtime::OperationSnapshot, String> {
    match operation.wait(WaitTimeout::For(timeout)) {
        WaitResult::Completed(snapshot) => Ok(snapshot),
        WaitResult::Timeout => Err(format!("operation {} wait timed out", operation.id().get())),
    }
}

fn operation_failure(operation: &Operation) -> String {
    let snapshot = operation.snapshot();
    format!(
        "operation {} ended in {:?}: {:?}",
        operation.id().get(),
        snapshot.state,
        snapshot.error
    )
}

fn operation_json(operation: &Operation) -> Value {
    let snapshot = operation.snapshot();
    json!({
        "id": operation.id().get(),
        "state": format!("{:?}", snapshot.state),
        "error": snapshot.error.map(|error| error.to_string()),
        "cancellation_reason": snapshot.cancellation_reason.map(|reason| format!("{reason:?}")),
    })
}

fn port_json(port: &SerialPortDescriptor) -> Value {
    let usb = port.usb_identifiers();
    json!({
        "stable_id": port.stable_id(),
        "port": port.port_name(),
        "friendly_name": port.friendly_name(),
        "manufacturer": port.manufacturer(),
        "vid": usb.map(|value| format!("{:04X}", value.vid)),
        "pid": usb.map(|value| format!("{:04X}", value.pid)),
    })
}

fn snapshot_json(controller: &ControllerSession) -> Value {
    let snapshot = controller.snapshot();
    json!({
        "state": format!("{:?}", snapshot.state),
        "desired_report_neutral": snapshot.desired_report.is_neutral(),
        "accepted_report_count": snapshot.accepted_report_count,
        "last_report_timestamp_ns": snapshot.last_report_timestamp_ns,
        "lease": format!("{:?}", snapshot.lease),
    })
}

fn handshake_json(telemetry: &Arc<Mutex<Telemetry>>) -> Value {
    Value::Array(
        telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .handshake_attempts
            .iter()
            .map(|attempt| {
                json!({
                    "baud": attempt.baud,
                    "succeeded": attempt.succeeded,
                    "error": attempt.error,
                })
            })
            .collect(),
    )
}

fn latency_json(telemetry: &Arc<Mutex<Telemetry>>, baud: Option<u32>) -> Value {
    let telemetry = telemetry.lock().unwrap_or_else(|error| error.into_inner());
    let direct: Vec<_> = telemetry
        .timings
        .iter()
        .filter(|sample| sample.command_admitted_ns.is_some())
        .copied()
        .collect();
    let admission_to_write: Vec<_> = direct
        .iter()
        .map(|sample| {
            sample
                .write_entered_ns
                .saturating_sub(sample.command_admitted_ns.expect("filtered direct timing"))
        })
        .collect();
    let write_call: Vec<_> = telemetry
        .timings
        .iter()
        .map(|sample| {
            sample
                .transport_accepted_ns
                .saturating_sub(sample.write_entered_ns)
        })
        .collect();
    let dispatch_to_write: Vec<_> = telemetry
        .timings
        .iter()
        .map(|sample| sample.write_entered_ns.saturating_sub(sample.dispatch_ns))
        .collect();
    json!({
        "sample_count": telemetry.timings.len(),
        "direct_sample_count": direct.len(),
        "command_admitted_to_write_entered_ns": distribution_json(&admission_to_write),
        "dispatch_to_write_entered_ns": distribution_json(&dispatch_to_write),
        "write_entered_to_os_acceptance_ns": distribution_json(&write_call),
        "uart_complete_frame": baud.map(|value| json!({
            "baud": value,
            "bytes": 8,
            "format": "8N1",
            "theoretical_ns": 80_000_000_000_u64 / u64::from(value),
            "measured": false,
        })),
        "usb_hid": {"measured": false, "reason": "USB analyzer or auditable firmware trace unavailable"},
    })
}

fn distribution_json(values: &[u64]) -> Value {
    distribution(values).map_or(
        Value::Null,
        |value| json!({"p50": value.p50, "p95": value.p95, "p99": value.p99, "max": value.max}),
    )
}

fn counts_json(counts: RuntimeCounts) -> Value {
    json!({
        "active_operations": counts.active_operations,
        "active_resources": counts.active_resources,
        "active_tasks": counts.active_tasks,
    })
}

fn with_cleanup(mut value: Value, cleanup: Value) -> Value {
    value["cleanup"] = cleanup;
    value
}

fn runtime_close_json(
    outcome: Result<CloseOutcome, CloseRejection>,
    observed_counts: RuntimeCounts,
) -> Value {
    match outcome {
        Ok(CloseOutcome::Closed) => {
            let succeeded = counts_are_zero(observed_counts);
            json!({
                "kind": "runtime_cleanup",
                "succeeded": succeeded,
                "outcome": "Closed",
                "counts": counts_json(observed_counts),
                "diagnostic": (!succeeded).then_some("Runtime reported Closed with non-zero registries"),
            })
        }
        Ok(CloseOutcome::Failed(report)) => json!({
            "kind": "runtime_cleanup",
            "succeeded": false,
            "outcome": "Failed",
            "counts": counts_json(observed_counts),
            "report": {
                "phase": format!("{:?}", report.phase),
                "diagnostic": report.diagnostic.as_ref(),
                "resource_id": report.resource_id.map(|id| id.get()),
                "task_id": report.task_id.map(|id| id.get()),
                "counts": counts_json(report.counts),
            },
        }),
        Err(rejection) => json!({
            "kind": "runtime_cleanup",
            "succeeded": false,
            "outcome": "Rejected",
            "counts": counts_json(observed_counts),
            "rejection": format!("{rejection:?}"),
        }),
    }
}

fn counts_are_zero(counts: RuntimeCounts) -> bool {
    counts.active_operations == 0 && counts.active_resources == 0 && counts.active_tasks == 0
}

fn process_metrics() -> Result<Value, String> {
    let process_id = std::process::id();
    let script = format!(
        "$p=Get-Process -Id {process_id}; Write-Output ($p.HandleCount.ToString() + ',' + $p.Threads.Count.ToString())"
    );
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    let mut fields = text.trim().split(',');
    let handles: i64 = fields
        .next()
        .ok_or_else(|| "missing process handle count".to_owned())?
        .parse()
        .map_err(|_| "invalid process handle count".to_owned())?;
    let threads: i64 = fields
        .next()
        .ok_or_else(|| "missing process thread count".to_owned())?
        .parse()
        .map_err(|_| "invalid process thread count".to_owned())?;
    Ok(json!({"handles": handles, "threads": threads}))
}

fn write_timing_csv(path: &Path, telemetry: &Arc<Mutex<Telemetry>>) -> Result<(), String> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "sequence,command_admitted_ns,lane_wake_ns,dispatch_ns,write_entered_ns,transport_accepted_ns"
    )
    .map_err(|error| error.to_string())?;
    for sample in &telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .timings
    {
        writeln!(
            writer,
            "{},{},{},{},{},{}",
            sample.sequence,
            sample
                .command_admitted_ns
                .map_or_else(String::new, |value| value.to_string()),
            sample
                .lane_wake_ns
                .map_or_else(String::new, |value| value.to_string()),
            sample.dispatch_ns,
            sample.write_entered_ns,
            sample.transport_accepted_ns,
        )
        .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
}

fn artifact_dir(arguments: &[String]) -> Result<PathBuf, String> {
    if let Some(value) = option_value(arguments, "--output-dir")? {
        return Ok(PathBuf::from(value));
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_millis();
    Ok(PathBuf::from("artifacts")
        .join("hardware")
        .join(timestamp.to_string()))
}

fn print_help() {
    println!(
        "Usage:\n  easycon-hardware-qualification discover [--samples N]\n  \
         easycon-hardware-qualification handshake --port COMx\n  \
         easycon-hardware-qualification smoke --port COMx [--full] [--wake-left-stick] [--wake-home] [--hold-ms N]\n  \
         easycon-hardware-qualification home-wake --port COMx [--attempts 20] [--interval-seconds 3]\n  \
         easycon-hardware-qualification faults --port COMx\n  \
         easycon-hardware-qualification hotplug --port COMx [--timeout-seconds N]\n  \
         easycon-hardware-qualification lifecycle --port COMx [--cycles 100]\n  \
         easycon-hardware-qualification sequence --port COMx [--steps 10000]\n  \
         easycon-hardware-qualification amiibo [--port COMx --slot N --slot-count N \
         --maximum-data-len N --data FILE --authorize-write --confirm-disposable]\n\n  \
         Every command accepts --output-dir PATH."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use easycon_controller::TransportErrorKind;
    use easycon_runtime::ManagedResource;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "easycon-hardware-main-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after epoch")
                    .as_nanos()
            ));
            fs::create_dir(&path).expect("create unique test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn successful_cleanup() -> Value {
        json!({
            "kind": "runtime_cleanup",
            "succeeded": true,
            "outcome": "Closed",
            "counts": {
                "active_operations": 0,
                "active_resources": 0,
                "active_tasks": 0,
            },
        })
    }

    struct NoopTransport;

    impl ControllerTransport for NoopTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in close-outcome test",
            ))
        }

        fn close(&mut self) {}
    }

    struct PanickingResource;

    impl ManagedResource for PanickingResource {
        fn close(&self) {
            panic!("injected qualification close failure");
        }
    }

    #[test]
    fn failed_command_still_produces_an_artifact_document() {
        let (document, error) = finalize_result("handshake", Err("protocol timeout".to_owned()));
        assert_eq!(document["status"], "failed");
        assert_eq!(document["command"], "handshake");
        assert_eq!(document["error"], "protocol timeout");
        assert_eq!(error.as_deref(), Some("protocol timeout"));
    }

    #[test]
    fn false_qualification_predicates_do_not_pass_or_exit_zero() {
        let cases = [
            (
                "faults",
                json!({
                    "port_occupied_expected_failed": false,
                    "cancel_expected_cancelled": true,
                    "deadline_expected_cancelled": true,
                    "cleanup": successful_cleanup(),
                    "secondary_cleanup": successful_cleanup(),
                    "deadline_cleanup": successful_cleanup(),
                }),
            ),
            (
                "hotplug",
                json!({
                    "disconnect_detected": false,
                    "cleanup": successful_cleanup(),
                    "disconnect_cleanup": successful_cleanup(),
                }),
            ),
            (
                "lifecycle",
                json!({"cycles": 100, "no_positive_growth": false, "records": []}),
            ),
            (
                "sequence",
                json!({
                    "requested_steps": 10_000,
                    "functional_success": false,
                    "accepted_report_count_before_final_reset": 10_000,
                    "recorded_complete_writes": 10_001,
                    "cleanup": successful_cleanup(),
                }),
            ),
        ];

        for (command, result) in cases {
            let (document, failure) = finalize_result(command, Ok(result));
            assert_ne!(document["status"], "passed", "{command}");
            assert!(failure.is_some(), "{command} must return a non-zero exit");
            assert_eq!(document_exit_code(&document), 1, "{command}");
        }
    }

    #[test]
    fn discovery_requires_nonempty_consistent_samples() {
        let empty_snapshots = json!({
            "samples": 3,
            "stable_across_samples": true,
            "snapshots": [[], [], []],
        });
        let (empty, failure) = finalize_result("discover", Ok(empty_snapshots));
        assert!(failure.is_none());
        assert_eq!(empty["execution_status"], "completed");
        assert_eq!(empty["qualification_status"], "not_run");
        assert_eq!(document_exit_code(&empty), 2);

        for result in [
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [],
            }),
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [
                    [{"stable_id": "DEVICE\\ONE", "port": "COM1"}],
                    [{"stable_id": "DEVICE\\TWO", "port": "COM2"}],
                    [{"stable_id": "DEVICE\\ONE", "port": "COM1"}],
                ],
            }),
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [[null], [null], [null]],
            }),
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [
                    [{"stable_id": "", "port": "COM8"}],
                    [{"stable_id": "", "port": "COM8"}],
                    [{"stable_id": "", "port": "COM8"}],
                ],
            }),
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [
                    [{"stable_id": "DEVICE\\EXPECTED", "port": ""}],
                    [{"stable_id": "DEVICE\\EXPECTED", "port": ""}],
                    [{"stable_id": "DEVICE\\EXPECTED", "port": ""}],
                ],
            }),
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [
                    [{"stable_id": "   ", "port": "COM8"}],
                    [{"stable_id": "   ", "port": "COM8"}],
                    [{"stable_id": "   ", "port": "COM8"}],
                ],
            }),
            json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [
                    [{"stable_id": "DEVICE\\EXPECTED", "port": "   "}],
                    [{"stable_id": "DEVICE\\EXPECTED", "port": "   "}],
                    [{"stable_id": "DEVICE\\EXPECTED", "port": "   "}],
                ],
            }),
        ] {
            let (document, failure) = finalize_result("discover", Ok(result));
            assert!(failure.is_some());
            assert_eq!(document["execution_status"], "completed");
            assert_eq!(document["qualification_status"], "failed");
            assert_eq!(document_exit_code(&document), 1);
        }

        let stable_device = json!([{
            "stable_id": "DEVICE\\EXPECTED",
            "port": "COM8",
        }]);
        let (nonempty, failure) = finalize_result(
            "discover",
            Ok(json!({
                "samples": 3,
                "stable_across_samples": true,
                "snapshots": [
                    stable_device.clone(),
                    stable_device.clone(),
                    stable_device,
                ],
            })),
        );
        assert!(failure.is_none());
        assert_eq!(nonempty["qualification_status"], "passed");
        assert_eq!(document_exit_code(&nonempty), 0);
    }

    #[test]
    fn cleanup_layout_is_required_and_exact_for_each_command() {
        let cleanup = successful_cleanup;
        let lifecycle_records = || {
            (0..100)
                .map(|cycle| {
                    json!({
                        "cycle": cycle + 1,
                        "cleanup": cleanup(),
                        "port_present": true,
                    })
                })
                .collect::<Vec<_>>()
        };
        let valid_cases = [
            (
                "handshake",
                json!({
                    "operation": {"state": "Succeeded"},
                    "actual_baud": 115_200,
                    "cleanup": cleanup(),
                }),
            ),
            (
                "smoke",
                json!({
                    "final_snapshot": {"desired_report_neutral": true},
                    "cleanup": cleanup(),
                }),
            ),
            (
                "home-wake",
                json!({
                    "final_snapshot": {"desired_report_neutral": true},
                    "cleanup": cleanup(),
                }),
            ),
            (
                "faults",
                json!({
                    "port_occupied_expected_failed": true,
                    "cancel_expected_cancelled": true,
                    "deadline_expected_cancelled": true,
                    "cleanup": cleanup(),
                    "secondary_cleanup": cleanup(),
                    "deadline_cleanup": cleanup(),
                }),
            ),
            (
                "hotplug",
                json!({
                    "disconnect_detected": true,
                    "reconnect_operation": {"state": "Succeeded"},
                    "stable_id": "DEVICE\\EXPECTED",
                    "reconnected_identity": {"stable_id": "DEVICE\\EXPECTED"},
                    "cleanup": cleanup(),
                    "disconnect_cleanup": cleanup(),
                }),
            ),
            (
                "lifecycle",
                json!({
                    "cycles": 100,
                    "no_positive_growth": true,
                    "records": lifecycle_records(),
                }),
            ),
            (
                "sequence",
                json!({
                    "requested_steps": 10_000,
                    "functional_success": true,
                    "accepted_report_count_before_final_reset": 10_000,
                    "recorded_complete_writes": 10_001,
                    "cleanup": cleanup(),
                }),
            ),
            (
                "amiibo",
                json!({
                    "write_performed": true,
                    "cleanup": cleanup(),
                }),
            ),
        ];

        for (command, result) in valid_cases {
            let (document, _) = finalize_result(command, Ok(result.clone()));
            assert_ne!(document["execution_status"], "failed", "valid {command}");

            let mut missing = result.clone();
            if command == "lifecycle" {
                missing["records"][0]
                    .as_object_mut()
                    .expect("lifecycle record")
                    .remove("cleanup");
            } else {
                missing
                    .as_object_mut()
                    .expect("command result")
                    .remove("cleanup");
            }
            assert_cleanup_failure(command, missing);

            let mut extra = result;
            extra["misplaced"]["cleanup"] = cleanup();
            assert_cleanup_failure(command, extra);
        }

        let mut failed_outcome = cleanup();
        failed_outcome["outcome"] = json!("Failed");
        assert_cleanup_failure(
            "handshake",
            json!({
                "operation": {"state": "Succeeded"},
                "actual_baud": 115_200,
                "cleanup": failed_outcome,
            }),
        );
        let mut nonzero_counts = cleanup();
        nonzero_counts["counts"]["active_operations"] = json!(1);
        assert_cleanup_failure(
            "handshake",
            json!({
                "operation": {"state": "Succeeded"},
                "actual_baud": 115_200,
                "cleanup": nonzero_counts,
            }),
        );

        for (command, mut result) in [
            (
                "discover",
                json!({
                    "samples": 3,
                    "stable_across_samples": true,
                    "snapshots": [
                        [{"stable_id": "DEVICE\\EXPECTED", "port": "COM8"}],
                        [{"stable_id": "DEVICE\\EXPECTED", "port": "COM8"}],
                        [{"stable_id": "DEVICE\\EXPECTED", "port": "COM8"}],
                    ],
                }),
            ),
            (
                "amiibo",
                json!({
                    "write_performed": false,
                    "capability": "unknown",
                }),
            ),
        ] {
            let (document, _) = finalize_result(command, Ok(result.clone()));
            assert_ne!(document["execution_status"], "failed", "valid {command}");
            result["cleanup"] = cleanup();
            assert_cleanup_failure(command, result);
        }
    }

    fn assert_cleanup_failure(command: &str, result: Value) {
        let (document, failure) = finalize_result(command, Ok(result));
        assert!(
            failure.is_some(),
            "{command} must reject its cleanup layout"
        );
        assert_eq!(document["execution_status"], "failed", "{command}");
        assert_eq!(document["qualification_status"], "failed", "{command}");
        assert_eq!(document_exit_code(&document), 1, "{command}");
    }

    #[test]
    fn pending_observation_and_unwritten_amiibo_are_not_passed() {
        let (smoke, _) = finalize_result(
            "smoke",
            Ok(json!({
                "final_snapshot": {"desired_report_neutral": true},
                "switch_observation": "requires operator confirmation",
                "cleanup": successful_cleanup(),
            })),
        );
        assert_eq!(smoke["status"], "unverified");
        assert_eq!(document_exit_code(&smoke), 2);

        let (home, _) = finalize_result(
            "home-wake",
            Ok(json!({
                "final_snapshot": {"desired_report_neutral": true},
                "switch_observation": "requires operator confirmation",
                "cleanup": successful_cleanup(),
            })),
        );
        assert_eq!(home["status"], "unverified");
        assert_eq!(document_exit_code(&home), 2);

        let (amiibo, _) = finalize_result(
            "amiibo",
            Ok(json!({"write_performed": false, "capability": "unknown"})),
        );
        assert_eq!(amiibo["status"], "not_run");
        assert_eq!(document_exit_code(&amiibo), 2);
    }

    #[test]
    fn functional_sequence_without_physical_trace_is_unverified() {
        let (document, failure) = finalize_result(
            "sequence",
            Ok(json!({
                "requested_steps": 10_000,
                "functional_success": true,
                "accepted_report_count_before_final_reset": 10_000,
                "recorded_complete_writes": 10_001,
                "physical_order_evidence": "open: no logic analyzer or firmware trace",
                "cleanup": successful_cleanup(),
            })),
        );

        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 2);
    }

    #[test]
    fn runtime_close_failure_is_not_reported_as_clean_counts() {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let runtime = Runtime::new(clock.clone());
        let controller = ControllerSession::new(
            &runtime,
            Box::new(NoopTransport),
            ControllerOptions::default(),
        )
        .expect("controller");
        let resource: Arc<dyn ManagedResource> = Arc::new(PanickingResource);
        let _registration = runtime
            .register_resource(resource.clone())
            .expect("resource");
        let harness = Harness {
            runtime,
            clock,
            controller,
            telemetry: Arc::new(Mutex::new(Telemetry::default())),
            descriptor: SerialPortDescriptor::new("TEST\\CLOSE-FAILURE", "COM1")
                .expect("descriptor"),
        };

        let cleanup = harness.close();
        assert_eq!(cleanup["succeeded"], false);
        assert_eq!(cleanup["outcome"], "Failed");
        assert_eq!(cleanup["report"]["phase"], "ResourceCleanup");
        assert_eq!(
            cleanup["report"]["diagnostic"],
            "ManagedResource::close panicked"
        );

        let (document, failure) = finalize_result("handshake", Ok(json!({"cleanup": cleanup})));
        assert!(failure.is_some());
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(
            document["result"]["cleanup"]["report"]["phase"],
            "ResourceCleanup"
        );
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn diagnostic_hold_is_bounded() {
        assert!(validate_hold_ms(0).is_ok());
        assert!(validate_hold_ms(5_000).is_ok());
        assert!(validate_hold_ms(5_001).is_err());
    }

    #[test]
    fn left_stick_wake_returns_to_neutral_before_smoke() {
        let actions = left_stick_wake_actions();
        assert_eq!(
            actions.map(|(_, action)| action),
            [
                ControllerAction::LeftStick(StickPosition::new(255, 128)),
                ControllerAction::LeftStick(StickPosition::new(0, 128)),
                ControllerAction::LeftStick(StickPosition::CENTER),
                ControllerAction::Reset,
            ]
        );
    }

    #[test]
    fn home_wake_releases_and_neutralizes_before_smoke() {
        assert_eq!(
            home_wake_actions().map(|(_, action)| action),
            [
                ControllerAction::ButtonDown(Button::Home),
                ControllerAction::ButtonUp(Button::Home),
                ControllerAction::Reset,
            ]
        );
    }

    #[test]
    fn home_wake_loop_is_bounded() {
        assert!(validate_home_wake(20, 3).is_ok());
        assert!(validate_home_wake(0, 3).is_err());
        assert!(validate_home_wake(101, 3).is_err());
        assert!(validate_home_wake(20, 0).is_err());
        assert!(validate_home_wake(20, 61).is_err());
    }

    #[test]
    fn home_attempt_marker_is_the_one_based_number_only() {
        assert_eq!(home_attempt_marker(1), "1");
        assert_eq!(home_attempt_marker(20), "20");
    }

    #[test]
    fn existing_timing_csv_is_rejected_without_overwrite() {
        let path = std::env::temp_dir().join(format!(
            "easycon-hardware-existing-timing-{}-{}.csv",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        let sentinel = b"original timing evidence\n";
        fs::write(&path, sentinel).expect("seed existing timing evidence");
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));

        let result = write_timing_csv(&path, &telemetry);

        assert!(result.is_err());
        assert_eq!(
            fs::read(&path).expect("read original timing evidence"),
            sentinel
        );
        fs::remove_file(path).expect("remove timing test file");
    }

    #[test]
    fn existing_temporary_result_is_rejected_before_reservation() {
        let directory = TestDirectory::new("existing-temporary");
        let temporary = directory.0.join("unknown.json.tmp");
        let sentinel = b"unfinished evidence owned by another run\n";
        fs::write(&temporary, sentinel).expect("seed existing temporary evidence");

        let result = ArtifactReservation::begin("unknown", &directory.0);

        assert!(result.is_err());
        assert_eq!(fs::read(temporary).expect("preserved temporary"), sentinel);
        assert!(!directory.0.join(".unknown.in-progress.json").exists());
    }

    #[test]
    fn commit_does_not_delete_a_temporary_file_it_did_not_create() {
        let directory = TestDirectory::new("raced-temporary");
        let reservation = ArtifactReservation::begin("unknown", &directory.0).expect("reserve run");
        let temporary = directory.0.join("unknown.json.tmp");
        let sentinel = b"raced evidence owned by another writer\n";
        fs::write(&temporary, sentinel).expect("seed raced temporary evidence");

        let result = reservation.commit(&json!({"status": "failed"}));

        assert!(result.is_err());
        assert_eq!(fs::read(temporary).expect("preserved raced file"), sentinel);
        assert!(directory.0.join(".unknown.in-progress.json").exists());
        assert!(!directory.0.join("unknown.json").exists());
    }

    #[test]
    fn commit_does_not_replace_a_final_file_created_after_reservation() {
        let directory = TestDirectory::new("raced-final");
        let reservation = ArtifactReservation::begin("unknown", &directory.0).expect("reserve run");
        let output = directory.0.join("unknown.json");
        let sentinel = b"final evidence owned by another writer\n";
        fs::write(&output, sentinel).expect("seed raced final evidence");

        let result = reservation.commit(&json!({"status": "failed"}));

        assert!(result.is_err());
        assert_eq!(fs::read(output).expect("preserved final file"), sentinel);
        assert!(directory.0.join(".unknown.in-progress.json").exists());
        assert!(!directory.0.join("unknown.json.tmp").exists());
    }

    #[test]
    fn marker_serializes_runners_and_final_is_rechecked_after_handoff() {
        let directory = TestDirectory::new("marker-linearization");
        let first = ArtifactReservation::begin("unknown", &directory.0).expect("first reserve");
        assert!(ArtifactReservation::begin("unknown", &directory.0).is_err());

        first
            .commit(&json!({"status": "failed"}))
            .expect("first commit");
        let original = fs::read(directory.0.join("unknown.json")).expect("first final");

        assert!(ArtifactReservation::begin("unknown", &directory.0).is_err());
        assert_eq!(
            fs::read(directory.0.join("unknown.json")).expect("preserved first final"),
            original
        );
        assert!(!directory.0.join(".unknown.in-progress.json").exists());
    }
}
