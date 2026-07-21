#![forbid(unsafe_code)]

mod artifact;
mod device;
mod faults;

use std::env;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(test)]
use artifact::RESERVATION_FILE_NAME;
use artifact::{ArtifactReservation, AuxiliaryKind, SEQUENCE_TIMINGS_FILE_NAME};
use device::{
    AdmissionDecision, AdmissionEvidence, AdmittedDevice, DeviceDiscovery, DeviceTargetRequest,
    SystemDeviceDiscovery, admit_device,
};
use easycon_controller::{
    AUTO_BAUD_RATES, AckFrame, AckRequest, AmiiboLimits, AmiiboSaveOptions, AmiiboSelectOptions,
    ConnectOptions, ControllerAction, ControllerLeaseState, ControllerOptions, ControllerSession,
    ControllerSnapshot, ControllerState, ControllerTransport, HandshakeRequest, PreciseSequence,
    SequenceStep, TransportError, WriteContext, WriteKind, WriteRequest,
};
use easycon_hardware_qualification::{distribution, option_value, required_value, value_or};
use easycon_model::{Button, Hat, ResourceId, StickPosition};
use easycon_runtime::{
    Clock, CloseOutcome, CloseRejection, Operation, OperationState, Runtime, RuntimeCounts,
    SystemClock, WaitResult, WaitTimeout,
};
use easycon_serial::{
    ByteIo, ByteIoFactory, ByteIoRequest, SerialControllerTransport, SerialError,
    SerialPortDescriptor, WindowsByteIoFactory, discover_system_ports,
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

#[derive(Clone, Debug)]
struct NativeOpenAttempt {
    baud: u32,
    error: Option<SerialError>,
}

#[derive(Clone, Debug)]
enum NeutralizationOutcome {
    Pending,
    Accepted,
    Failed(TransportError),
    Contradiction(String),
}

#[derive(Clone, Debug)]
struct NeutralizationAttempt {
    context: WriteContext,
    first_write_entered_ns: u64,
    accepted_bytes: usize,
    outcome: NeutralizationOutcome,
}

#[derive(Default)]
struct Telemetry {
    actual_baud: Option<u32>,
    handshake_attempts: Vec<HandshakeAttempt>,
    native_open_attempts: Vec<NativeOpenAttempt>,
    timings: Vec<TimingSample>,
    neutralization_attempts: Vec<NeutralizationAttempt>,
}

impl NeutralizationAttempt {
    fn contradict(&mut self, message: impl Into<String>) {
        if !matches!(self.outcome, NeutralizationOutcome::Contradiction(_)) {
            self.outcome = NeutralizationOutcome::Contradiction(message.into());
        }
    }
}

impl Telemetry {
    fn begin_neutralization(&mut self, context: WriteContext, remaining: usize, entered_ns: u64) {
        let offset = context.total_len.checked_sub(remaining);
        if let Some(attempt) = self
            .neutralization_attempts
            .iter_mut()
            .find(|attempt| attempt.context.sequence == context.sequence)
        {
            if attempt.context != context {
                attempt.contradict("neutralization context changed across partial writes");
            } else if !matches!(attempt.outcome, NeutralizationOutcome::Pending) {
                attempt.contradict("neutralization write resumed after a terminal observation");
            } else if offset != Some(attempt.accepted_bytes) {
                attempt.contradict("neutralization partial-write prefix was not contiguous");
            }
            return;
        }

        let mut attempt = NeutralizationAttempt {
            context,
            first_write_entered_ns: entered_ns,
            accepted_bytes: 0,
            outcome: NeutralizationOutcome::Pending,
        };
        if offset != Some(0) {
            attempt.contradict("neutralization write began without the full logical payload");
        }
        self.neutralization_attempts.push(attempt);
    }

    fn finish_neutralization(
        &mut self,
        context: WriteContext,
        remaining: usize,
        result: &Result<usize, TransportError>,
    ) {
        let Some(attempt) = self
            .neutralization_attempts
            .iter_mut()
            .find(|attempt| attempt.context.sequence == context.sequence)
        else {
            self.neutralization_attempts.push(NeutralizationAttempt {
                context,
                first_write_entered_ns: context.timestamp_ns,
                accepted_bytes: 0,
                outcome: NeutralizationOutcome::Contradiction(
                    "neutralization result had no matching intent".to_owned(),
                ),
            });
            return;
        };
        if matches!(attempt.outcome, NeutralizationOutcome::Contradiction(_)) {
            return;
        }
        let Some(offset) = context.total_len.checked_sub(remaining) else {
            attempt.contradict("neutralization remainder exceeded the logical payload");
            return;
        };
        if attempt.context != context || attempt.accepted_bytes != offset {
            attempt.contradict("neutralization result did not match its logical prefix");
            return;
        }
        if !matches!(attempt.outcome, NeutralizationOutcome::Pending) {
            attempt.contradict("neutralization produced more than one terminal result");
            return;
        }

        match result {
            Ok(written) => {
                if *written == 0 || *written > remaining {
                    attempt.contradict("neutralization transport returned invalid write progress");
                    return;
                }
                let Some(accepted) = offset.checked_add(*written) else {
                    attempt.contradict("neutralization accepted-byte count overflowed");
                    return;
                };
                if accepted > context.total_len {
                    attempt.contradict("neutralization accepted beyond the logical payload");
                    return;
                }
                attempt.accepted_bytes = accepted;
                if accepted == context.total_len {
                    attempt.outcome = NeutralizationOutcome::Accepted;
                }
            }
            Err(error) => attempt.outcome = NeutralizationOutcome::Failed(error.clone()),
        }
    }
}

struct ObservedByteIoFactory {
    inner: Box<dyn ByteIoFactory>,
    telemetry: Arc<Mutex<Telemetry>>,
}

impl ByteIoFactory for ObservedByteIoFactory {
    fn open(
        &mut self,
        port: &SerialPortDescriptor,
        baud_rate: u32,
        request: ByteIoRequest,
    ) -> Result<Box<dyn ByteIo>, SerialError> {
        let result = self.inner.open(port, baud_rate, request);
        self.telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .native_open_attempts
            .push(NativeOpenAttempt {
                baud: baud_rate,
                error: result.as_ref().err().cloned(),
            });
        result
    }
}

struct ObservedTransport {
    inner: Box<dyn ControllerTransport>,
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
        if context.kind == WriteKind::Neutralize {
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .begin_neutralization(context, remaining, entered);
        }
        let result = self.inner.write(request);
        let accepted_at = self.clock.now_ns();
        if context.kind == WriteKind::Neutralize {
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .finish_neutralization(context, remaining, &result);
        }
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

impl Harness {
    fn new(target: &AdmittedDevice, options: ControllerOptions) -> Result<Self, String> {
        let descriptor = target.descriptor().clone();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let observed_factory = ObservedByteIoFactory {
            inner: Box::new(WindowsByteIoFactory),
            telemetry: telemetry.clone(),
        };
        let serial = SerialControllerTransport::new(
            clock.clone(),
            descriptor.clone(),
            Box::new(observed_factory),
        );
        let observed = ObservedTransport {
            inner: Box::new(serial),
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

    #[cfg(test)]
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
        let pre_close = self.controller.snapshot();
        let controller_resource_id = self.controller.id();
        let evidence_boundary = self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .neutralization_attempts
            .len();
        self.controller.close();
        let post_close = self.controller.snapshot();
        let attempts = self
            .telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .neutralization_attempts[evidence_boundary..]
            .to_vec();
        let controller =
            controller_cleanup_json(controller_resource_id, pre_close, post_close, &attempts);
        let runtime = runtime_close_json(self.runtime.close(), self.runtime.counts());
        harness_cleanup_json(controller, runtime)
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

    fn native_open_attempts(&self) -> Value {
        native_open_attempts_json(&self.telemetry)
    }
}

struct CommandFailure {
    stage: &'static str,
    message: String,
    operation: Option<Operation>,
}

impl CommandFailure {
    fn new(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
            operation: None,
        }
    }

    fn with_operation(
        stage: &'static str,
        message: impl Into<String>,
        operation: Operation,
    ) -> Self {
        Self {
            stage,
            message: message.into(),
            operation: Some(operation),
        }
    }
}

fn finish_harness_phase(
    harness: Harness,
    mut result: Value,
    cleanup_field: &'static str,
    evidence_field: &'static str,
    close_stage: &'static str,
    execute: impl FnOnce(&Harness, &mut Value) -> Result<(), CommandFailure>,
) -> Value {
    let mut failure = execute(&harness, &mut result).err();
    if let Some(operation) = failure
        .as_ref()
        .and_then(|failure| failure.operation.as_ref())
    {
        result["failed_operation_at_failure"] = operation_json(operation);
        if !operation.snapshot().state.is_terminal() {
            let cancel_outcome = operation.cancel();
            result["recovery_cancel_outcome"] = json!(format!("{cancel_outcome:?}"));
            if let Err(error) = wait_terminal(operation, OPERATION_TIMEOUT) {
                result["recovery_settle_error"] = json!(error);
            }
        }
    }

    result[evidence_field] = harness_evidence_json(&harness);
    let cleanup = harness.close();
    if let Some(operation) = failure
        .as_ref()
        .and_then(|failure| failure.operation.as_ref())
    {
        result["failed_operation_post_close"] = operation_json(operation);
    }
    let cleanup_complete = cleanup_succeeded(&cleanup);
    result[cleanup_field] = cleanup;

    if !cleanup_complete {
        let close_failure =
            CommandFailure::new(close_stage, format!("{cleanup_field} did not complete"));
        if failure.is_some() {
            append_cleanup_error(&mut result, &close_failure);
        } else {
            failure = Some(close_failure);
        }
    }
    if let Some(failure) = failure {
        result["execution_error"] = json!({
            "stage": failure.stage,
            "message": failure.message,
        });
    }
    result
}

fn append_cleanup_error(result: &mut Value, failure: &CommandFailure) {
    let entry = json!({
        "stage": failure.stage,
        "message": failure.message,
    });
    match result.get_mut("cleanup_errors") {
        Some(Value::Array(errors)) => errors.push(entry),
        _ => result["cleanup_errors"] = json!([entry]),
    }
}

fn set_command_failure(result: &mut Value, failure: CommandFailure) {
    result["execution_error"] = json!({
        "stage": failure.stage,
        "message": failure.message,
    });
}

fn harness_evidence_json(harness: &Harness) -> Value {
    json!({
        "port": port_json(&harness.descriptor),
        "actual_baud": harness.actual_baud(),
        "handshake_attempts": handshake_json(&harness.telemetry),
        "native_open_attempts": harness.native_open_attempts(),
        "pre_cleanup_snapshot": snapshot_json(&harness.controller),
    })
}

fn connect_for_command(
    harness: &Harness,
    options: ConnectOptions,
    admit_stage: &'static str,
    wait_stage: &'static str,
    terminal_stage: &'static str,
) -> Result<Value, CommandFailure> {
    let operation = harness
        .controller
        .connect(options)
        .map_err(|error| CommandFailure::new(admit_stage, error.to_string()))?;
    wait_for_command_success(operation, OPERATION_TIMEOUT, wait_stage, terminal_stage)
}

fn wait_for_command_success(
    operation: Operation,
    timeout: Duration,
    wait_stage: &'static str,
    terminal_stage: &'static str,
) -> Result<Value, CommandFailure> {
    let (operation, state) = wait_for_command_terminal(operation, timeout, wait_stage)?;
    if state == OperationState::Succeeded {
        Ok(operation_json(&operation))
    } else {
        let message = operation_failure(&operation);
        Err(CommandFailure::with_operation(
            terminal_stage,
            message,
            operation,
        ))
    }
}

fn wait_for_command_terminal(
    operation: Operation,
    timeout: Duration,
    wait_stage: &'static str,
) -> Result<(Operation, OperationState), CommandFailure> {
    let snapshot = match operation.wait(WaitTimeout::For(timeout)) {
        WaitResult::Completed(snapshot) => snapshot,
        WaitResult::Timeout => {
            return Err(CommandFailure::with_operation(
                wait_stage,
                format!("operation {} wait timed out", operation.id().get()),
                operation,
            ));
        }
    };
    if !snapshot.state.is_terminal() {
        return Err(CommandFailure::with_operation(
            wait_stage,
            format!(
                "operation {} wait returned nonterminal state {:?}",
                operation.id().get(),
                snapshot.state
            ),
            operation,
        ));
    }
    Ok((operation, snapshot.state))
}

fn run_with_device_admission(
    command: &'static str,
    arguments: &[String],
    runner: impl FnOnce(AdmittedDevice, Arc<dyn DeviceDiscovery>) -> Result<Value, String>,
) -> Result<Value, String> {
    run_with_device_admission_using(command, arguments, Arc::new(SystemDeviceDiscovery), runner)
}

fn run_with_device_admission_using(
    command: &'static str,
    arguments: &[String],
    discovery: Arc<dyn DeviceDiscovery>,
    runner: impl FnOnce(AdmittedDevice, Arc<dyn DeviceDiscovery>) -> Result<Value, String>,
) -> Result<Value, String> {
    let request = device_target_request(arguments)?;
    match admit_device(discovery.as_ref(), request.clone()) {
        Ok(AdmissionDecision::Admitted(target)) => {
            let evidence = target.evidence().clone();
            match runner(target, discovery) {
                Ok(result) => Ok(attach_admission_evidence(result, &evidence, "admitted")),
                Err(error) => Ok(attach_admission_evidence(
                    json!({
                        "command": command,
                        "execution_error": {
                            "stage": "admitted_runner_failure",
                            "message": error,
                        },
                    }),
                    &evidence,
                    "admitted",
                )),
            }
        }
        Ok(AdmissionDecision::Rejected(evidence)) => Ok(json!({
            "command": command,
            "device_target": device_target_json(evidence.request()),
            "identity_admission": admission_evidence_json(&evidence, "rejected"),
            "capability_inference": "none",
        })),
        Ok(AdmissionDecision::Ambiguous(evidence)) => Ok(json!({
            "command": command,
            "device_target": device_target_json(evidence.request()),
            "identity_admission": admission_evidence_json(&evidence, "ambiguous"),
            "capability_inference": "none",
            "execution_error": {
                "stage": "identity_admission",
                "message": "system discovery returned an ambiguous serial snapshot",
            },
        })),
        Err(error) => Ok(json!({
            "command": command,
            "device_target": device_target_json(&request),
            "identity_admission": {
                "status": "discovery_error",
                "reason": "discovery_error",
                "snapshot": [],
                "observed_expected": null,
                "observed_hint": null,
                "structured_error": serial_error_json(&error),
            },
            "capability_inference": "none",
            "execution_error": {
                "stage": "identity_admission_discovery",
                "message": error.message(),
            },
        })),
    }
}

fn device_target_request(arguments: &[String]) -> Result<DeviceTargetRequest, String> {
    let expected_identity = required_non_option_string(arguments, "--expected-identity")?;
    let port = required_non_option_string(arguments, "--port")?;
    DeviceTargetRequest::new(expected_identity, port)
}

fn required_non_option_string(arguments: &[String], option: &str) -> Result<String, String> {
    let value = option_value(arguments, option)?
        .ok_or_else(|| format!("missing required option {option}"))?;
    if value.starts_with('-') {
        return Err(format!("missing value for {option}"));
    }
    Ok(value.to_owned())
}

fn attach_admission_evidence(
    mut result: Value,
    evidence: &AdmissionEvidence,
    status: &str,
) -> Value {
    result["device_target"] = device_target_json(evidence.request());
    result["identity_admission"] = admission_evidence_json(evidence, status);
    result["capability_inference"] = json!("none");
    result
}

fn device_target_json(request: &DeviceTargetRequest) -> Value {
    json!({
        "expected_stable_id": request.expected_stable_id(),
        "initial_port_hint": request.initial_port_hint(),
    })
}

fn admission_evidence_json(evidence: &AdmissionEvidence, status: &str) -> Value {
    json!({
        "status": status,
        "reason": evidence.reason().map(|reason| reason.as_str()),
        "snapshot": evidence.snapshot().iter().map(port_json).collect::<Vec<_>>(),
        "observed_expected": evidence.expected().map(port_json),
        "observed_hint": evidence.hint().map(port_json),
        "structured_error": null,
    })
}

fn serial_error_json(error: &SerialError) -> Value {
    json!({
        "kind": format!("{:?}", error.kind()),
        "os_code": error.os_code(),
        "message": error.message(),
    })
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
    ArtifactReservation::validate_command(command)?;
    let artifact_dir = artifact_dir(&arguments)?;
    fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let mut reservation = ArtifactReservation::begin(command, &artifact_dir)?;
    let mut sequence_timings = None;
    let execution = match command {
        "discover" => run_discover(&arguments),
        "handshake" => run_with_device_admission("handshake", &arguments, |target, _| {
            run_handshake(&arguments, target)
        }),
        "smoke" => run_with_device_admission("smoke", &arguments, |target, _| {
            run_smoke(&arguments, target)
        }),
        "home-wake" => run_with_device_admission("home-wake", &arguments, |target, _| {
            run_home_wake(&arguments, target)
        }),
        "faults" => run_with_device_admission("faults", &arguments, |target, _| run_faults(target)),
        "hotplug" => run_with_device_admission("hotplug", &arguments, |target, discovery| {
            run_hotplug(&arguments, target, discovery)
        }),
        "lifecycle" => run_with_device_admission("lifecycle", &arguments, |target, discovery| {
            run_lifecycle(&arguments, target, discovery)
        }),
        "sequence" => run_with_device_admission("sequence", &arguments, |target, _| {
            run_sequence(&arguments, target).map(|(result, timings)| {
                sequence_timings = timings;
                result
            })
        }),
        "amiibo" if amiibo_write_authorized(&arguments) => {
            run_with_device_admission("amiibo", &arguments, |target, _| {
                run_amiibo(&arguments, target)
            })
        }
        "amiibo" => run_unauthorized_amiibo(),
        _ => Err(format!("unknown command: {command}")),
    };
    let (document, failure) = finalize_result(command, execution);
    let exit_code = document_exit_code(&document);
    if let Some(timings) = sequence_timings {
        reservation.stage_auxiliary(AuxiliaryKind::SequenceTimingsCsv, timings)?;
    }
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
        Ok(result) if execution_error(&result).is_some() => {
            let error = execution_error(&result).expect("guard checked execution error");
            let cleanup_status = if cleanup_contract_succeeded(command, &result) {
                "passed"
            } else {
                "incomplete_or_failed"
            };
            (
                json!({
                    "schema_version": 1,
                    "command": command,
                    "status": "failed",
                    "execution_status": "failed",
                    "qualification_status": "failed",
                    "checks": [
                        qualification_check("execution", "failed"),
                        qualification_check("cleanup_evidence", cleanup_status),
                    ],
                    "error": error.clone(),
                    "result": result,
                }),
                Some(error),
            )
        }
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

fn execution_error(result: &Value) -> Option<String> {
    let raw = result.get("execution_error")?;
    let parsed = raw.as_object().and_then(|error| {
        let stage = error.get("stage")?.as_str()?;
        let message = error.get("message")?.as_str()?;
        (!stage.trim().is_empty() && !message.trim().is_empty())
            .then(|| format!("{stage}: {message}"))
    });
    Some(parsed.unwrap_or_else(|| "malformed execution_error evidence".to_owned()))
}

fn qualification_decision(command: &str, result: &Value) -> QualificationDecision {
    if result["identity_admission"]["status"] == "rejected" {
        return QualificationDecision {
            status: QualificationStatus::NotRun,
            checks: vec![qualification_check("identity_admission", "not_run")],
            failure: None,
        };
    }
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
                "port_occupied_exact_error",
                operation_matches(&result["port_occupied"], "Failed", "Io", "Transport", None),
            ),
            (
                "port_occupied_exact_native_error",
                exact_port_busy_open_attempts(&result["port_occupied_native_open_attempts"]),
            ),
            (
                "cancel_exact_reason",
                operation_matches(
                    &result["cancel"],
                    "Cancelled",
                    "Runtime",
                    "Cancelled",
                    Some("Requested"),
                ),
            ),
            (
                "cancel_nonvacuous_and_neutralized",
                cancel_evidence_valid(result),
            ),
            (
                "deadline_exact_reason",
                operation_matches(
                    &result["deadline"],
                    "Cancelled",
                    "Runtime",
                    "DeadlineExceeded",
                    Some("Deadline"),
                ),
            ),
            (
                "deadline_did_not_open_port",
                result
                    .get("deadline_native_open_attempts")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty),
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

fn operation_matches(
    operation: &Value,
    state: &str,
    domain: &str,
    code: &str,
    cancellation_reason: Option<&str>,
) -> bool {
    let Some(operation) = operation.as_object() else {
        return false;
    };
    let Some(error) = operation.get("structured_error").and_then(Value::as_object) else {
        return false;
    };
    let reason_matches = match cancellation_reason {
        Some(reason) => {
            operation.get("cancellation_reason").and_then(Value::as_str) == Some(reason)
        }
        None => operation
            .get("cancellation_reason")
            .is_some_and(Value::is_null),
    };
    operation.get("state").and_then(Value::as_str) == Some(state)
        && error.get("domain").and_then(Value::as_str) == Some(domain)
        && error.get("code").and_then(Value::as_str) == Some(code)
        && error
            .get("message")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty())
        && reason_matches
}

fn exact_port_busy_open_attempts(value: &Value) -> bool {
    let Some(attempts) = value.as_array() else {
        return false;
    };
    attempts.len() == AUTO_BAUD_RATES.len()
        && attempts
            .iter()
            .zip(AUTO_BAUD_RATES)
            .all(|(attempt, expected_baud)| {
                let Some(attempt) = attempt.as_object() else {
                    return false;
                };
                let Some(error) = attempt.get("error").and_then(Value::as_object) else {
                    return false;
                };
                attempt.get("baud").and_then(Value::as_u64) == Some(u64::from(expected_baud))
                    && attempt.get("succeeded").and_then(Value::as_bool) == Some(false)
                    && error.get("kind").and_then(Value::as_str) == Some("PortBusy")
                    && error.get("os_code").and_then(Value::as_u64) == Some(32)
                    && error
                        .get("message")
                        .and_then(Value::as_str)
                        .is_some_and(|message| !message.is_empty())
            })
}

fn cancel_evidence_valid(result: &Value) -> bool {
    let Some(cancel_id) = result["cancel"]["id"].as_u64() else {
        return false;
    };
    let Some(baseline) = result["cancel_baseline_snapshot"].as_object() else {
        return false;
    };
    let Some(before_request) = result["cancel_before_request_snapshot"].as_object() else {
        return false;
    };
    let Some(after_cancel) = result["post_cancel_snapshot"].as_object() else {
        return false;
    };
    let Some(baseline_count) = baseline
        .get("accepted_report_count")
        .and_then(Value::as_u64)
    else {
        return false;
    };
    let Some(before_count) = baseline_count.checked_add(1) else {
        return false;
    };
    let Some(after_minimum) = before_count.checked_add(1) else {
        return false;
    };

    baseline.get("state").and_then(Value::as_str) == Some("Connected")
        && result["cancel_request_outcome"].as_str() == Some("Applied")
        && baseline
            .get("desired_report_neutral")
            .and_then(Value::as_bool)
            == Some(true)
        && lease_detail_matches(baseline.get("lease_detail"), "available", None)
        && before_request.get("state").and_then(Value::as_str) == Some("Connected")
        && before_request
            .get("accepted_report_count")
            .and_then(Value::as_u64)
            == Some(before_count)
        && before_request
            .get("desired_report_neutral")
            .and_then(Value::as_bool)
            == Some(false)
        && lease_detail_matches(
            before_request.get("lease_detail"),
            "sequence",
            Some(cancel_id),
        )
        && after_cancel.get("state").and_then(Value::as_str) == Some("Connected")
        && after_cancel
            .get("accepted_report_count")
            .and_then(Value::as_u64)
            == Some(after_minimum)
        && after_cancel
            .get("desired_report_neutral")
            .and_then(Value::as_bool)
            == Some(true)
        && lease_detail_matches(after_cancel.get("lease_detail"), "available", None)
}

fn lease_detail_matches(value: Option<&Value>, kind: &str, operation_id: Option<u64>) -> bool {
    let Some(value) = value.and_then(Value::as_object) else {
        return false;
    };
    value.get("kind").and_then(Value::as_str) == Some(kind)
        && match operation_id {
            Some(operation_id) => {
                value.len() == 2
                    && value.get("operation_id").and_then(Value::as_u64) == Some(operation_id)
            }
            None => value.len() == 1,
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

const FAULT_CLEANUP_ROLES: [(&str, &str); 4] = [
    ("occupier", "occupier_cleanup"),
    ("occupied_probe", "occupied_probe_cleanup"),
    ("cancel", "cancel_cleanup"),
    ("deadline", "deadline_cleanup"),
];
const FAULT_SCENARIOS: [&str; 3] = ["port_occupied", "cancel", "deadline"];

fn cleanup_contract_succeeded(command: &str, result: &Value) -> bool {
    if result["identity_admission"]["status"] == "rejected" {
        return pre_harness_admission_contract_succeeded(result, "rejected");
    }
    if result.get("execution_error").is_some()
        && matches!(
            result["identity_admission"]["status"].as_str(),
            Some("ambiguous" | "discovery_error")
        )
    {
        return pre_harness_admission_contract_succeeded(
            result,
            result["identity_admission"]["status"]
                .as_str()
                .expect("matched admission status"),
        );
    }
    if result.get("execution_error").is_some() {
        return match command {
            "faults" => partial_fault_cleanup_contract_succeeded(result),
            "hotplug" => partial_hotplug_cleanup_contract_succeeded(result),
            "lifecycle" => partial_lifecycle_cleanup_contract_succeeded(result),
            "handshake" | "smoke" | "home-wake" | "sequence" | "amiibo" => {
                partial_single_harness_cleanup_contract_succeeded(result)
            }
            _ => false,
        };
    }

    let expected_count = match command {
        "handshake" | "smoke" | "home-wake" | "sequence" => 1,
        "faults" => 4,
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
            complete_fault_projection_succeeded(result)
                && FAULT_CLEANUP_ROLES
                    .iter()
                    .all(|(_, cleanup)| cleanup_succeeded(&result[*cleanup]))
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

fn pre_harness_admission_contract_succeeded(result: &Value, expected_status: &str) -> bool {
    let Some(target) = result.get("device_target").and_then(Value::as_object) else {
        return false;
    };
    let Some(admission) = result.get("identity_admission").and_then(Value::as_object) else {
        return false;
    };
    target.len() == 2
        && target
            .get("expected_stable_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        && target
            .get("initial_port_hint")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
        && admission.get("status").and_then(Value::as_str) == Some(expected_status)
        && admission
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| {
                matches!(
                    (expected_status, reason),
                    (
                        "rejected",
                        "expected_absent"
                            | "expected_at_different_port"
                            | "hint_owned_by_different_identity"
                    ) | ("ambiguous", "ambiguous_snapshot")
                        | ("discovery_error", "discovery_error")
                )
            })
        && admission.get("snapshot").is_some_and(Value::is_array)
        && cleanup_slot_count(result) == 0
        && runtime_cleanup_count(result) == 0
}

fn partial_single_harness_cleanup_contract_succeeded(result: &Value) -> bool {
    primary_resource_was_created(result)
        && cleanup_slot_count(result) == 1
        && runtime_cleanup_count(result) == 1
        && cleanup_succeeded(&result["cleanup"])
}

fn primary_resource_was_created(result: &Value) -> bool {
    result
        .get("resources")
        .and_then(Value::as_object)
        .is_some_and(|resources| {
            resources.len() == 1
                && resources
                    .get("primary")
                    .and_then(Value::as_object)
                    .is_some_and(|primary| {
                        primary.len() == 1
                            && primary.get("created").and_then(Value::as_bool) == Some(true)
                    })
        })
}

fn partial_hotplug_cleanup_contract_succeeded(result: &Value) -> bool {
    let Some(resources) = result.get("resources").and_then(Value::as_object) else {
        return false;
    };
    if resources.len() != 2 {
        return false;
    }
    let created = |role: &str| {
        resources
            .get(role)
            .and_then(Value::as_object)
            .and_then(|role| {
                (role.len() == 1)
                    .then(|| role.get("created").and_then(Value::as_bool))
                    .flatten()
            })
    };
    let initial_created = created("initial");
    let reconnected_created = created("reconnected");
    if initial_created != Some(true) || reconnected_created.is_none() {
        return false;
    }
    let reconnected_created = reconnected_created == Some(true);
    let Some(stage) = result["execution_error"]["stage"].as_str() else {
        return false;
    };
    let stage_requires_reconnected = match stage {
        "hotplug_initial_connect_admit"
        | "hotplug_initial_connect_wait"
        | "hotplug_initial_connect_terminal"
        | "hotplug_unplug_marker_flush"
        | "hotplug_wait_absent"
        | "hotplug_disconnect_admit"
        | "hotplug_disconnect_wait"
        | "hotplug_initial_close"
        | "hotplug_reconnect_marker_flush"
        | "hotplug_wait_return"
        | "hotplug_reconnected_create" => false,
        "hotplug_reconnect_admit"
        | "hotplug_reconnect_wait"
        | "hotplug_reconnect_terminal"
        | "hotplug_reconnected_close" => true,
        _ => return false,
    };
    if reconnected_created != stage_requires_reconnected {
        return false;
    }
    let result_object = result
        .as_object()
        .expect("resources require an object result");
    if !result_object.contains_key("disconnect_cleanup")
        || result_object.contains_key("cleanup") != reconnected_created
    {
        return false;
    }
    let expected = 1 + usize::from(reconnected_created);
    cleanup_slot_count(result) == expected
        && runtime_cleanup_count(result) == expected
        && cleanup_succeeded(&result["disconnect_cleanup"])
        && (!reconnected_created || cleanup_succeeded(&result["cleanup"]))
}

fn partial_lifecycle_cleanup_contract_succeeded(result: &Value) -> bool {
    let Some(resources) = result.get("resources").and_then(Value::as_object) else {
        return false;
    };
    let Some(records) = result.get("records").and_then(Value::as_array) else {
        return false;
    };
    let created_cycles = resources
        .get("created_cycles")
        .and_then(Value::as_u64)
        .and_then(|cycles| usize::try_from(cycles).ok());
    let requested_cycles = result
        .get("cycles")
        .and_then(Value::as_u64)
        .and_then(|cycles| usize::try_from(cycles).ok());
    if resources.len() != 1
        || created_cycles != Some(records.len())
        || requested_cycles.is_none_or(|cycles| cycles == 0 || records.len() > cycles)
    {
        return false;
    }
    let records_are_structurally_valid = records.iter().enumerate().all(|(index, record)| {
        record.get("cycle").and_then(Value::as_u64) == u64::try_from(index + 1).ok()
            && primary_resource_was_created(record)
            && cleanup_succeeded(&record["cleanup"])
    });
    if !records_are_structurally_valid {
        return false;
    }
    let Some(stage) = result["execution_error"]["stage"].as_str() else {
        return false;
    };
    let statuses: Vec<_> = records
        .iter()
        .map(|record| record.get("status").and_then(Value::as_str))
        .collect();
    let all_completed = statuses.iter().all(|status| *status == Some("completed"));
    let completed_then_failed = statuses.last() == Some(&Some("failed"))
        && statuses[..statuses.len().saturating_sub(1)]
            .iter()
            .all(|status| *status == Some("completed"));
    let failed_cycle = result
        .get("failed_cycle")
        .and_then(Value::as_u64)
        .and_then(|cycle| usize::try_from(cycle).ok());
    let stage_layout_is_valid = match stage {
        "lifecycle_create" => all_completed && failed_cycle == Some(records.len() + 1),
        "lifecycle_connect_admit"
        | "lifecycle_connect_wait"
        | "lifecycle_connect_terminal"
        | "lifecycle_close"
        | "lifecycle_metrics"
        | "lifecycle_post_cycle_ambiguous"
        | "lifecycle_post_cycle_discovery" => {
            completed_then_failed && failed_cycle == Some(records.len())
        }
        "lifecycle_final_metrics" => {
            all_completed && records.len() == requested_cycles.expect("validated cycles")
        }
        _ => false,
    };
    stage_layout_is_valid
        && cleanup_slot_count(result) == records.len()
        && runtime_cleanup_count(result) == records.len()
}

fn complete_fault_projection_succeeded(result: &Value) -> bool {
    let Some(resources) = result.get("resources").and_then(Value::as_object) else {
        return false;
    };
    if resources.len() != FAULT_CLEANUP_ROLES.len()
        || FAULT_CLEANUP_ROLES.iter().any(|(role, _)| {
            resources
                .get(*role)
                .and_then(|resource| resource.get("created"))
                .and_then(Value::as_bool)
                != Some(true)
        })
    {
        return false;
    }

    let Some(scenarios) = result.get("scenarios").and_then(Value::as_object) else {
        return false;
    };
    scenarios.len() == FAULT_SCENARIOS.len()
        && FAULT_SCENARIOS.iter().all(|scenario| {
            scenarios
                .get(*scenario)
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str)
                == Some("completed")
        })
}

fn partial_fault_cleanup_contract_succeeded(result: &Value) -> bool {
    let Some(result_object) = result.as_object() else {
        return false;
    };
    let Some(resources) = result.get("resources").and_then(Value::as_object) else {
        return false;
    };
    if resources.len() != FAULT_CLEANUP_ROLES.len()
        || FAULT_CLEANUP_ROLES
            .iter()
            .any(|(role, _)| !resources.contains_key(*role))
    {
        return false;
    }

    let mut expected_cleanup_count = 0;
    let mut found_uncreated_role = false;
    for (role, cleanup_field) in FAULT_CLEANUP_ROLES {
        let Some(created) = resources[role].get("created").and_then(Value::as_bool) else {
            return false;
        };
        if created && found_uncreated_role {
            return false;
        }
        found_uncreated_role |= !created;

        let cleanup_present = result_object.contains_key(cleanup_field);
        if cleanup_present != created {
            return false;
        }
        if created {
            expected_cleanup_count += 1;
            if !cleanup_succeeded(&result[cleanup_field]) {
                return false;
            }
        }
    }

    let Some(scenarios) = result.get("scenarios").and_then(Value::as_object) else {
        return false;
    };
    if scenarios.len() != FAULT_SCENARIOS.len()
        || FAULT_SCENARIOS
            .iter()
            .any(|scenario| !scenarios.contains_key(*scenario))
    {
        return false;
    }
    let Some(stage) = result["execution_error"]["stage"].as_str() else {
        return false;
    };
    let cleanup_failure_stage = matches!(
        stage,
        "occupied_probe_close" | "occupier_close" | "cancel_close" | "deadline_close"
    );
    let Some((failed_scenario, expected_created_roles)) = partial_fault_stage_contract(stage)
    else {
        return false;
    };
    if expected_cleanup_count != expected_created_roles {
        return false;
    }
    let status = |scenario: &str| scenarios[scenario].get("status").and_then(Value::as_str);
    let evidence_order_is_valid = match failed_scenario {
        "port_occupied" => {
            status("port_occupied") == Some("failed")
                && status("cancel") == Some("not_run")
                && status("deadline") == Some("not_run")
        }
        "cancel" => {
            status("port_occupied") == Some("completed")
                && status("cancel") == Some("failed")
                && status("deadline") == Some("not_run")
        }
        "deadline" => {
            status("port_occupied") == Some("completed")
                && status("cancel") == Some("completed")
                && status("deadline") == Some("failed")
        }
        _ => false,
    };

    !cleanup_failure_stage
        && evidence_order_is_valid
        && cleanup_slot_count(result) == expected_cleanup_count
        && runtime_cleanup_count(result) == expected_cleanup_count
}

fn partial_fault_stage_contract(stage: &str) -> Option<(&'static str, usize)> {
    match stage {
        "occupier_create" => Some(("port_occupied", 0)),
        "occupier_connect_admit"
        | "occupier_connect_wait"
        | "occupier_connect_terminal"
        | "occupied_probe_create" => Some(("port_occupied", 1)),
        "occupied_probe_connect_admit"
        | "occupied_probe_connect_wait"
        | "occupied_probe_connect_terminal"
        | "occupied_probe_close"
        | "occupier_close" => Some(("port_occupied", 2)),
        "cancel_create" => Some(("cancel", 2)),
        "cancel_connect_admit"
        | "cancel_connect_wait"
        | "cancel_connect_terminal"
        | "cancel_sequence_build"
        | "cancel_sequence_admit"
        | "cancel_readiness"
        | "cancel_request"
        | "cancel_terminal_wait"
        | "cancel_operation_terminal"
        | "cancel_close" => Some(("cancel", 3)),
        "deadline_create" => Some(("deadline", 3)),
        "deadline_connect_admit"
        | "deadline_terminal_wait"
        | "deadline_connect_terminal"
        | "deadline_close" => Some(("deadline", 4)),
        _ => None,
    }
}

fn cleanup_succeeded(value: &Value) -> bool {
    if !object_has_exact_keys(value, &["kind", "succeeded", "controller", "runtime"])
        || value["kind"] != "harness_cleanup"
    {
        return false;
    }
    let succeeded = controller_cleanup_succeeded(&value["controller"])
        && runtime_cleanup_succeeded(&value["runtime"]);
    value["succeeded"].as_bool() == Some(succeeded) && succeeded
}

fn runtime_cleanup_succeeded(value: &Value) -> bool {
    object_has_exact_keys(
        value,
        &["kind", "succeeded", "outcome", "counts", "diagnostic"],
    ) && value["kind"] == "runtime_cleanup"
        && value["succeeded"].as_bool() == Some(true)
        && value["outcome"] == "Closed"
        && zero_runtime_counts(&value["counts"])
        && value.get("diagnostic") == Some(&Value::Null)
}

fn controller_cleanup_succeeded(value: &Value) -> bool {
    let succeeded = controller_cleanup_predicate(value);
    value["succeeded"].as_bool() == Some(succeeded) && succeeded
}

fn controller_cleanup_predicate(value: &Value) -> bool {
    if !object_has_exact_keys(
        value,
        &[
            "kind",
            "succeeded",
            "controller_resource_id",
            "pre_close_state",
            "post_close_state",
            "post_close_desired_report_neutral",
            "post_close_lease",
            "neutralization",
            "attempts",
        ],
    ) || value["kind"] != "controller_cleanup"
    {
        return false;
    }
    let Some(controller_resource_id) = value["controller_resource_id"]
        .as_u64()
        .filter(|resource_id| *resource_id != 0)
    else {
        return false;
    };
    let Some(pre_close_state) = value["pre_close_state"].as_str() else {
        return false;
    };
    if !matches!(
        pre_close_state,
        "Disconnected" | "Connecting" | "Connected" | "Disconnecting" | "Closed"
    ) {
        return false;
    }
    let Some(attempts) = value["attempts"].as_array() else {
        return false;
    };
    let attempts_are_valid = attempts.iter().all(neutralization_attempt_is_valid)
        && attempts.windows(2).all(|pair| {
            pair[0]["sequence"]
                .as_u64()
                .zip(pair[1]["sequence"].as_u64())
                .is_some_and(|(left, right)| left < right)
        })
        && attempts
            .iter()
            .all(|attempt| attempt["resource_id"].as_u64() == Some(controller_resource_id));
    let final_attempts: Vec<_> = attempts
        .iter()
        .filter(|attempt| attempt.get("operation_id") == Some(&Value::Null))
        .collect();
    let expected_neutralization = if pre_close_state != "Connected" {
        if final_attempts.is_empty() {
            "not_required"
        } else {
            "evidence_incomplete"
        }
    } else if final_attempts.len() != 1 {
        "evidence_incomplete"
    } else if neutralization_attempt_was_accepted(final_attempts[0]) {
        "accepted"
    } else if final_attempts[0]["outcome"] == "failed" {
        "not_delivered"
    } else {
        "evidence_incomplete"
    };
    let final_attempt_rule = if pre_close_state == "Connected" {
        final_attempts.len() == 1
            && neutralization_attempt_was_accepted(final_attempts[0])
            && attempts
                .last()
                .and_then(|attempt| attempt["sequence"].as_u64())
                == final_attempts[0]["sequence"].as_u64()
    } else {
        final_attempts.is_empty()
    };

    attempts_are_valid
        && value["neutralization"].as_str() == Some(expected_neutralization)
        && final_attempt_rule
        && value["post_close_state"] == "Closed"
        && value["post_close_desired_report_neutral"].as_bool() == Some(true)
        && value["post_close_lease"] == "Available"
}

fn neutralization_attempt_is_valid(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !object_has_exact_keys(
        value,
        &[
            "resource_id",
            "operation_id",
            "sequence",
            "dispatch_timestamp_ns",
            "first_write_entered_ns",
            "total_bytes",
            "accepted_bytes",
            "outcome",
            "structured_error",
            "diagnostic",
        ],
    ) {
        return false;
    }
    let operation_id_is_valid = match object.get("operation_id") {
        Some(Value::Null) => true,
        Some(value) => value.as_u64().is_some_and(|id| id != 0),
        None => false,
    };
    let Some(total_bytes) = value["total_bytes"].as_u64() else {
        return false;
    };
    let Some(accepted_bytes) = value["accepted_bytes"].as_u64() else {
        return false;
    };
    let common = value["resource_id"].as_u64().is_some_and(|id| id != 0)
        && value["sequence"]
            .as_u64()
            .is_some_and(|sequence| sequence != 0)
        && operation_id_is_valid
        && value["dispatch_timestamp_ns"].is_u64()
        && value["first_write_entered_ns"].is_u64()
        && total_bytes == 8
        && accepted_bytes <= total_bytes;
    if !common {
        return false;
    }

    match value["outcome"].as_str() {
        Some("accepted") => {
            accepted_bytes == total_bytes
                && object.get("structured_error") == Some(&Value::Null)
                && object.get("diagnostic") == Some(&Value::Null)
        }
        Some("failed") => {
            accepted_bytes < total_bytes
                && structured_transport_error_is_valid(&value["structured_error"])
                && object.get("diagnostic") == Some(&Value::Null)
        }
        Some("pending" | "contradiction") => false,
        Some(_) | None => false,
    }
}

fn structured_transport_error_is_valid(value: &Value) -> bool {
    object_has_exact_keys(value, &["kind", "message"])
        && value["kind"].as_str().is_some_and(|kind| {
            matches!(
                kind,
                "Timeout" | "WriteTimeout" | "Cancelled" | "Disconnected" | "Io" | "Protocol"
            )
        })
        && value["message"]
            .as_str()
            .is_some_and(|message| !message.trim().is_empty())
}

fn neutralization_attempt_was_accepted(value: &Value) -> bool {
    value["operation_id"].is_null()
        && value["total_bytes"].as_u64() == Some(8)
        && value["accepted_bytes"].as_u64() == Some(8)
        && value["outcome"] == "accepted"
        && value.get("structured_error") == Some(&Value::Null)
}

fn zero_runtime_counts(value: &Value) -> bool {
    object_has_exact_keys(
        value,
        &["active_operations", "active_resources", "active_tasks"],
    ) && value["active_operations"].as_u64() == Some(0)
        && value["active_resources"].as_u64() == Some(0)
        && value["active_tasks"].as_u64() == Some(0)
}

fn object_has_exact_keys(value: &Value, expected: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == expected.len() && expected.iter().all(|key| object.contains_key(*key))
    })
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

fn run_handshake(_arguments: &[String], target: AdmittedDevice) -> Result<Value, String> {
    let harness = Harness::new(&target, ControllerOptions::default())?;
    let result = json!({
        "command": "handshake",
        "port": port_json(&harness.descriptor),
        "resources": {"primary": {"created": true}},
    });
    Ok(finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "handshake_close",
        |harness, result| {
            result["operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "handshake_connect_admit",
                "handshake_connect_wait",
                "handshake_connect_terminal",
            )?;
            result["actual_baud"] = json!(harness.actual_baud());
            result["attempts"] = handshake_json(&harness.telemetry);
            Ok(())
        },
    ))
}

fn run_smoke(arguments: &[String], target: AdmittedDevice) -> Result<Value, String> {
    let full = arguments.iter().any(|argument| argument == "--full");
    let wake_left_stick = arguments
        .iter()
        .any(|argument| argument == "--wake-left-stick");
    let wake_home = arguments.iter().any(|argument| argument == "--wake-home");
    let hold_ms = value_or(arguments, "--hold-ms", 0_u64)?;
    validate_hold_ms(hold_ms)?;
    let harness = Harness::new(&target, ControllerOptions::default())?;
    let result = json!({
        "command": "smoke",
        "stage": if full { "full" } else { "a_only" },
        "wake_left_stick": wake_left_stick,
        "wake_home": wake_home,
        "wake_home_to_a_delay_ms": wake_home.then_some(3_000),
        "a_hold_ms": hold_ms,
        "port": port_json(&harness.descriptor),
        "actions": [],
        "switch_observation": "requires operator confirmation",
        "resources": {"primary": {"created": true}},
    });
    Ok(finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "smoke_close",
        |harness, result| {
            result["connect_operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "smoke_connect_admit",
                "smoke_connect_wait",
                "smoke_connect_terminal",
            )?;
            if wake_home {
                for (label, action) in home_wake_actions() {
                    exercise_action_for_command(harness, label, action, &mut result["actions"])?;
                }
                thread::sleep(Duration::from_secs(3));
            }
            if wake_left_stick {
                for (label, action) in left_stick_wake_actions() {
                    exercise_action_for_command(harness, label, action, &mut result["actions"])?;
                }
            }
            exercise_action_for_command(
                harness,
                "button.A.down",
                ControllerAction::ButtonDown(Button::A),
                &mut result["actions"],
            )?;
            if hold_ms != 0 {
                thread::sleep(Duration::from_millis(hold_ms));
            }
            exercise_action_for_command(
                harness,
                "button.A.up",
                ControllerAction::ButtonUp(Button::A),
                &mut result["actions"],
            )?;
            exercise_action_for_command(
                harness,
                "neutral",
                ControllerAction::Reset,
                &mut result["actions"],
            )?;

            if full {
                for button in Button::ALL {
                    if button != Button::A {
                        exercise_action_for_command(
                            harness,
                            &format!("button.{button:?}.down"),
                            ControllerAction::ButtonDown(button),
                            &mut result["actions"],
                        )?;
                        exercise_action_for_command(
                            harness,
                            &format!("button.{button:?}.up"),
                            ControllerAction::ButtonUp(button),
                            &mut result["actions"],
                        )?;
                        exercise_action_for_command(
                            harness,
                            "neutral",
                            ControllerAction::Reset,
                            &mut result["actions"],
                        )?;
                    }
                }
                for hat in Hat::ALL.into_iter().filter(|hat| *hat != Hat::Center) {
                    exercise_action_for_command(
                        harness,
                        &format!("hat.{hat:?}"),
                        ControllerAction::Hat(hat),
                        &mut result["actions"],
                    )?;
                    exercise_action_for_command(
                        harness,
                        "hat.Center",
                        ControllerAction::Hat(Hat::Center),
                        &mut result["actions"],
                    )?;
                    exercise_action_for_command(
                        harness,
                        "neutral",
                        ControllerAction::Reset,
                        &mut result["actions"],
                    )?;
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
                        exercise_action_for_command(
                            harness,
                            &format!("stick.{side}.{},{}", position.x, position.y),
                            constructor(position),
                            &mut result["actions"],
                        )?;
                        exercise_action_for_command(
                            harness,
                            &format!("stick.{side}.center"),
                            constructor(StickPosition::CENTER),
                            &mut result["actions"],
                        )?;
                        exercise_action_for_command(
                            harness,
                            "neutral",
                            ControllerAction::Reset,
                            &mut result["actions"],
                        )?;
                    }
                }
            }
            result["actual_baud"] = json!(harness.actual_baud());
            result["final_snapshot"] = snapshot_json(&harness.controller);
            result["latency"] = latency_json(&harness.telemetry, harness.actual_baud());
            Ok(())
        },
    ))
}

fn run_home_wake(arguments: &[String], target: AdmittedDevice) -> Result<Value, String> {
    let attempts = value_or(arguments, "--attempts", 20_usize)?;
    let interval_seconds = value_or(arguments, "--interval-seconds", 3_u64)?;
    validate_home_wake(attempts, interval_seconds)?;
    let harness = Harness::new(&target, ControllerOptions::default())?;
    let result = json!({
        "command": "home-wake",
        "attempts": attempts,
        "interval_seconds": interval_seconds,
        "port": port_json(&harness.descriptor),
        "records": [],
        "switch_observation": "requires operator confirmation",
        "resources": {"primary": {"created": true}},
    });
    Ok(finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "home_wake_close",
        |harness, result| {
            result["connect_operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "home_wake_connect_admit",
                "home_wake_connect_wait",
                "home_wake_connect_terminal",
            )?;
            let started = Instant::now();
            for attempt in 0..attempts {
                let target = Duration::from_secs(
                    u64::try_from(attempt)
                        .expect("bounded attempt index fits u64")
                        .saturating_mul(interval_seconds),
                );
                if let Some(remaining) = target.checked_sub(started.elapsed()) {
                    thread::sleep(remaining);
                }
                let attempt_started_ms =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                result["records"]
                    .as_array_mut()
                    .expect("home-wake records are an array")
                    .push(json!({
                        "attempt": attempt + 1,
                        "started_after_ms": attempt_started_ms,
                        "status": "running",
                        "actions": [],
                    }));
                let record = result["records"]
                    .as_array_mut()
                    .expect("home-wake records are an array")
                    .last_mut()
                    .expect("current home-wake record exists");
                exercise_action_for_command(
                    harness,
                    "button.Home.down",
                    ControllerAction::ButtonDown(Button::Home),
                    &mut record["actions"],
                )?;
                println!("{}", home_attempt_marker(attempt + 1));
                std::io::stdout().flush().map_err(|error| {
                    CommandFailure::new("home_wake_marker_flush", error.to_string())
                })?;
                exercise_action_for_command(
                    harness,
                    "button.Home.up",
                    ControllerAction::ButtonUp(Button::Home),
                    &mut record["actions"],
                )?;
                exercise_action_for_command(
                    harness,
                    "neutral",
                    ControllerAction::Reset,
                    &mut record["actions"],
                )?;
                record["status"] = json!("completed");
            }
            result["actual_baud"] = json!(harness.actual_baud());
            result["final_snapshot"] = snapshot_json(&harness.controller);
            result["latency"] = latency_json(&harness.telemetry, harness.actual_baud());
            Ok(())
        },
    ))
}

fn run_faults(target: AdmittedDevice) -> Result<Value, String> {
    Ok(faults::run(target))
}

fn run_hotplug(
    arguments: &[String],
    target: AdmittedDevice,
    _discovery: Arc<dyn DeviceDiscovery>,
) -> Result<Value, String> {
    let timeout_seconds = value_or(arguments, "--timeout-seconds", 180_u64)?;
    let port = target.descriptor().port_name().to_owned();
    let harness = Harness::new(&target, ControllerOptions::default())?;
    let stable_id = harness.descriptor.stable_id().to_owned();
    let result = json!({
        "command": "hotplug",
        "stable_id": stable_id,
        "initial_port": port_json(&harness.descriptor),
        "resources": {
            "initial": {"created": true},
            "reconnected": {"created": false},
        },
    });
    let mut result = finish_harness_phase(
        harness,
        result,
        "disconnect_cleanup",
        "initial_harness_evidence",
        "hotplug_initial_close",
        |harness, result| {
            result["initial_connect_operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "hotplug_initial_connect_admit",
                "hotplug_initial_connect_wait",
                "hotplug_initial_connect_terminal",
            )?;
            println!("HOTPLUG_READY: unplug {port} now");
            std::io::stdout().flush().map_err(|error| {
                CommandFailure::new("hotplug_unplug_marker_flush", error.to_string())
            })?;
            wait_for_presence(&stable_id, false, Duration::from_secs(timeout_seconds))
                .map_err(|error| CommandFailure::new("hotplug_wait_absent", error))?;
            let disconnected = harness.controller.reset().map_err(|error| {
                CommandFailure::new("hotplug_disconnect_admit", error.to_string())
            })?;
            let (disconnected, state) = wait_for_command_terminal(
                disconnected,
                OPERATION_TIMEOUT,
                "hotplug_disconnect_wait",
            )?;
            result["disconnect_operation"] = operation_json(&disconnected);
            result["disconnect_detected"] = json!(state == OperationState::Failed);
            Ok(())
        },
    );
    if result.get("execution_error").is_some() {
        return Ok(result);
    }

    println!("HOTPLUG_DISCONNECTED: reconnect the same device now");
    if let Err(error) = std::io::stdout().flush() {
        set_command_failure(
            &mut result,
            CommandFailure::new("hotplug_reconnect_marker_flush", error.to_string()),
        );
        return Ok(result);
    }
    if let Err(error) = wait_for_presence(&stable_id, true, Duration::from_secs(timeout_seconds)) {
        set_command_failure(
            &mut result,
            CommandFailure::new("hotplug_wait_return", error),
        );
        return Ok(result);
    }
    let reconnected = match Harness::new(&target, ControllerOptions::default()) {
        Ok(harness) => harness,
        Err(error) => {
            set_command_failure(
                &mut result,
                CommandFailure::new("hotplug_reconnected_create", error),
            );
            return Ok(result);
        }
    };
    result["resources"]["reconnected"]["created"] = json!(true);
    result["reconnected_identity"] = port_json(&reconnected.descriptor);
    Ok(finish_harness_phase(
        reconnected,
        result,
        "cleanup",
        "reconnected_harness_evidence",
        "hotplug_reconnected_close",
        |harness, result| {
            result["reconnect_operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "hotplug_reconnect_admit",
                "hotplug_reconnect_wait",
                "hotplug_reconnect_terminal",
            )?;
            result["reconnect_baud"] = json!(harness.actual_baud());
            Ok(())
        },
    ))
}

fn run_lifecycle(
    arguments: &[String],
    initial_target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
) -> Result<Value, String> {
    let cycles = value_or(arguments, "--cycles", 100_usize)?;
    if cycles == 0 {
        return Err("--cycles must be non-zero".to_owned());
    }
    let request = initial_target.request().clone();
    let mut next_target = Some(initial_target);
    let before = process_metrics()?;
    let mut result = json!({
        "command": "lifecycle",
        "cycles": cycles,
        "before": before,
        "records": [],
        "resources": {"created_cycles": 0},
    });
    for cycle in 1..=cycles {
        let target = next_target
            .take()
            .expect("a successful prior admission provides the next lifecycle target");
        let cycle_admission = admission_evidence_json(target.evidence(), "admitted");
        let harness = match Harness::new(&target, ControllerOptions::default()) {
            Ok(harness) => harness,
            Err(error) => {
                set_command_failure(&mut result, CommandFailure::new("lifecycle_create", error));
                result["failed_cycle"] = json!(cycle);
                return Ok(result);
            }
        };
        result["resources"]["created_cycles"] = json!(cycle);
        let record = json!({
            "cycle": cycle,
            "status": "running",
            "identity_admission": cycle_admission,
            "resources": {"primary": {"created": true}},
        });
        let mut record = finish_harness_phase(
            harness,
            record,
            "cleanup",
            "harness_evidence",
            "lifecycle_close",
            |harness, record| {
                record["connect_operation"] = connect_for_command(
                    harness,
                    ConnectOptions::default(),
                    "lifecycle_connect_admit",
                    "lifecycle_connect_wait",
                    "lifecycle_connect_terminal",
                )?;
                record["baud"] = json!(harness.actual_baud());
                Ok(())
            },
        );
        if record.get("execution_error").is_some() {
            record["status"] = json!("failed");
            result["execution_error"] = record["execution_error"].clone();
            result["failed_cycle"] = json!(cycle);
            result["records"]
                .as_array_mut()
                .expect("lifecycle records are an array")
                .push(record);
            return Ok(result);
        }
        let metrics = match process_metrics() {
            Ok(metrics) => metrics,
            Err(error) => {
                record["status"] = json!("failed");
                record["execution_error"] = json!({
                    "stage": "lifecycle_metrics",
                    "message": error,
                });
                result["execution_error"] = record["execution_error"].clone();
                result["failed_cycle"] = json!(cycle);
                result["records"]
                    .as_array_mut()
                    .expect("lifecycle records are an array")
                    .push(record);
                return Ok(result);
            }
        };
        record["status"] = json!("completed");
        record["process"] = metrics;
        match admit_device(discovery.as_ref(), request.clone()) {
            Ok(AdmissionDecision::Admitted(admitted)) => {
                record["port_present"] = json!(true);
                record["post_cycle_identity_admission"] =
                    admission_evidence_json(admitted.evidence(), "admitted");
                next_target = Some(admitted);
            }
            Ok(AdmissionDecision::Rejected(evidence)) => {
                record["port_present"] = json!(false);
                record["post_cycle_identity_admission"] =
                    admission_evidence_json(&evidence, "rejected");
                result["identity_admission_stop"] = json!({
                    "cycle": cycle,
                    "reason": evidence.reason().map(|reason| reason.as_str()),
                });
            }
            Ok(AdmissionDecision::Ambiguous(evidence)) => {
                record["status"] = json!("failed");
                record["port_present"] = json!(false);
                record["post_cycle_identity_admission"] =
                    admission_evidence_json(&evidence, "ambiguous");
                record["execution_error"] = json!({
                    "stage": "lifecycle_post_cycle_ambiguous",
                    "message": "system discovery returned an ambiguous serial snapshot",
                });
                result["execution_error"] = record["execution_error"].clone();
                result["failed_cycle"] = json!(cycle);
            }
            Err(error) => {
                record["status"] = json!("failed");
                record["port_present"] = json!(false);
                record["post_cycle_identity_admission"] = json!({
                    "status": "discovery_error",
                    "reason": "discovery_error",
                    "snapshot": [],
                    "observed_expected": null,
                    "observed_hint": null,
                    "structured_error": serial_error_json(&error),
                });
                record["execution_error"] = json!({
                    "stage": "lifecycle_post_cycle_discovery",
                    "message": error.message(),
                });
                result["execution_error"] = record["execution_error"].clone();
                result["failed_cycle"] = json!(cycle);
            }
        }
        result["records"]
            .as_array_mut()
            .expect("lifecycle records are an array")
            .push(record);
        if result.get("execution_error").is_some()
            || result.get("identity_admission_stop").is_some()
        {
            return Ok(result);
        }
    }
    let after = match process_metrics() {
        Ok(after) => after,
        Err(error) => {
            set_command_failure(
                &mut result,
                CommandFailure::new("lifecycle_final_metrics", error),
            );
            return Ok(result);
        }
    };
    let handle_delta = after["handles"].as_i64().unwrap_or_default()
        - result["before"]["handles"].as_i64().unwrap_or_default();
    let thread_delta = after["threads"].as_i64().unwrap_or_default()
        - result["before"]["threads"].as_i64().unwrap_or_default();
    result["after"] = after;
    result["handle_delta"] = json!(handle_delta);
    result["thread_delta"] = json!(thread_delta);
    result["no_positive_growth"] = json!(handle_delta <= 0 && thread_delta <= 0);
    Ok(result)
}

fn run_sequence(
    arguments: &[String],
    target: AdmittedDevice,
) -> Result<(Value, Option<Vec<u8>>), String> {
    let steps = value_or(arguments, "--steps", 10_000_usize)?;
    if !(1..=10_000).contains(&steps) {
        return Err("--steps must be in 1..=10000".to_owned());
    }
    let harness = Harness::new(&target, ControllerOptions::default())?;
    let result = json!({
        "command": "sequence",
        "requested_steps": steps,
        "timing_csv": SEQUENCE_TIMINGS_FILE_NAME,
        "physical_order_evidence": "open: no logic analyzer or firmware trace",
        "resources": {"primary": {"created": true}},
    });
    let mut timing_csv = None;
    let result = finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "sequence_close",
        |harness, result| {
            result["connect_operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "sequence_connect_admit",
                "sequence_connect_wait",
                "sequence_connect_terminal",
            )?;
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
            let sequence = PreciseSequence::new(sequence_steps)
                .map_err(|error| CommandFailure::new("sequence_build", error.to_string()))?;
            let started = harness.now_ns();
            let operation = harness
                .controller
                .precise_sequence(sequence)
                .map_err(|error| CommandFailure::new("sequence_admit", error.to_string()))?;
            let expected_seconds = u64::try_from(steps)
                .expect("step count fits u64")
                .saturating_mul(30)
                .div_ceil(1_000)
                .saturating_add(60);
            let (operation, state) = wait_for_command_terminal(
                operation,
                Duration::from_secs(expected_seconds),
                "sequence_terminal_wait",
            )?;
            let ended = harness.now_ns();
            result["operation"] = operation_json(&operation);
            result["functional_success"] = json!(state == OperationState::Succeeded);
            let reset = harness
                .controller
                .reset()
                .map_err(|error| CommandFailure::new("sequence_reset_admit", error.to_string()))?;
            result["reset_operation"] = wait_for_command_success(
                reset,
                OPERATION_TIMEOUT,
                "sequence_reset_wait",
                "sequence_reset_terminal",
            )?;
            timing_csv = Some(
                timing_csv_bytes(&harness.telemetry)
                    .map_err(|error| CommandFailure::new("sequence_timing_render", error))?,
            );
            let timing_count = harness
                .telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .timings
                .len();
            result["accepted_report_count_before_final_reset"] = json!(
                harness
                    .controller
                    .snapshot()
                    .accepted_report_count
                    .saturating_sub(1)
            );
            result["recorded_complete_writes"] = json!(timing_count);
            result["elapsed_ns"] = json!(ended.saturating_sub(started));
            result["actual_baud"] = json!(harness.actual_baud());
            result["latency"] = latency_json(&harness.telemetry, harness.actual_baud());
            Ok(())
        },
    );
    Ok((result, timing_csv))
}

fn amiibo_write_authorized(arguments: &[String]) -> bool {
    arguments
        .iter()
        .any(|argument| argument == "--authorize-write")
}

fn run_unauthorized_amiibo() -> Result<Value, String> {
    Ok(json!({
        "command": "amiibo",
        "capability": "unknown",
        "write_performed": false,
        "reason": "the v1 protocol has no safe capacity query; explicit limits and write authorization are required",
    }))
}

fn run_amiibo(arguments: &[String], target: AdmittedDevice) -> Result<Value, String> {
    if !arguments
        .iter()
        .any(|argument| argument == "--confirm-disposable")
    {
        return Err("Amiibo write also requires --confirm-disposable".to_owned());
    }
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
        &target,
        ControllerOptions {
            amiibo_limits: Some(limits),
            ..ControllerOptions::default()
        },
    )?;
    let result = json!({
        "command": "amiibo",
        "write_attempted": false,
        "write_performed": false,
        "slot": slot,
        "slot_count": slot_count,
        "maximum_data_len": maximum_data_len,
        "data_path": data_path,
        "actual_payload_len": data.len(),
        "resources": {"primary": {"created": true}},
    });
    Ok(finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "amiibo_close",
        move |harness, result| {
            result["connect_operation"] = connect_for_command(
                harness,
                ConnectOptions::default(),
                "amiibo_connect_admit",
                "amiibo_connect_wait",
                "amiibo_connect_terminal",
            )?;
            result["write_attempted"] = json!(true);
            let save = harness
                .controller
                .save_amiibo(slot, data, AmiiboSaveOptions::default())
                .map_err(|error| CommandFailure::new("amiibo_save_admit", error.to_string()))?;
            result["save"] = wait_for_command_success(
                save,
                Duration::from_secs(60),
                "amiibo_save_wait",
                "amiibo_save_terminal",
            )?;
            result["write_performed"] = json!(true);
            let select = harness
                .controller
                .select_amiibo(slot, AmiiboSelectOptions::default())
                .map_err(|error| CommandFailure::new("amiibo_select_admit", error.to_string()))?;
            result["select"] = wait_for_command_success(
                select,
                OPERATION_TIMEOUT,
                "amiibo_select_wait",
                "amiibo_select_terminal",
            )?;
            Ok(())
        },
    ))
}

fn exercise_action_for_command(
    harness: &Harness,
    label: &str,
    action: ControllerAction,
    output: &mut Value,
) -> Result<(), CommandFailure> {
    let operation = harness
        .controller
        .direct(action)
        .map_err(|error| CommandFailure::new("action_admit", format!("{label}: {error}")))?;
    let operation = wait_for_command_success(
        operation,
        OPERATION_TIMEOUT,
        "action_wait",
        "action_terminal",
    )?;
    output
        .as_array_mut()
        .expect("action output is an array")
        .push(json!({"label": label, "operation": operation}));
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

#[cfg(test)]
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

#[cfg(test)]
fn wait_for_cancel_ready(
    controller: &ControllerSession,
    operation: &Operation,
    baseline: ControllerSnapshot,
    timeout: Duration,
) -> Result<ControllerSnapshot, String> {
    if !baseline.desired_report.is_neutral() || baseline.lease != ControllerLeaseState::Available {
        return Err("cancel fault baseline was not neutral and lease-available".to_owned());
    }
    let expected_count = baseline
        .accepted_report_count
        .checked_add(1)
        .ok_or_else(|| "accepted report count exhausted".to_owned())?;
    let started = Instant::now();
    loop {
        let snapshot = controller.snapshot();
        match cancel_snapshot_readiness(&snapshot, expected_count, operation.id()) {
            CancelSnapshotReadiness::Ready => return Ok(snapshot),
            CancelSnapshotReadiness::Pending => {}
            CancelSnapshotReadiness::Invalid(message) => return Err(message.to_owned()),
        }
        if operation.snapshot().state.is_terminal() {
            return Err("cancel-fault sequence reached terminal before cancel request".to_owned());
        }
        if started.elapsed() >= timeout {
            return Err("timed out waiting for first cancel-fault report acceptance".to_owned());
        }
        thread::sleep(Duration::from_millis(1));
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CancelSnapshotReadiness {
    Pending,
    Ready,
    Invalid(&'static str),
}

fn cancel_snapshot_readiness(
    snapshot: &ControllerSnapshot,
    expected_count: u64,
    operation_id: easycon_model::OperationId,
) -> CancelSnapshotReadiness {
    match snapshot.accepted_report_count.cmp(&expected_count) {
        std::cmp::Ordering::Less => CancelSnapshotReadiness::Pending,
        std::cmp::Ordering::Equal
            if !snapshot.desired_report.is_neutral()
                && snapshot.lease == ControllerLeaseState::Sequence(operation_id) =>
        {
            CancelSnapshotReadiness::Ready
        }
        std::cmp::Ordering::Equal => CancelSnapshotReadiness::Pending,
        std::cmp::Ordering::Greater => CancelSnapshotReadiness::Invalid(
            "more than one report was accepted before cancel request",
        ),
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
        "error": snapshot.error.as_ref().map(ToString::to_string),
        "structured_error": snapshot.error.as_ref().map(|error| json!({
            "domain": format!("{:?}", error.domain()),
            "code": format!("{:?}", error.code()),
            "message": error.message(),
        })),
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
    controller_snapshot_json(controller.snapshot())
}

fn controller_snapshot_json(snapshot: ControllerSnapshot) -> Value {
    json!({
        "state": format!("{:?}", snapshot.state),
        "desired_report_neutral": snapshot.desired_report.is_neutral(),
        "accepted_report_count": snapshot.accepted_report_count,
        "last_report_timestamp_ns": snapshot.last_report_timestamp_ns,
        "lease": format!("{:?}", snapshot.lease),
        "lease_detail": lease_json(snapshot.lease),
    })
}

fn lease_json(lease: ControllerLeaseState) -> Value {
    match lease {
        ControllerLeaseState::Available => json!({"kind": "available"}),
        ControllerLeaseState::Sequence(operation_id) => json!({
            "kind": "sequence",
            "operation_id": operation_id.get(),
        }),
        ControllerLeaseState::Automation(lease_id) => json!({
            "kind": "automation",
            "lease_id": lease_id,
        }),
    }
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

fn native_open_attempts_json(telemetry: &Arc<Mutex<Telemetry>>) -> Value {
    Value::Array(
        telemetry
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .native_open_attempts
            .iter()
            .map(|attempt| {
                json!({
                    "baud": attempt.baud,
                    "succeeded": attempt.error.is_none(),
                    "error": attempt.error.as_ref().map(|error| json!({
                        "kind": format!("{:?}", error.kind()),
                        "os_code": error.os_code(),
                        "message": error.message(),
                    })),
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

fn harness_cleanup_json(controller: Value, runtime: Value) -> Value {
    let succeeded =
        controller_cleanup_succeeded(&controller) && runtime_cleanup_succeeded(&runtime);
    json!({
        "kind": "harness_cleanup",
        "succeeded": succeeded,
        "controller": controller,
        "runtime": runtime,
    })
}

fn controller_cleanup_json(
    controller_resource_id: ResourceId,
    pre_close: ControllerSnapshot,
    post_close: ControllerSnapshot,
    attempts: &[NeutralizationAttempt],
) -> Value {
    let final_attempts: Vec<_> = attempts
        .iter()
        .filter(|attempt| attempt.context.operation_id.is_none())
        .collect();
    let neutralization = if pre_close.state != ControllerState::Connected {
        if final_attempts.is_empty() {
            "not_required"
        } else {
            "evidence_incomplete"
        }
    } else if final_attempts.len() != 1 {
        "evidence_incomplete"
    } else {
        match &final_attempts[0].outcome {
            NeutralizationOutcome::Accepted
                if final_attempts[0].context.total_len == 8
                    && final_attempts[0].accepted_bytes == 8 =>
            {
                "accepted"
            }
            NeutralizationOutcome::Failed(_) => "not_delivered",
            NeutralizationOutcome::Pending
            | NeutralizationOutcome::Accepted
            | NeutralizationOutcome::Contradiction(_) => "evidence_incomplete",
        }
    };
    let mut value = json!({
        "kind": "controller_cleanup",
        "succeeded": false,
        "controller_resource_id": controller_resource_id.get(),
        "pre_close_state": format!("{:?}", pre_close.state),
        "post_close_state": format!("{:?}", post_close.state),
        "post_close_desired_report_neutral": post_close.desired_report.is_neutral(),
        "post_close_lease": format!("{:?}", post_close.lease),
        "neutralization": neutralization,
        "attempts": attempts.iter().map(neutralization_attempt_json).collect::<Vec<_>>(),
    });
    let succeeded = controller_cleanup_predicate(&value);
    value["succeeded"] = json!(succeeded);
    value
}

fn neutralization_attempt_json(attempt: &NeutralizationAttempt) -> Value {
    let (outcome, structured_error, diagnostic) = match &attempt.outcome {
        NeutralizationOutcome::Pending => ("pending", Value::Null, Value::Null),
        NeutralizationOutcome::Accepted => ("accepted", Value::Null, Value::Null),
        NeutralizationOutcome::Failed(error) => (
            "failed",
            json!({
                "kind": format!("{:?}", error.kind()),
                "message": error.message(),
            }),
            Value::Null,
        ),
        NeutralizationOutcome::Contradiction(message) => {
            ("contradiction", Value::Null, json!(message))
        }
    };
    json!({
        "resource_id": attempt.context.resource_id.get(),
        "operation_id": attempt.context.operation_id.map(|id| id.get()),
        "sequence": attempt.context.sequence,
        "dispatch_timestamp_ns": attempt.context.timestamp_ns,
        "first_write_entered_ns": attempt.first_write_entered_ns,
        "total_bytes": attempt.context.total_len,
        "accepted_bytes": attempt.accepted_bytes,
        "outcome": outcome,
        "structured_error": structured_error,
        "diagnostic": diagnostic,
    })
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

fn timing_csv_bytes(telemetry: &Arc<Mutex<Telemetry>>) -> Result<Vec<u8>, String> {
    let mut writer = Vec::new();
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
    Ok(writer)
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
         easycon-hardware-qualification handshake --port COMx --expected-identity ID\n  \
         easycon-hardware-qualification smoke --port COMx --expected-identity ID [--full] [--wake-left-stick] [--wake-home] [--hold-ms N]\n  \
         easycon-hardware-qualification home-wake --port COMx --expected-identity ID [--attempts 20] [--interval-seconds 3]\n  \
         easycon-hardware-qualification faults --port COMx --expected-identity ID\n  \
         easycon-hardware-qualification hotplug --port COMx --expected-identity ID [--timeout-seconds N]\n  \
         easycon-hardware-qualification lifecycle --port COMx --expected-identity ID [--cycles 100]\n  \
         easycon-hardware-qualification sequence --port COMx --expected-identity ID [--steps 10000]\n  \
         easycon-hardware-qualification amiibo [--port COMx --expected-identity ID --slot N --slot-count N \
         --maximum-data-len N --data FILE --authorize-write --confirm-disposable]\n\n  \
         Every command accepts --output-dir PATH."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use easycon_controller::TransportErrorKind;
    use easycon_model::{EasyConError, ErrorCode, ErrorDomain};
    use easycon_runtime::{CancellationReason, CancellationToken, ManagedResource};
    use easycon_serial::{ByteIoOperation, SerialErrorKind};

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
        let controller = controller_cleanup_json(
            ResourceId::new(1),
            ControllerSnapshot::default(),
            ControllerSnapshot {
                state: ControllerState::Closed,
                ..ControllerSnapshot::default()
            },
            &[],
        );
        let runtime = json!({
            "kind": "runtime_cleanup",
            "succeeded": true,
            "outcome": "Closed",
            "counts": {
                "active_operations": 0,
                "active_resources": 0,
                "active_tasks": 0,
            },
            "diagnostic": null,
        });
        harness_cleanup_json(controller, runtime)
    }

    fn fault_operation(id: u64, state: &str, domain: &str, code: &str, reason: Value) -> Value {
        json!({
            "id": id,
            "state": state,
            "structured_error": {
                "domain": domain,
                "code": code,
                "message": "diagnostic only",
            },
            "cancellation_reason": reason,
        })
    }

    fn port_busy_attempt(baud: u32) -> Value {
        json!({
            "baud": baud,
            "succeeded": false,
            "error": {
                "kind": "PortBusy",
                "os_code": 32,
                "message": "CreateFileW(serial port) failed",
            },
        })
    }

    fn valid_fault_projection() -> Value {
        json!({
            "port_occupied": fault_operation(41, "Failed", "Io", "Transport", Value::Null),
            "port_occupied_native_open_attempts": [
                port_busy_attempt(115_200),
                port_busy_attempt(9_600),
            ],
            "cancel": fault_operation(
                42,
                "Cancelled",
                "Runtime",
                "Cancelled",
                json!("Requested"),
            ),
            "cancel_request_outcome": "Applied",
            "cancel_baseline_snapshot": {
                "state": "Connected",
                "desired_report_neutral": true,
                "accepted_report_count": 7,
                "lease_detail": {"kind": "available"},
            },
            "cancel_before_request_snapshot": {
                "state": "Connected",
                "desired_report_neutral": false,
                "accepted_report_count": 8,
                "lease_detail": {"kind": "sequence", "operation_id": 42},
            },
            "post_cancel_snapshot": {
                "state": "Connected",
                "desired_report_neutral": true,
                "accepted_report_count": 9,
                "lease_detail": {"kind": "available"},
            },
            "deadline": fault_operation(
                43,
                "Cancelled",
                "Runtime",
                "DeadlineExceeded",
                json!("Deadline"),
            ),
            "deadline_native_open_attempts": [],
        })
    }

    fn valid_fault_projection_with_cleanup() -> Value {
        let mut result = valid_fault_projection();
        result["scenarios"] = json!({
            "port_occupied": {"status": "completed"},
            "cancel": {"status": "completed"},
            "deadline": {"status": "completed"},
        });
        result["resources"] = json!({
            "occupier": {"created": true},
            "occupied_probe": {"created": true},
            "cancel": {"created": true},
            "deadline": {"created": true},
        });
        result["occupier_cleanup"] = successful_cleanup();
        result["occupied_probe_cleanup"] = successful_cleanup();
        result["cancel_cleanup"] = successful_cleanup();
        result["deadline_cleanup"] = successful_cleanup();
        result
    }

    #[test]
    fn complete_fault_cleanup_uses_four_explicit_roles_without_legacy_aliases() {
        let result = valid_fault_projection_with_cleanup();

        assert!(cleanup_contract_succeeded("faults", &result));
        assert!(result.get("cleanup").is_none());
        assert!(result.get("secondary_cleanup").is_none());

        let mut missing_resources = result.clone();
        missing_resources
            .as_object_mut()
            .expect("fault result")
            .remove("resources");
        assert!(!cleanup_contract_succeeded("faults", &missing_resources));

        let mut incomplete_scenario = result.clone();
        incomplete_scenario["scenarios"]["cancel"]["status"] = json!("running");
        assert!(!cleanup_contract_succeeded("faults", &incomplete_scenario));

        let mut uncreated_role = result;
        uncreated_role["resources"]["deadline"]["created"] = json!(false);
        assert!(!cleanup_contract_succeeded("faults", &uncreated_role));
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

    struct HandshakeFailureTransport;

    impl ControllerTransport for HandshakeFailureTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "injected handshake failure",
            ))
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in handshake failure test",
            ))
        }

        fn close(&mut self) {}
    }

    struct FailSecondReportTransport {
        accepted_reports: usize,
    }

    impl ControllerTransport for FailSecondReportTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            if request.context.kind == WriteKind::Report {
                self.accepted_reports += 1;
                if self.accepted_reports == 2 {
                    return Err(TransportError::new(
                        TransportErrorKind::Io,
                        "injected second report failure",
                    ));
                }
            }
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in action failure test",
            ))
        }

        fn close(&mut self) {}
    }

    struct FinalNeutralFailureTransport;

    impl ControllerTransport for FinalNeutralFailureTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            if request.context.kind == WriteKind::Neutralize
                && request.context.operation_id.is_none()
            {
                Err(TransportError::new(
                    TransportErrorKind::Io,
                    "injected final neutral write failure",
                ))
            } else {
                Ok(request.bytes.len())
            }
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in final neutralization test",
            ))
        }

        fn close(&mut self) {}
    }

    struct PartialFinalNeutralFailureTransport {
        accepted_prefix: bool,
    }

    impl ControllerTransport for PartialFinalNeutralFailureTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            if request.context.kind != WriteKind::Neutralize
                || request.context.operation_id.is_some()
            {
                return Ok(request.bytes.len());
            }
            if !self.accepted_prefix {
                self.accepted_prefix = true;
                Ok(3)
            } else {
                Err(TransportError::new(
                    TransportErrorKind::Io,
                    "injected failure after final neutral prefix",
                ))
            }
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in partial final neutralization test",
            ))
        }

        fn close(&mut self) {}
    }

    fn observed_harness(transport: Box<dyn ControllerTransport>, label: &str) -> Harness {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let runtime = Runtime::new(clock.clone());
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let observed = ObservedTransport {
            inner: transport,
            clock: clock.clone(),
            telemetry: telemetry.clone(),
        };
        let controller =
            ControllerSession::new(&runtime, Box::new(observed), ControllerOptions::default())
                .expect("controller");
        Harness {
            runtime,
            clock,
            controller,
            telemetry,
            descriptor: SerialPortDescriptor::new(format!("TEST\\{label}"), "COM1")
                .expect("descriptor"),
        }
    }

    struct PanickingResource;

    impl ManagedResource for PanickingResource {
        fn close(&self) {
            panic!("injected qualification close failure");
        }
    }

    struct ScriptedOpenFactory {
        errors: VecDeque<SerialError>,
    }

    struct ScriptedDeviceDiscovery {
        result: Result<Vec<SerialPortDescriptor>, SerialError>,
        calls: Arc<AtomicUsize>,
    }

    impl DeviceDiscovery for ScriptedDeviceDiscovery {
        fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    impl ByteIoFactory for ScriptedOpenFactory {
        fn open(
            &mut self,
            _port: &SerialPortDescriptor,
            _baud_rate: u32,
            _request: ByteIoRequest,
        ) -> Result<Box<dyn ByteIo>, SerialError> {
            Err(self.errors.pop_front().expect("scripted open error"))
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
    fn rejected_identity_is_not_run_and_never_constructs_a_harness() {
        let calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::new(AtomicUsize::new(0));
        let discovery = Arc::new(ScriptedDeviceDiscovery {
            result: Ok(vec![
                SerialPortDescriptor::new("DEVICE\\OTHER", "COM8").expect("other descriptor"),
                SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM11")
                    .expect("expected descriptor"),
            ]),
            calls: Arc::clone(&calls),
        });
        let arguments = vec![
            "handshake".to_owned(),
            "--port".to_owned(),
            "COM8".to_owned(),
            "--expected-identity".to_owned(),
            "DEVICE\\EXPECTED".to_owned(),
        ];
        let observed_runner_calls = Arc::clone(&runner_calls);
        let result =
            run_with_device_admission_using("handshake", &arguments, discovery, move |_, _| {
                observed_runner_calls.fetch_add(1, Ordering::SeqCst);
                Ok(json!({"unexpected": true}))
            })
            .expect("rejected admission result");

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(runner_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            result["identity_admission"]["reason"],
            "expected_at_different_port"
        );
        assert!(cleanup_contract_succeeded("handshake", &result));
        let (document, _) = finalize_result("handshake", Ok(result));
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "not_run");
        assert_eq!(document_exit_code(&document), 2);
    }

    #[test]
    fn unsafe_discovery_results_fail_before_the_runner() {
        let arguments = vec![
            "handshake".to_owned(),
            "--port".to_owned(),
            "COM8".to_owned(),
            "--expected-identity".to_owned(),
            "DEVICE\\EXPECTED".to_owned(),
        ];
        let cases = [
            Ok(vec![
                SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM8").expect("descriptor"),
                SerialPortDescriptor::new("DEVICE\\OTHER", "com8").expect("duplicate port"),
            ]),
            Err(SerialError::with_os_code(
                easycon_serial::SerialErrorKind::Io,
                "injected discovery failure",
                5,
            )),
        ];

        for discovery_result in cases {
            let calls = Arc::new(AtomicUsize::new(0));
            let runner_calls = Arc::new(AtomicUsize::new(0));
            let observed_runner_calls = Arc::clone(&runner_calls);
            let result = run_with_device_admission_using(
                "handshake",
                &arguments,
                Arc::new(ScriptedDeviceDiscovery {
                    result: discovery_result,
                    calls: Arc::clone(&calls),
                }),
                move |_, _| {
                    observed_runner_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"unexpected": true}))
                },
            )
            .expect("fail-closed admission result");

            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert_eq!(runner_calls.load(Ordering::SeqCst), 0);
            assert!(result.get("execution_error").is_some());
            assert!(cleanup_contract_succeeded("handshake", &result));
            let (document, _) = finalize_result("handshake", Ok(result));
            assert_eq!(document["execution_status"], "failed");
            assert_eq!(document["qualification_status"], "failed");
            assert_eq!(document_exit_code(&document), 1);
        }
    }

    #[test]
    fn missing_identity_is_rejected_without_discovery() {
        let calls = Arc::new(AtomicUsize::new(0));
        for arguments in [
            vec![
                "handshake".to_owned(),
                "--port".to_owned(),
                "COM8".to_owned(),
            ],
            vec![
                "handshake".to_owned(),
                "--expected-identity".to_owned(),
                "--port".to_owned(),
                "COM8".to_owned(),
            ],
        ] {
            let result = run_with_device_admission_using(
                "handshake",
                &arguments,
                Arc::new(ScriptedDeviceDiscovery {
                    result: Ok(Vec::new()),
                    calls: Arc::clone(&calls),
                }),
                |_, _| Ok(json!({"unexpected": true})),
            );
            assert!(result.is_err());
        }
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn failed_connect_still_records_operation_and_explicit_cleanup() {
        let harness = observed_harness(Box::new(HandshakeFailureTransport), "CONNECT-FAILURE");
        let result = json!({
            "command": "handshake",
            "resources": {"primary": {"created": true}},
        });
        let result = finish_harness_phase(
            harness,
            result,
            "cleanup",
            "harness_evidence",
            "handshake_close",
            |harness, result| {
                result["operation"] = connect_for_command(
                    harness,
                    ConnectOptions::default(),
                    "handshake_connect_admit",
                    "handshake_connect_wait",
                    "handshake_connect_terminal",
                )?;
                Ok(())
            },
        );

        assert_eq!(
            result["execution_error"]["stage"],
            "handshake_connect_terminal"
        );
        assert_eq!(result["failed_operation_at_failure"]["state"], "Failed");
        assert_eq!(result["failed_operation_post_close"]["state"], "Failed");
        assert!(cleanup_succeeded(&result["cleanup"]));
        assert!(cleanup_contract_succeeded("handshake", &result));
        assert_eq!(
            result["harness_evidence"]["pre_cleanup_snapshot"]["state"],
            "Disconnected"
        );
        let (document, _) = finalize_result("handshake", Ok(result));
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn action_failure_preserves_the_completed_prefix_and_cleanup() {
        let harness = observed_harness(
            Box::new(FailSecondReportTransport {
                accepted_reports: 0,
            }),
            "ACTION-FAILURE",
        );
        let result = json!({
            "command": "smoke",
            "actions": [],
            "resources": {"primary": {"created": true}},
        });
        let result = finish_harness_phase(
            harness,
            result,
            "cleanup",
            "harness_evidence",
            "smoke_close",
            |harness, result| {
                result["connect_operation"] = connect_for_command(
                    harness,
                    ConnectOptions::default(),
                    "smoke_connect_admit",
                    "smoke_connect_wait",
                    "smoke_connect_terminal",
                )?;
                exercise_action_for_command(
                    harness,
                    "button.A.down",
                    ControllerAction::ButtonDown(Button::A),
                    &mut result["actions"],
                )?;
                exercise_action_for_command(
                    harness,
                    "button.A.up",
                    ControllerAction::ButtonUp(Button::A),
                    &mut result["actions"],
                )?;
                Ok(())
            },
        );

        assert_eq!(result["execution_error"]["stage"], "action_terminal");
        assert_eq!(result["actions"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["actions"][0]["label"], "button.A.down");
        assert!(cleanup_succeeded(&result["cleanup"]));
        assert!(cleanup_contract_succeeded("smoke", &result));
        assert_eq!(result["failed_operation_post_close"]["state"], "Failed");
    }

    #[test]
    fn cleanup_failure_is_appended_without_overwriting_the_first_error() {
        let harness = observed_harness(Box::new(FinalNeutralFailureTransport), "COMBINED-FAILURE");
        let result = json!({
            "command": "smoke",
            "resources": {"primary": {"created": true}},
        });
        let result = finish_harness_phase(
            harness,
            result,
            "cleanup",
            "harness_evidence",
            "smoke_close",
            |harness, result| {
                result["connect_operation"] = connect_for_command(
                    harness,
                    ConnectOptions::default(),
                    "smoke_connect_admit",
                    "smoke_connect_wait",
                    "smoke_connect_terminal",
                )?;
                Err(CommandFailure::new(
                    "operator_marker",
                    "injected execution failure",
                ))
            },
        );

        assert_eq!(result["execution_error"]["stage"], "operator_marker");
        assert_eq!(result["cleanup_errors"][0]["stage"], "smoke_close");
        assert!(!cleanup_succeeded(&result["cleanup"]));
        assert!(!cleanup_contract_succeeded("smoke", &result));
        let (document, _) = finalize_result("smoke", Ok(result));
        assert_eq!(
            document["error"],
            "operator_marker: injected execution failure"
        );
        assert_eq!(document["checks"][1]["status"], "incomplete_or_failed");
    }

    #[test]
    fn ordinary_partial_cleanup_contracts_require_exact_created_roles() {
        let failure = json!({"stage": "injected", "message": "injected failure"});
        let single = json!({
            "execution_error": failure,
            "resources": {"primary": {"created": true}},
            "cleanup": successful_cleanup(),
        });
        assert!(cleanup_contract_succeeded("handshake", &single));
        let mut single_without_cleanup = single.clone();
        single_without_cleanup
            .as_object_mut()
            .expect("single result")
            .remove("cleanup");
        assert!(!cleanup_contract_succeeded(
            "handshake",
            &single_without_cleanup
        ));

        let hotplug = json!({
            "execution_error": {"stage": "hotplug_wait_return", "message": "injected failure"},
            "resources": {
                "initial": {"created": true},
                "reconnected": {"created": false},
            },
            "disconnect_cleanup": successful_cleanup(),
        });
        assert!(cleanup_contract_succeeded("hotplug", &hotplug));
        let mut forged_reconnected = hotplug.clone();
        forged_reconnected["resources"]["reconnected"]["created"] = json!(true);
        assert!(!cleanup_contract_succeeded("hotplug", &forged_reconnected));

        let lifecycle = json!({
            "execution_error": {"stage": "lifecycle_metrics", "message": "injected failure"},
            "cycles": 2,
            "failed_cycle": 2,
            "resources": {"created_cycles": 2},
            "records": [
                {
                    "cycle": 1,
                    "status": "completed",
                    "resources": {"primary": {"created": true}},
                    "cleanup": successful_cleanup()
                },
                {
                    "cycle": 2,
                    "status": "failed",
                    "resources": {"primary": {"created": true}},
                    "cleanup": successful_cleanup()
                },
            ],
        });
        assert!(cleanup_contract_succeeded("lifecycle", &lifecycle));
        let mut wrong_cycle_count = lifecycle;
        wrong_cycle_count["resources"]["created_cycles"] = json!(1);
        assert!(!cleanup_contract_succeeded("lifecycle", &wrong_cycle_count));
    }

    #[test]
    fn partial_execution_failure_preserves_stage_error_and_cleanup_evidence() {
        let mut result = valid_fault_projection();
        result
            .as_object_mut()
            .expect("fault result")
            .remove("deadline");
        result
            .as_object_mut()
            .expect("fault result")
            .remove("deadline_native_open_attempts");
        result["execution_error"] = json!({
            "stage": "cancel_readiness",
            "message": "timed out waiting for first report",
        });
        result["scenarios"] = json!({
            "port_occupied": {"status": "completed"},
            "cancel": {"status": "failed"},
            "deadline": {"status": "not_run"},
        });
        result["resources"] = json!({
            "occupier": {"created": true},
            "occupied_probe": {"created": true},
            "cancel": {"created": true},
            "deadline": {"created": false},
        });
        result["occupier_cleanup"] = successful_cleanup();
        result["occupied_probe_cleanup"] = successful_cleanup();
        result["cancel_cleanup"] = successful_cleanup();

        let (document, failure) = finalize_result("faults", Ok(result));
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(
            document["error"],
            "cancel_readiness: timed out waiting for first report"
        );
        assert_eq!(
            document["result"]["scenarios"]["deadline"]["status"],
            "not_run"
        );
        assert_eq!(
            document["result"]["cancel_cleanup"]["runtime"]["outcome"],
            "Closed"
        );
        assert_eq!(
            failure.as_deref(),
            Some("cancel_readiness: timed out waiting for first report")
        );
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn malformed_execution_error_evidence_fails_closed() {
        let malformed = [
            Value::Null,
            json!({}),
            json!({"stage": "", "message": "failure"}),
            json!({"stage": "cancel", "message": ""}),
            json!({"stage": 7, "message": "failure"}),
            json!("cancel failed"),
        ];

        for raw_error in malformed {
            let mut result = valid_fault_projection_with_cleanup();
            result["execution_error"] = raw_error.clone();

            let (document, failure) = finalize_result("faults", Ok(result));

            assert_eq!(document["execution_status"], "failed");
            assert_eq!(document["qualification_status"], "failed");
            assert_eq!(document["error"], "malformed execution_error evidence");
            assert_eq!(document["result"]["execution_error"], raw_error);
            assert_eq!(
                failure.as_deref(),
                Some("malformed execution_error evidence")
            );
            assert_eq!(document_exit_code(&document), 1);
        }
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
                    "occupier_cleanup": successful_cleanup(),
                    "occupied_probe_cleanup": successful_cleanup(),
                    "cancel_cleanup": successful_cleanup(),
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
    fn operation_json_preserves_structured_error_and_cancellation_reason() {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let runtime = Runtime::new(clock);

        let failed = runtime.create_operation(None).expect("failed operation");
        failed.start();
        failed.fail(EasyConError::new(
            ErrorDomain::Io,
            ErrorCode::Transport,
            "native serial open failed",
        ));
        let failed_json = operation_json(&failed);
        assert_eq!(
            failed_json["error"],
            "Io/Transport: native serial open failed"
        );
        assert_eq!(failed_json["structured_error"]["domain"], "Io");
        assert_eq!(failed_json["structured_error"]["code"], "Transport");
        assert_eq!(
            failed_json["structured_error"]["message"],
            "native serial open failed"
        );
        assert_eq!(failed_json["cancellation_reason"], Value::Null);

        let requested = runtime
            .create_operation(None)
            .expect("requested cancellation operation");
        requested.start();
        requested.cancel();
        requested.finish_cancelled();
        let requested_json = operation_json(&requested);
        assert_eq!(requested_json["structured_error"]["domain"], "Runtime");
        assert_eq!(requested_json["structured_error"]["code"], "Cancelled");
        assert_eq!(requested_json["cancellation_reason"], "Requested");

        let deadline = runtime
            .create_operation(None)
            .expect("deadline cancellation operation");
        deadline.start();
        deadline.request_cancel(CancellationReason::Deadline);
        deadline.finish_cancelled();
        let deadline_json = operation_json(&deadline);
        assert_eq!(deadline_json["structured_error"]["domain"], "Runtime");
        assert_eq!(
            deadline_json["structured_error"]["code"],
            "DeadlineExceeded"
        );
        assert_eq!(deadline_json["cancellation_reason"], "Deadline");

        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
    }

    #[test]
    fn observed_factory_preserves_native_open_failures() {
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let inner = ScriptedOpenFactory {
            errors: VecDeque::from([
                SerialError::with_os_code(SerialErrorKind::PortBusy, "sharing violation", 32),
                SerialError::with_os_code(SerialErrorKind::AccessDenied, "access denied", 5),
            ]),
        };
        let mut factory = ObservedByteIoFactory {
            inner: Box::new(inner),
            telemetry: telemetry.clone(),
        };
        let descriptor = SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM8").expect("descriptor");
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let request = || ByteIoRequest {
            operation: ByteIoOperation::Open,
            clock: clock.clone(),
            deadline_ns: u64::MAX,
            cancellation: CancellationToken::root(),
            resource_cancellation: CancellationToken::root(),
        };

        assert!(factory.open(&descriptor, 115_200, request()).is_err());
        assert!(factory.open(&descriptor, 9_600, request()).is_err());

        let attempts = native_open_attempts_json(&telemetry);
        assert_eq!(attempts[0]["baud"], 115_200);
        assert_eq!(attempts[0]["succeeded"], false);
        assert_eq!(attempts[0]["error"]["kind"], "PortBusy");
        assert_eq!(attempts[0]["error"]["os_code"], 32);
        assert_eq!(attempts[1]["baud"], 9_600);
        assert_eq!(attempts[1]["error"]["kind"], "AccessDenied");
        assert_eq!(attempts[1]["error"]["os_code"], 5);
    }

    #[test]
    fn faults_require_exact_operation_causes() {
        let valid = valid_fault_projection();
        assert_eq!(
            qualification_decision("faults", &valid).status,
            QualificationStatus::Passed
        );

        let mut invalid = Vec::new();
        let mut wrong_occupied_code = valid.clone();
        wrong_occupied_code["port_occupied"]["structured_error"]["code"] = json!("ProtocolError");
        invalid.push(wrong_occupied_code);

        let mut missing_occupied_reason = valid.clone();
        missing_occupied_reason["port_occupied"]
            .as_object_mut()
            .expect("occupied operation")
            .remove("cancellation_reason");
        invalid.push(missing_occupied_reason);

        let mut wrong_cancel_reason = valid.clone();
        wrong_cancel_reason["cancel"]["cancellation_reason"] = json!("Deadline");
        invalid.push(wrong_cancel_reason);

        let mut wrong_deadline_code = valid.clone();
        wrong_deadline_code["deadline"]["structured_error"]["code"] = json!("Cancelled");
        invalid.push(wrong_deadline_code);

        let mut missing_deadline_error = valid;
        missing_deadline_error["deadline"]
            .as_object_mut()
            .expect("deadline operation")
            .remove("structured_error");
        invalid.push(missing_deadline_error);

        for result in invalid {
            let decision = qualification_decision("faults", &result);
            assert_eq!(decision.status, QualificationStatus::Failed);
            assert!(decision.failure.is_some());
        }
    }

    #[test]
    fn faults_require_exact_native_open_evidence() {
        let valid = valid_fault_projection();
        assert_eq!(
            qualification_decision("faults", &valid).status,
            QualificationStatus::Passed
        );

        let mut invalid = Vec::new();
        let mut missing_occupied_attempts = valid.clone();
        missing_occupied_attempts
            .as_object_mut()
            .expect("fault result")
            .remove("port_occupied_native_open_attempts");
        invalid.push(missing_occupied_attempts);

        let mut access_denied = valid.clone();
        access_denied["port_occupied_native_open_attempts"][0]["error"]["kind"] =
            json!("AccessDenied");
        access_denied["port_occupied_native_open_attempts"][0]["error"]["os_code"] = json!(5);
        invalid.push(access_denied);

        let mut wrong_baud_order = valid.clone();
        wrong_baud_order["port_occupied_native_open_attempts"][0]["baud"] = json!(9_600);
        invalid.push(wrong_baud_order);

        let mut deadline_opened = valid;
        deadline_opened["deadline_native_open_attempts"] = json!([port_busy_attempt(115_200)]);
        invalid.push(deadline_opened);

        for result in invalid {
            let decision = qualification_decision("faults", &result);
            assert_eq!(decision.status, QualificationStatus::Failed);
            assert!(decision.failure.is_some());
        }
    }

    #[test]
    fn faults_require_nonvacuous_cancel_evidence() {
        let valid = valid_fault_projection();
        assert_eq!(
            qualification_decision("faults", &valid).status,
            QualificationStatus::Passed
        );

        let mut invalid = Vec::new();
        let mut missing_baseline = valid.clone();
        missing_baseline
            .as_object_mut()
            .expect("fault result")
            .remove("cancel_baseline_snapshot");
        invalid.push(missing_baseline);

        let mut no_report_before_cancel = valid.clone();
        no_report_before_cancel["cancel_before_request_snapshot"]["accepted_report_count"] =
            json!(7);
        invalid.push(no_report_before_cancel);

        let mut neutral_before_cancel = valid.clone();
        neutral_before_cancel["cancel_before_request_snapshot"]["desired_report_neutral"] =
            json!(true);
        invalid.push(neutral_before_cancel);

        let mut wrong_sequence_owner = valid.clone();
        wrong_sequence_owner["cancel_before_request_snapshot"]["lease_detail"]["operation_id"] =
            json!(99);
        invalid.push(wrong_sequence_owner);

        let mut missing_request_outcome = valid.clone();
        missing_request_outcome
            .as_object_mut()
            .expect("fault result")
            .remove("cancel_request_outcome");
        invalid.push(missing_request_outcome);

        let mut repeated_request_outcome = valid.clone();
        repeated_request_outcome["cancel_request_outcome"] = json!("Unchanged");
        invalid.push(repeated_request_outcome);

        let mut no_neutral_acceptance = valid.clone();
        no_neutral_acceptance["post_cancel_snapshot"]["accepted_report_count"] = json!(8);
        invalid.push(no_neutral_acceptance);

        let mut unexpected_extra_acceptance = valid.clone();
        unexpected_extra_acceptance["post_cancel_snapshot"]["accepted_report_count"] = json!(10);
        invalid.push(unexpected_extra_acceptance);

        let mut nonneutral_terminal = valid;
        nonneutral_terminal["post_cancel_snapshot"]["desired_report_neutral"] = json!(false);
        invalid.push(nonneutral_terminal);

        for result in invalid {
            let decision = qualification_decision("faults", &result);
            assert_eq!(decision.status, QualificationStatus::Failed);
            assert!(decision.failure.is_some());
        }
    }

    #[test]
    fn cancel_readiness_tolerates_the_split_snapshot_publication() {
        let operation_id = easycon_model::OperationId::new(42);
        let baseline = ControllerSnapshot {
            state: easycon_controller::ControllerState::Connected,
            desired_report: easycon_controller::SwitchReport::NEUTRAL,
            accepted_report_count: 7,
            last_report_timestamp_ns: None,
            lease: ControllerLeaseState::Available,
        };
        assert_eq!(
            cancel_snapshot_readiness(&baseline, 8, operation_id),
            CancelSnapshotReadiness::Pending
        );

        let intermediate = ControllerSnapshot {
            accepted_report_count: 8,
            lease: ControllerLeaseState::Sequence(operation_id),
            ..baseline
        };
        assert_eq!(
            cancel_snapshot_readiness(&intermediate, 8, operation_id),
            CancelSnapshotReadiness::Pending
        );

        let mut nonneutral = easycon_controller::SwitchReport::NEUTRAL;
        nonneutral.press(Button::A);
        let ready = ControllerSnapshot {
            desired_report: nonneutral,
            ..intermediate
        };
        assert_eq!(
            cancel_snapshot_readiness(&ready, 8, operation_id),
            CancelSnapshotReadiness::Ready
        );
    }

    #[test]
    fn cancel_fault_waits_for_a_nonneutral_acceptance_before_request() {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let runtime = Runtime::new(clock.clone());
        let controller = ControllerSession::new(
            &runtime,
            Box::new(NoopTransport),
            ControllerOptions::default(),
        )
        .expect("controller");
        let connect = controller
            .connect(ConnectOptions::default())
            .expect("connect operation");
        wait_succeeded(&connect, OPERATION_TIMEOUT).expect("connected");

        let baseline = controller.snapshot();
        let sequence = PreciseSequence::new(vec![
            SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
            SequenceStep::new(60_000_000_000, ControllerAction::ButtonUp(Button::A)),
        ])
        .expect("sequence");
        let operation = controller
            .precise_sequence(sequence)
            .expect("sequence operation");
        let before_request =
            wait_for_cancel_ready(&controller, &operation, baseline, OPERATION_TIMEOUT)
                .expect("first non-neutral report acceptance");
        assert_eq!(
            before_request.accepted_report_count,
            baseline.accepted_report_count + 1
        );
        assert!(!before_request.desired_report.is_neutral());
        assert_eq!(
            before_request.lease,
            ControllerLeaseState::Sequence(operation.id())
        );

        operation.cancel();
        wait_terminal(&operation, OPERATION_TIMEOUT).expect("cancel terminal");
        let after_cancel = controller.snapshot();
        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert!(after_cancel.desired_report.is_neutral());
        assert_eq!(after_cancel.lease, ControllerLeaseState::Available);
        assert!(after_cancel.accepted_report_count >= baseline.accepted_report_count + 2);

        controller.close();
        assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
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
    fn partial_fault_cleanup_layout_tracks_created_roles_and_terminal_scenarios() {
        let partial = json!({
            "command": "faults",
            "execution_error": {
                "stage": "occupied_probe_create",
                "message": "injected create failure",
            },
            "scenarios": {
                "port_occupied": {"status": "failed"},
                "cancel": {"status": "not_run"},
                "deadline": {"status": "not_run"},
            },
            "resources": {
                "occupier": {"created": true},
                "occupied_probe": {"created": false},
                "cancel": {"created": false},
                "deadline": {"created": false},
            },
            "occupier_cleanup": successful_cleanup(),
        });

        assert!(cleanup_contract_succeeded("faults", &partial));
        let (document, failure) = finalize_result("faults", Ok(partial.clone()));
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
        assert_eq!(
            document["checks"][1],
            qualification_check("cleanup_evidence", "passed")
        );
        assert_eq!(
            failure.as_deref(),
            Some("occupied_probe_create: injected create failure")
        );

        let mut invalid = Vec::new();

        let mut missing_created_cleanup = partial.clone();
        missing_created_cleanup
            .as_object_mut()
            .expect("partial fault result")
            .remove("occupier_cleanup");
        invalid.push(missing_created_cleanup);

        let mut cleanup_for_uncreated_role = partial.clone();
        cleanup_for_uncreated_role["occupied_probe_cleanup"] = successful_cleanup();
        invalid.push(cleanup_for_uncreated_role);

        let mut out_of_order_creation = partial.clone();
        out_of_order_creation["resources"]["cancel"]["created"] = json!(true);
        out_of_order_creation["cancel_cleanup"] = successful_cleanup();
        invalid.push(out_of_order_creation);

        let mut nonterminal_scenario = partial.clone();
        nonterminal_scenario["scenarios"]["port_occupied"]["status"] = json!("running");
        invalid.push(nonterminal_scenario);

        let mut completed_without_probe = partial.clone();
        completed_without_probe["scenarios"]["port_occupied"]["status"] = json!("completed");
        completed_without_probe["scenarios"]["cancel"]["status"] = json!("failed");
        invalid.push(completed_without_probe);

        let mut later_roles_after_early_failure = partial.clone();
        later_roles_after_early_failure["resources"]["occupied_probe"]["created"] = json!(true);
        later_roles_after_early_failure["resources"]["cancel"]["created"] = json!(true);
        later_roles_after_early_failure["occupied_probe_cleanup"] = successful_cleanup();
        later_roles_after_early_failure["cancel_cleanup"] = successful_cleanup();
        invalid.push(later_roles_after_early_failure);

        let mut stage_without_resource = partial.clone();
        stage_without_resource["execution_error"]["stage"] = json!("deadline_terminal_wait");
        invalid.push(stage_without_resource);

        let mut unknown_stage = partial.clone();
        unknown_stage["execution_error"]["stage"] = json!("future_fault_stage");
        invalid.push(unknown_stage);

        for close_stage in ["occupied_probe_close", "occupier_close"] {
            let mut contradictory_close = partial.clone();
            contradictory_close["execution_error"]["stage"] = json!(close_stage);
            contradictory_close["resources"]["occupied_probe"]["created"] = json!(true);
            contradictory_close["occupied_probe_cleanup"] = successful_cleanup();
            invalid.push(contradictory_close);
        }

        let mut contradictory_cancel_close = partial.clone();
        contradictory_cancel_close["execution_error"]["stage"] = json!("cancel_close");
        contradictory_cancel_close["scenarios"]["port_occupied"]["status"] = json!("completed");
        contradictory_cancel_close["scenarios"]["cancel"]["status"] = json!("failed");
        contradictory_cancel_close["resources"]["occupied_probe"]["created"] = json!(true);
        contradictory_cancel_close["resources"]["cancel"]["created"] = json!(true);
        contradictory_cancel_close["occupied_probe_cleanup"] = successful_cleanup();
        contradictory_cancel_close["cancel_cleanup"] = successful_cleanup();
        invalid.push(contradictory_cancel_close);

        let mut contradictory_deadline_close = partial.clone();
        contradictory_deadline_close["execution_error"]["stage"] = json!("deadline_close");
        contradictory_deadline_close["scenarios"]["port_occupied"]["status"] = json!("completed");
        contradictory_deadline_close["scenarios"]["cancel"]["status"] = json!("completed");
        contradictory_deadline_close["scenarios"]["deadline"]["status"] = json!("failed");
        contradictory_deadline_close["resources"]["occupied_probe"]["created"] = json!(true);
        contradictory_deadline_close["resources"]["cancel"]["created"] = json!(true);
        contradictory_deadline_close["resources"]["deadline"]["created"] = json!(true);
        contradictory_deadline_close["occupied_probe_cleanup"] = successful_cleanup();
        contradictory_deadline_close["cancel_cleanup"] = successful_cleanup();
        contradictory_deadline_close["deadline_cleanup"] = successful_cleanup();
        invalid.push(contradictory_deadline_close);

        let mut missing_scenario = partial.clone();
        missing_scenario["scenarios"]
            .as_object_mut()
            .expect("fault scenarios")
            .remove("deadline");
        invalid.push(missing_scenario);

        let mut misplaced_cleanup = partial.clone();
        misplaced_cleanup["nested"]["occupier_cleanup"] = successful_cleanup();
        invalid.push(misplaced_cleanup);

        for result in invalid {
            assert!(!cleanup_contract_succeeded("faults", &result));
            let (document, _) = finalize_result("faults", Ok(result));
            assert_eq!(
                document["checks"][1],
                qualification_check("cleanup_evidence", "incomplete_or_failed")
            );
            assert_eq!(document_exit_code(&document), 1);
        }

        let mut no_execution_error = partial;
        no_execution_error
            .as_object_mut()
            .expect("partial fault result")
            .remove("execution_error");
        assert!(!cleanup_contract_succeeded("faults", &no_execution_error));
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
            ("faults", valid_fault_projection_with_cleanup()),
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
            } else if command == "faults" {
                missing
                    .as_object_mut()
                    .expect("fault result")
                    .remove("occupier_cleanup");
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
        failed_outcome["runtime"]["outcome"] = json!("Failed");
        assert_cleanup_failure(
            "handshake",
            json!({
                "operation": {"state": "Succeeded"},
                "actual_baud": 115_200,
                "cleanup": failed_outcome,
            }),
        );
        let mut nonzero_counts = cleanup();
        nonzero_counts["runtime"]["counts"]["active_operations"] = json!(1);
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
        assert_eq!(cleanup["runtime"]["outcome"], "Failed");
        assert_eq!(cleanup["runtime"]["report"]["phase"], "ResourceCleanup");
        assert_eq!(
            cleanup["runtime"]["report"]["diagnostic"],
            "ManagedResource::close panicked"
        );

        let (document, failure) = finalize_result("handshake", Ok(json!({"cleanup": cleanup})));
        assert!(failure.is_some());
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(
            document["result"]["cleanup"]["runtime"]["report"]["phase"],
            "ResourceCleanup"
        );
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn final_close_neutralization_failure_cannot_pass_cleanup() {
        let harness = observed_harness(Box::new(FinalNeutralFailureTransport), "FINAL-NEUTRAL");
        let operation = harness
            .connect(ConnectOptions::default())
            .expect("synthetic connect");

        let cleanup = harness.close();

        assert_eq!(cleanup["kind"], "harness_cleanup");
        assert_eq!(cleanup["controller"]["neutralization"], "not_delivered");
        assert_eq!(cleanup["controller"]["attempts"][0]["outcome"], "failed");
        assert_eq!(cleanup["runtime"]["outcome"], "Closed");
        assert_eq!(cleanup["runtime"]["counts"]["active_operations"], 0);
        assert_eq!(cleanup["runtime"]["counts"]["active_resources"], 0);
        assert_eq!(cleanup["runtime"]["counts"]["active_tasks"], 0);
        assert_eq!(cleanup["succeeded"], false);

        let (document, failure) = finalize_result(
            "handshake",
            Ok(json!({
                "operation": operation,
                "actual_baud": 115_200,
                "cleanup": cleanup,
            })),
        );
        assert!(failure.is_some());
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn connected_close_requires_one_complete_final_neutral_attempt() {
        let harness = observed_harness(Box::new(NoopTransport), "FINAL-NEUTRAL-SUCCESS");
        harness
            .connect(ConnectOptions::default())
            .expect("synthetic connect");

        let cleanup = harness.close();

        assert!(cleanup_succeeded(&cleanup));
        assert_eq!(cleanup["controller"]["neutralization"], "accepted");
        assert_eq!(
            cleanup["controller"]["controller_resource_id"],
            cleanup["controller"]["attempts"][0]["resource_id"]
        );
        assert_eq!(
            cleanup["controller"]["attempts"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            cleanup["controller"]["attempts"][0]["operation_id"],
            Value::Null
        );
        assert_eq!(cleanup["controller"]["attempts"][0]["total_bytes"], 8);
        assert_eq!(cleanup["controller"]["attempts"][0]["accepted_bytes"], 8);
        assert_eq!(cleanup["controller"]["attempts"][0]["outcome"], "accepted");
    }

    #[test]
    fn disconnected_close_records_not_required_without_claiming_delivery() {
        let harness = observed_harness(Box::new(NoopTransport), "FINAL-NEUTRAL-NOT-REQUIRED");

        let cleanup = harness.close();

        assert!(cleanup_succeeded(&cleanup));
        assert_eq!(cleanup["controller"]["pre_close_state"], "Disconnected");
        assert!(
            cleanup["controller"]["controller_resource_id"]
                .as_u64()
                .is_some_and(|resource_id| resource_id != 0)
        );
        assert_eq!(cleanup["controller"]["neutralization"], "not_required");
        assert_eq!(cleanup["controller"]["attempts"], json!([]));
    }

    #[test]
    fn partial_final_neutral_failure_preserves_prefix_and_error() {
        let harness = observed_harness(
            Box::new(PartialFinalNeutralFailureTransport {
                accepted_prefix: false,
            }),
            "FINAL-NEUTRAL-PARTIAL",
        );
        harness
            .connect(ConnectOptions::default())
            .expect("synthetic connect");

        let cleanup = harness.close();

        assert!(!cleanup_succeeded(&cleanup));
        assert_eq!(cleanup["controller"]["neutralization"], "not_delivered");
        assert_eq!(cleanup["controller"]["attempts"][0]["total_bytes"], 8);
        assert_eq!(cleanup["controller"]["attempts"][0]["accepted_bytes"], 3);
        assert_eq!(cleanup["controller"]["attempts"][0]["outcome"], "failed");
        assert_eq!(
            cleanup["controller"]["attempts"][0]["structured_error"]["kind"],
            "Io"
        );
        assert_eq!(cleanup["runtime"]["outcome"], "Closed");
    }

    #[test]
    fn cleanup_projection_rejects_legacy_partial_and_misbound_evidence() {
        let harness = observed_harness(Box::new(NoopTransport), "FINAL-NEUTRAL-VALIDATOR");
        harness
            .connect(ConnectOptions::default())
            .expect("synthetic connect");
        let valid = harness.close();
        assert!(cleanup_succeeded(&valid));

        let mut invalid = vec![valid["runtime"].clone()];
        let mut missing_controller = valid.clone();
        missing_controller
            .as_object_mut()
            .expect("cleanup")
            .remove("controller");
        invalid.push(missing_controller);

        let mut operation_bound = valid.clone();
        operation_bound["controller"]["attempts"][0]["operation_id"] = json!(71);
        invalid.push(operation_bound);

        let mut partial = valid.clone();
        partial["controller"]["attempts"][0]["accepted_bytes"] = json!(3);
        invalid.push(partial);

        for field in ["structured_error", "diagnostic"] {
            let mut missing_nullable = valid.clone();
            missing_nullable["controller"]["attempts"][0]
                .as_object_mut()
                .expect("attempt")
                .remove(field);
            invalid.push(missing_nullable);
        }

        let mut missing_runtime_diagnostic = valid.clone();
        missing_runtime_diagnostic["runtime"]
            .as_object_mut()
            .expect("runtime cleanup")
            .remove("diagnostic");
        invalid.push(missing_runtime_diagnostic);

        let mut contradictory_runtime = valid.clone();
        contradictory_runtime["runtime"]["report"] = json!({
            "phase": "ResourceCleanup",
            "diagnostic": "contradicts Closed",
        });
        invalid.push(contradictory_runtime);

        for path in ["outer", "controller", "attempt", "counts"] {
            let mut extra_field = valid.clone();
            match path {
                "outer" => extra_field["unexpected"] = json!(true),
                "controller" => extra_field["controller"]["unexpected"] = json!(true),
                "attempt" => extra_field["controller"]["attempts"][0]["unexpected"] = json!(true),
                "counts" => extra_field["runtime"]["counts"]["unexpected"] = json!(0),
                _ => unreachable!("fixed mutation path"),
            }
            invalid.push(extra_field);
        }

        let mut missing_resource_anchor = valid.clone();
        missing_resource_anchor["controller"]
            .as_object_mut()
            .expect("controller cleanup")
            .remove("controller_resource_id");
        invalid.push(missing_resource_anchor);

        let mut foreign_resource = valid.clone();
        let resource_id = foreign_resource["controller"]["attempts"][0]["resource_id"]
            .as_u64()
            .expect("resource ID");
        foreign_resource["controller"]["attempts"][0]["resource_id"] =
            json!(resource_id.checked_add(1).expect("foreign resource ID"));
        invalid.push(foreign_resource);

        let mut unknown_error = valid.clone();
        let mut operation_failure = unknown_error["controller"]["attempts"][0].clone();
        let final_sequence = operation_failure["sequence"]
            .as_u64()
            .expect("final sequence");
        operation_failure["operation_id"] = json!(71);
        operation_failure["accepted_bytes"] = json!(0);
        operation_failure["outcome"] = json!("failed");
        operation_failure["structured_error"] = json!({
            "kind": "InventedError",
            "message": "unknown stable kind",
        });
        unknown_error["controller"]["attempts"][0]["sequence"] =
            json!(final_sequence.checked_add(1).expect("next sequence"));
        unknown_error["controller"]["attempts"]
            .as_array_mut()
            .expect("attempts")
            .insert(0, operation_failure);
        let mut known_operation_error = unknown_error.clone();
        known_operation_error["controller"]["attempts"][0]["structured_error"]["kind"] =
            json!("Io");
        assert!(cleanup_succeeded(&known_operation_error));
        invalid.push(unknown_error);

        let mut post_final_attempt = valid.clone();
        let mut late_operation = post_final_attempt["controller"]["attempts"][0].clone();
        let final_sequence = late_operation["sequence"].as_u64().expect("final sequence");
        late_operation["sequence"] = json!(final_sequence.checked_add(1).expect("late sequence"));
        late_operation["operation_id"] = json!(72);
        post_final_attempt["controller"]["attempts"]
            .as_array_mut()
            .expect("attempts")
            .push(late_operation);
        invalid.push(post_final_attempt);

        let mut outer_contradiction = valid;
        outer_contradiction["succeeded"] = json!(false);
        invalid.push(outer_contradiction);

        for cleanup in invalid {
            assert!(!cleanup_succeeded(&cleanup));
            assert_cleanup_failure(
                "handshake",
                json!({
                    "operation": {"state": "Succeeded"},
                    "actual_baud": 115_200,
                    "cleanup": cleanup,
                }),
            );
        }
    }

    #[test]
    fn controller_and_runtime_close_failures_are_both_preserved() {
        let harness = observed_harness(
            Box::new(FinalNeutralFailureTransport),
            "FINAL-NEUTRAL-DOUBLE-FAILURE",
        );
        let resource: Arc<dyn ManagedResource> = Arc::new(PanickingResource);
        let _registration = harness
            .runtime
            .register_resource(resource.clone())
            .expect("resource");
        harness
            .connect(ConnectOptions::default())
            .expect("synthetic connect");

        let cleanup = harness.close();

        assert_eq!(cleanup["succeeded"], false);
        assert_eq!(cleanup["controller"]["neutralization"], "not_delivered");
        assert_eq!(cleanup["controller"]["attempts"][0]["outcome"], "failed");
        assert_eq!(cleanup["runtime"]["outcome"], "Failed");
        assert_eq!(cleanup["runtime"]["report"]["phase"], "ResourceCleanup");
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
    fn timing_csv_is_rendered_without_writing_an_artifact_path() {
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));

        let bytes = timing_csv_bytes(&telemetry).expect("render timing CSV");

        assert_eq!(
            bytes,
            b"sequence,command_admitted_ns,lane_wake_ns,dispatch_ns,write_entered_ns,transport_accepted_ns\n"
        );
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
        assert!(!directory.0.join(RESERVATION_FILE_NAME).exists());
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
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
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
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
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
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
        let reservation: Value = serde_json::from_slice(
            &fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("read reservation"),
        )
        .expect("parse reservation");
        let staging = reservation["primary_artifact"]["staging"]
            .as_str()
            .expect("primary staging");
        assert!(directory.0.join(staging).exists());
    }

    #[test]
    fn different_commands_cannot_share_a_run_directory() {
        let directory = TestDirectory::new("cross-command-reservation");
        let _first = ArtifactReservation::begin("unknown", &directory.0).expect("first reserve");

        let second = ArtifactReservation::begin("amiibo", &directory.0);

        assert!(second.is_err());
    }
}
