#![forbid(unsafe_code)]

mod amiibo;
mod artifact;
mod checkpoint;
mod device;
mod faults;
mod journal;
mod operator;
mod provenance;
mod telemetry;

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use amiibo::{
    AmiiboEvidenceBinding, AmiiboEvidenceConfig, AmiiboEvidenceRecorder, AmiiboEvidenceTransport,
};
#[cfg(test)]
use artifact::RESERVATION_FILE_NAME;
use artifact::{ArtifactReservation, AuxiliaryKind, RunStartMetadata, SEQUENCE_TIMINGS_FILE_NAME};
use device::{
    AdmissionDecision, AdmissionEvidence, AdmittedDevice, DeviceDiscovery, DeviceTargetRequest,
    IdentityGuardedByteIoFactory, IdentityOpenRecorder, InnerOpenOutcome, OpenGuardCheck,
    RebindDecision, SystemDeviceDiscovery, admit_device, rebind_device,
};
use easycon_controller::{
    AUTO_BAUD_RATES, AckFrame, AckRequest, AmiiboLimits, AmiiboSaveOptions, AmiiboSelectOptions,
    ConnectOptions, ControllerAction, ControllerLeaseState, ControllerOptions, ControllerSession,
    ControllerSnapshot, ControllerState, ControllerTransport, HandshakeRequest, PreciseSequence,
    SequenceStep, TransportError, WriteContext, WriteKind, WriteRequest,
};
use easycon_hardware_qualification::{option_value, required_value, value_or};
use easycon_model::{Button, Hat, ResourceId, StickPosition};
use easycon_runtime::{
    Clock, CloseOutcome, CloseRejection, Operation, OperationState, Runtime, RuntimeCounts,
    SystemClock, TransitionOutcome, WaitResult, WaitTimeout,
};
use easycon_serial::{
    ByteIo, ByteIoFactory, ByteIoOperation, ByteIoRequest, SerialControllerTransport, SerialError,
    SerialPortDescriptor, WindowsByteIoFactory, discover_system_ports,
};
use serde_json::{Value, json};

use journal::JournalEventKind;
use operator::{
    ActionMarker, ConsoleOperatorPort, InterruptToken, ObservationRequest, OperatorOutcome,
    OperatorPort, install_console_interrupt_handler,
};
use provenance::{RuntimeProvenance, sha256_bytes};
use telemetry::LogicalReportTelemetry;

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const MINIMUM_REPORT_INTERVAL_NS: u64 = 30_000_000;
const HOTPLUG_POLL_INTERVAL: Duration = Duration::from_millis(250);
const INTERRUPT_POLL_SLICE: Duration = Duration::from_millis(50);

struct RunControl<'a> {
    reservation: &'a mut ArtifactReservation,
    operator: &'a mut dyn OperatorPort,
    interrupt: InterruptToken,
    observation_sequence: u64,
    interrupt_recorded: bool,
    cancellation_terminal_recorded: bool,
}

impl<'a> RunControl<'a> {
    fn new(
        reservation: &'a mut ArtifactReservation,
        operator: &'a mut dyn OperatorPort,
        interrupt: InterruptToken,
    ) -> Self {
        Self {
            reservation,
            operator,
            interrupt,
            observation_sequence: 0,
            interrupt_recorded: false,
            cancellation_terminal_recorded: false,
        }
    }

    fn record_event(&mut self, kind: JournalEventKind, payload: Value) -> Result<(), String> {
        self.reservation.record_event(kind, payload)
    }

    fn lease_id(&self) -> &str {
        self.reservation.lease_id()
    }

    fn journal_writer(&self) -> Result<journal::JournalWriter, String> {
        self.reservation.journal_writer()
    }

    fn marker(
        &self,
        command: &str,
        expected_stable_id: &str,
        observed_stable_id: &str,
        action_id: &str,
        label: &str,
    ) -> ActionMarker {
        ActionMarker {
            lease_id: self.reservation.lease_id().to_owned(),
            command: command.to_owned(),
            expected_stable_id: expected_stable_id.to_owned(),
            observed_stable_id: observed_stable_id.to_owned(),
            action_id: action_id.to_owned(),
            label: label.to_owned(),
        }
    }

    fn announce(&mut self, marker: &ActionMarker) -> Result<(), CommandFailure> {
        self.operator
            .announce(marker)
            .map_err(|error| CommandFailure::new("operator_marker", error))
    }

    fn observe_action(
        &mut self,
        marker: ActionMarker,
        observation_window_ms: u64,
        response_timeout: Duration,
        operations: ActionOperationEvidence,
        neutral_snapshot: Value,
    ) -> Result<(OperatorOutcome, Value), CommandFailure> {
        let request = ObservationRequest {
            marker: marker.clone(),
            response_timeout,
        };
        let outcome = self
            .operator
            .observe(&request, &self.interrupt)
            .map_err(|error| CommandFailure::new("operator_response", error))?;
        if outcome == OperatorOutcome::Interrupted {
            self.record_interrupt_without_operation()?;
        }
        self.observation_sequence = self.observation_sequence.checked_add(1).ok_or_else(|| {
            CommandFailure::new("operator_response", "observation sequence exhausted")
        })?;
        let response_timeout_ms = u64::try_from(response_timeout.as_millis()).map_err(|_| {
            CommandFailure::new("operator_response", "response timeout does not fit u64")
        })?;
        let observation = json!({
            "sequence": self.observation_sequence,
            "lease_id": marker.lease_id,
            "command": marker.command,
            "expected_stable_id": marker.expected_stable_id,
            "observed_stable_id": marker.observed_stable_id,
            "action_id": marker.action_id,
            "label": marker.label,
            "observation_window_ms": observation_window_ms,
            "response_timeout_ms": response_timeout_ms,
            "outcome": outcome.as_str(),
            "active_operation": operations.active,
            "release_operation": operations.release,
            "neutral_operation": operations.neutral,
            "neutral_snapshot": neutral_snapshot,
        });
        self.record_event(JournalEventKind::OperatorObservation, observation.clone())
            .map_err(|error| CommandFailure::new("operator_observation_journal", error))?;
        if outcome == OperatorOutcome::Interrupted {
            self.record_cancellation_without_operation()?;
        }
        Ok((outcome, observation))
    }

    fn record_interrupt_without_operation(&mut self) -> Result<(), CommandFailure> {
        self.interrupt.request();
        if self.interrupt_recorded {
            return Ok(());
        }
        self.record_event(
            JournalEventKind::InterruptRequested,
            json!({
                "source": "operator_interrupt",
                "operation_id": null,
                "operation_state": "none",
            }),
        )
        .map_err(|error| CommandFailure::new("interrupt_journal", error))?;
        self.interrupt_recorded = true;
        Ok(())
    }

    fn record_cancellation_without_operation(&mut self) -> Result<(), CommandFailure> {
        if self.cancellation_terminal_recorded {
            return Ok(());
        }
        self.record_event(
            JournalEventKind::CancellationTerminal,
            json!({
                "source": "operator_interrupt",
                "operation_id": null,
                "terminal_state": "no_current_operation",
                "cancellation_reason": null,
            }),
        )
        .map_err(|error| CommandFailure::new("cancellation_terminal_journal", error))?;
        self.cancellation_terminal_recorded = true;
        Ok(())
    }

    fn checkpoint(&mut self, stage: &'static str) -> Result<(), CommandFailure> {
        if !self.interrupt.is_requested() {
            return Ok(());
        }
        self.record_interrupt_without_operation()?;
        self.record_cancellation_without_operation()?;
        Err(CommandFailure::interrupted(
            stage,
            "operator interrupt stopped action admission",
            None,
        ))
    }

    fn cancel_operation(&mut self, operation: &Operation, stage: &'static str) -> CommandFailure {
        let snapshot_before = operation.snapshot();
        let interrupt_journal = self.record_interrupt_for_operation(operation, &snapshot_before);
        let cancel_outcome = operation.cancel();
        let terminal = wait_terminal_sliced(operation, OPERATION_TIMEOUT);
        if let Err(error) = interrupt_journal {
            return CommandFailure::with_operation(
                "interrupt_journal",
                format!("{error}; cancel outcome {cancel_outcome:?}"),
                operation.clone(),
            );
        }
        let snapshot = match terminal {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return CommandFailure::with_operation(
                    "interrupt_cancel_settle",
                    format!("{error}; cancel outcome {cancel_outcome:?}"),
                    operation.clone(),
                );
            }
        };
        if let Err(error) =
            self.record_cancellation_for_operation(operation, cancel_outcome, &snapshot)
        {
            return CommandFailure::with_operation(
                "cancellation_terminal_journal",
                error,
                operation.clone(),
            );
        }
        let cancellation_is_exact = snapshot.state == OperationState::Cancelled
            && snapshot.cancellation_reason == Some(easycon_runtime::CancellationReason::Requested)
            && matches!(
                cancel_outcome,
                TransitionOutcome::Applied | TransitionOutcome::Unchanged
            );
        let success_won_race = snapshot.state == OperationState::Succeeded
            && cancel_outcome == TransitionOutcome::AlreadyTerminal;
        if !cancellation_is_exact && !success_won_race {
            return CommandFailure::with_operation(
                "interrupt_cancel_terminal",
                format!(
                    "operator interrupt ended operation in {:?} with reason {:?} and request outcome {cancel_outcome:?}",
                    snapshot.state, snapshot.cancellation_reason
                ),
                operation.clone(),
            );
        }
        CommandFailure::interrupted(
            stage,
            "operator interrupt cancelled command execution",
            Some(operation.clone()),
        )
    }

    fn record_interrupt_for_operation(
        &mut self,
        operation: &Operation,
        snapshot: &easycon_runtime::OperationSnapshot,
    ) -> Result<(), String> {
        if self.interrupt_recorded {
            return Ok(());
        }
        self.record_event(
            JournalEventKind::InterruptRequested,
            json!({
                "source": "operator_interrupt",
                "operation_id": operation.id().get(),
                "operation_state": format!("{:?}", snapshot.state),
            }),
        )?;
        self.interrupt_recorded = true;
        Ok(())
    }

    fn record_cancellation_for_operation(
        &mut self,
        operation: &Operation,
        cancel_outcome: TransitionOutcome,
        snapshot: &easycon_runtime::OperationSnapshot,
    ) -> Result<(), String> {
        if self.cancellation_terminal_recorded {
            return Ok(());
        }
        self.record_event(
            JournalEventKind::CancellationTerminal,
            json!({
                "source": "operator_interrupt",
                "operation_id": operation.id().get(),
                "request_outcome": format!("{cancel_outcome:?}"),
                "terminal": operation_snapshot_json(snapshot),
            }),
        )?;
        self.cancellation_terminal_recorded = true;
        Ok(())
    }
}

struct ActionOperationEvidence {
    active: Value,
    release: Value,
    neutral: Value,
}

trait QualificationDelay {
    fn wait(&self, duration: Duration);
}

struct ThreadDelay;

impl QualificationDelay for ThreadDelay {
    fn wait(&self, duration: Duration) {
        thread::sleep(duration);
    }
}

fn wait_controlled(
    control: &mut RunControl<'_>,
    duration: Duration,
    delay: &dyn QualificationDelay,
    stage: &'static str,
) -> Result<(), CommandFailure> {
    let mut remaining = duration;
    while !remaining.is_zero() {
        control.checkpoint(stage)?;
        let current = remaining.min(INTERRUPT_POLL_SLICE);
        delay.wait(current);
        remaining = remaining.saturating_sub(current);
    }
    control.checkpoint(stage)
}

#[derive(Clone, Debug)]
struct HandshakeAttempt {
    baud: u32,
    succeeded: bool,
    error: Option<String>,
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
    logical_reports: LogicalReportTelemetry,
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

struct ObservedByteIo {
    inner: Box<dyn ByteIo>,
    telemetry: Arc<Mutex<Telemetry>>,
}

impl ByteIo for ObservedByteIo {
    fn read(&mut self, buffer: &mut [u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        self.inner.read(buffer, request)
    }

    fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        let operation = request.operation;
        let result = self.inner.write(buffer, request);
        if let (ByteIoOperation::ControllerWrite(context), Err(error)) = (operation, &result) {
            self.telemetry
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .logical_reports
                .record_native_error(context, error);
        }
        result
    }

    fn discard_input(&mut self, request: ByteIoRequest) -> Result<(), SerialError> {
        self.inner.discard_input(request)
    }

    fn close(&mut self) {
        self.inner.close();
    }
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
        result.map(|inner| {
            Box::new(ObservedByteIo {
                inner,
                telemetry: self.telemetry.clone(),
            }) as Box<dyn ByteIo>
        })
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
        if context.kind == WriteKind::Report {
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .logical_reports
                .begin(context, remaining, entered);
        }
        if context.kind == WriteKind::Neutralize {
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .begin_neutralization(context, remaining, entered);
        }
        let result = self.inner.write(request);
        let accepted_at = self.clock.now_ns();
        if context.kind == WriteKind::Report {
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .logical_reports
                .finish(context, remaining, accepted_at, &result);
        }
        if context.kind == WriteKind::Neutralize {
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .finish_neutralization(context, remaining, &result);
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
    identity_open_recorder: IdentityOpenRecorder,
    descriptor: SerialPortDescriptor,
}

impl Harness {
    fn new(
        target: &AdmittedDevice,
        discovery: Arc<dyn DeviceDiscovery>,
        options: ControllerOptions,
    ) -> Result<Self, String> {
        Self::new_with_amiibo_evidence(target, discovery, options, None)
    }

    fn new_with_amiibo_evidence(
        target: &AdmittedDevice,
        discovery: Arc<dyn DeviceDiscovery>,
        options: ControllerOptions,
        amiibo_evidence: Option<AmiiboEvidenceConfig>,
    ) -> Result<Self, String> {
        let descriptor = target.descriptor().clone();
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let identity_open_recorder = IdentityOpenRecorder::default();
        let guarded_factory = IdentityGuardedByteIoFactory::new(
            Box::new(WindowsByteIoFactory),
            discovery,
            target.clone(),
            identity_open_recorder.clone(),
        );
        let observed_factory = ObservedByteIoFactory {
            inner: Box::new(guarded_factory),
            telemetry: telemetry.clone(),
        };
        let serial = SerialControllerTransport::new(
            clock.clone(),
            descriptor.clone(),
            Box::new(observed_factory),
        );
        let transport: Box<dyn ControllerTransport> = match amiibo_evidence {
            Some(evidence) => Box::new(AmiiboEvidenceTransport::new(
                Box::new(serial),
                evidence.journal,
                evidence.binding,
                evidence.recorder,
            )),
            None => Box::new(serial),
        };
        let observed = ObservedTransport {
            inner: transport,
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
            identity_open_recorder,
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

    fn identity_open_attempts(&self) -> Value {
        identity_open_attempts_json(&self.identity_open_recorder)
    }

    fn logical_report_telemetry(&self) -> Value {
        let csv_path = {
            let telemetry = self
                .telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            (telemetry.logical_reports.attempt_count() > 512).then_some(SEQUENCE_TIMINGS_FILE_NAME)
        };
        latency_json(&self.telemetry, self.actual_baud(), csv_path)
    }
}

struct CommandFailure {
    kind: CommandFailureKind,
    stage: &'static str,
    message: String,
    operation: Option<Operation>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CommandFailureKind {
    Failed,
    Interrupted,
}

impl CommandFailure {
    fn new(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: CommandFailureKind::Failed,
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
            kind: CommandFailureKind::Failed,
            stage,
            message: message.into(),
            operation: Some(operation),
        }
    }

    fn interrupted(
        stage: &'static str,
        message: impl Into<String>,
        operation: Option<Operation>,
    ) -> Self {
        Self {
            kind: CommandFailureKind::Interrupted,
            stage,
            message: message.into(),
            operation,
        }
    }
}

fn pre_harness_cancellation_result(command: &str, failure: CommandFailure) -> Value {
    json!({
        "command": command,
        "operator_terminal_outcome": "interrupted",
        "operator_cancellation": {
            "stage": failure.stage,
            "message": failure.message,
            "operation": failure.operation.as_ref().map(operation_json),
        },
        "resources": {},
    })
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
        match failure.as_ref().map(|failure| failure.kind) {
            Some(CommandFailureKind::Failed) => {
                append_cleanup_error(&mut result, &close_failure);
            }
            Some(CommandFailureKind::Interrupted) => {
                append_cleanup_error(&mut result, &close_failure);
                failure = Some(close_failure);
            }
            None => failure = Some(close_failure),
        }
    }
    if let Some(failure) = failure {
        match failure.kind {
            CommandFailureKind::Failed => {
                result["execution_error"] = json!({
                    "stage": failure.stage,
                    "message": failure.message,
                });
            }
            CommandFailureKind::Interrupted => {
                result["operator_terminal_outcome"] = json!("interrupted");
                result["operator_cancellation"] = json!({
                    "stage": failure.stage,
                    "message": failure.message,
                    "operation": failure.operation.as_ref().map(operation_json),
                });
            }
        }
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
    match failure.kind {
        CommandFailureKind::Failed => {
            result["execution_error"] = json!({
                "stage": failure.stage,
                "message": failure.message,
            });
        }
        CommandFailureKind::Interrupted => {
            result["operator_terminal_outcome"] = json!("interrupted");
            result["operator_cancellation"] = json!({
                "stage": failure.stage,
                "message": failure.message,
                "operation": failure.operation.as_ref().map(operation_json),
            });
        }
    }
}

fn harness_evidence_json(harness: &Harness) -> Value {
    json!({
        "port": port_json(&harness.descriptor),
        "actual_baud": harness.actual_baud(),
        "handshake_attempts": handshake_json(&harness.telemetry),
        "native_open_attempts": harness.native_open_attempts(),
        "identity_open_attempts": harness.identity_open_attempts(),
        "logical_report_telemetry": harness.logical_report_telemetry(),
        "pre_cleanup_snapshot": snapshot_json(&harness.controller),
    })
}

#[cfg(test)]
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

fn connect_for_command_controlled(
    harness: &Harness,
    control: &mut RunControl<'_>,
    options: ConnectOptions,
    admit_stage: &'static str,
    wait_stage: &'static str,
    terminal_stage: &'static str,
) -> Result<Value, CommandFailure> {
    control.checkpoint(admit_stage)?;
    let operation = harness
        .controller
        .connect(options)
        .map_err(|error| CommandFailure::new(admit_stage, error.to_string()))?;
    wait_for_command_success_controlled(
        operation,
        control,
        OPERATION_TIMEOUT,
        wait_stage,
        terminal_stage,
    )
}

#[cfg(test)]
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

fn wait_for_command_success_controlled(
    operation: Operation,
    control: &mut RunControl<'_>,
    timeout: Duration,
    wait_stage: &'static str,
    terminal_stage: &'static str,
) -> Result<Value, CommandFailure> {
    let (operation, state) =
        wait_for_command_terminal_core(operation, timeout, wait_stage, Some(control))?;
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

#[cfg(test)]
fn wait_for_command_terminal(
    operation: Operation,
    timeout: Duration,
    wait_stage: &'static str,
) -> Result<(Operation, OperationState), CommandFailure> {
    wait_for_command_terminal_core(operation, timeout, wait_stage, None)
}

fn wait_for_command_terminal_controlled(
    operation: Operation,
    control: &mut RunControl<'_>,
    timeout: Duration,
    wait_stage: &'static str,
) -> Result<(Operation, OperationState), CommandFailure> {
    wait_for_command_terminal_core(operation, timeout, wait_stage, Some(control))
}

fn wait_for_command_terminal_core(
    operation: Operation,
    timeout: Duration,
    wait_stage: &'static str,
    mut control: Option<&mut RunControl<'_>>,
) -> Result<(Operation, OperationState), CommandFailure> {
    let started = Instant::now();
    let snapshot = loop {
        if let Some(control) = control.as_deref_mut()
            && control.interrupt.is_requested()
        {
            return Err(control.cancel_operation(&operation, wait_stage));
        }
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(CommandFailure::with_operation(
                wait_stage,
                format!("operation {} wait timed out", operation.id().get()),
                operation,
            ));
        };
        if remaining.is_zero() {
            return Err(CommandFailure::with_operation(
                wait_stage,
                format!("operation {} wait timed out", operation.id().get()),
                operation,
            ));
        }
        match operation.wait(WaitTimeout::For(remaining.min(INTERRUPT_POLL_SLICE))) {
            WaitResult::Completed(snapshot) => break snapshot,
            WaitResult::Timeout => {}
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

struct AmiiboOperationWaitSpec {
    timeout: Duration,
    wait_stage: &'static str,
    terminal_stage: &'static str,
    journal_kind: JournalEventKind,
    result_field: &'static str,
    durable_field: &'static str,
}

fn wait_for_amiibo_operation(
    operation: Operation,
    control: &mut RunControl<'_>,
    result: &mut Value,
    spec: AmiiboOperationWaitSpec,
) -> Result<(), CommandFailure> {
    match wait_for_command_terminal_controlled(operation, control, spec.timeout, spec.wait_stage) {
        Ok((operation, state)) => {
            let evidence = operation_json(&operation);
            result[spec.result_field] = evidence.clone();
            control
                .record_event(
                    spec.journal_kind,
                    json!({
                        "layer": "operation",
                        "stage": spec.result_field,
                        "operation": evidence,
                    }),
                )
                .map_err(|error| {
                    CommandFailure::with_operation(
                        spec.terminal_stage,
                        format!("cannot persist Amiibo operation terminal: {error}"),
                        operation.clone(),
                    )
                })?;
            result[spec.durable_field] = json!(true);
            if state == OperationState::Succeeded {
                Ok(())
            } else {
                Err(CommandFailure::with_operation(
                    spec.terminal_stage,
                    operation_failure(&operation),
                    operation,
                ))
            }
        }
        Err(failure) => {
            if let Some(operation) = failure.operation.as_ref() {
                let evidence = operation_json(operation);
                result[spec.result_field] = evidence.clone();
                if operation.snapshot().state.is_terminal() {
                    control
                        .record_event(
                            spec.journal_kind,
                            json!({
                                "layer": "operation",
                                "stage": spec.result_field,
                                "operation": evidence,
                            }),
                        )
                        .map_err(|error| {
                            CommandFailure::with_operation(
                                spec.terminal_stage,
                                format!("cannot persist Amiibo operation terminal: {error}"),
                                operation.clone(),
                            )
                        })?;
                    result[spec.durable_field] = json!(true);
                }
            }
            Err(failure)
        }
    }
}

fn run_with_device_admission(
    command: &'static str,
    arguments: &[String],
    control: &mut RunControl<'_>,
    runner: impl FnOnce(
        AdmittedDevice,
        Arc<dyn DeviceDiscovery>,
        &mut RunControl<'_>,
    ) -> Result<Value, String>,
) -> Result<Value, String> {
    run_with_device_admission_controlled_using(
        command,
        arguments,
        Arc::new(SystemDeviceDiscovery),
        control,
        runner,
    )
}

fn run_with_device_admission_controlled_using(
    command: &'static str,
    arguments: &[String],
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
    runner: impl FnOnce(
        AdmittedDevice,
        Arc<dyn DeviceDiscovery>,
        &mut RunControl<'_>,
    ) -> Result<Value, String>,
) -> Result<Value, String> {
    if control.interrupt.is_requested() {
        let request = device_target_request(arguments)?;
        let failure = control
            .checkpoint("identity_admission_interrupt")
            .expect_err("requested interrupt must stop admission");
        let mut result = pre_harness_cancellation_result(command, failure);
        result["device_target"] = device_target_json(&request);
        result["identity_admission"] = json!({
            "status": "not_run",
            "reason": "operator_interrupt",
            "snapshot": [],
            "observed_expected": null,
            "observed_hint": null,
            "structured_error": null,
        });
        return Ok(result);
    }
    run_with_device_admission_core(
        command,
        arguments,
        discovery,
        control,
        |control, payload| control.record_event(JournalEventKind::IdentityAdmission, payload),
        |target, discovery, control| match control.checkpoint("runner_interrupt") {
            Ok(()) => runner(target, discovery, control),
            Err(failure) => Ok(pre_harness_cancellation_result(command, failure)),
        },
    )
}

#[cfg(test)]
fn run_with_device_admission_using(
    command: &'static str,
    arguments: &[String],
    discovery: Arc<dyn DeviceDiscovery>,
    runner: impl FnOnce(AdmittedDevice, Arc<dyn DeviceDiscovery>) -> Result<Value, String>,
) -> Result<Value, String> {
    run_with_device_admission_core(
        command,
        arguments,
        discovery,
        &mut (),
        |(), _| Ok(()),
        |target, discovery, ()| runner(target, discovery),
    )
}

fn run_with_device_admission_core<C>(
    command: &'static str,
    arguments: &[String],
    discovery: Arc<dyn DeviceDiscovery>,
    context: &mut C,
    mut record_admission: impl FnMut(&mut C, Value) -> Result<(), String>,
    runner: impl FnOnce(AdmittedDevice, Arc<dyn DeviceDiscovery>, &mut C) -> Result<Value, String>,
) -> Result<Value, String> {
    let request = device_target_request(arguments)?;
    match admit_device(discovery.as_ref(), request.clone()) {
        Ok(AdmissionDecision::Admitted(target)) => {
            let evidence = target.evidence().clone();
            record_admission(
                context,
                json!({
                    "command": command,
                    "device_target": device_target_json(evidence.request()),
                    "identity_admission": admission_evidence_json(&evidence, "admitted"),
                }),
            )?;
            match runner(target, discovery, context) {
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
        Ok(AdmissionDecision::Rejected(evidence)) => {
            let result = json!({
                "command": command,
                "device_target": device_target_json(evidence.request()),
                "identity_admission": admission_evidence_json(&evidence, "rejected"),
                "capability_inference": "none",
            });
            record_admission(context, result.clone())?;
            Ok(result)
        }
        Ok(AdmissionDecision::Ambiguous(evidence)) => {
            let result = json!({
                "command": command,
                "device_target": device_target_json(evidence.request()),
                "identity_admission": admission_evidence_json(&evidence, "ambiguous"),
                "capability_inference": "none",
                "execution_error": {
                    "stage": "identity_admission",
                    "message": "system discovery returned an ambiguous serial snapshot",
                },
            });
            record_admission(context, result.clone())?;
            Ok(result)
        }
        Err(error) => {
            let result = json!({
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
            });
            record_admission(context, result.clone())?;
            Ok(result)
        }
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
    let provenance = RuntimeProvenance::capture();
    let artifact_dir = artifact_dir(&arguments)?;
    let checkpoint_inputs = if command == "checkpoint" {
        Some(checkpoint::CheckpointInputs::parse(
            &arguments,
            &artifact_dir,
        )?)
    } else {
        None
    };
    fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let mut reservation = ArtifactReservation::begin_journaled(
        command,
        &artifact_dir,
        RunStartMetadata {
            normalized_arguments: normalized_arguments(&arguments),
            provenance: provenance.to_json(),
        },
    )?;
    reservation.record_event(
        JournalEventKind::CommandDispatchStarted,
        json!({"command": command}),
    )?;
    let mut sequence_timings = if command == "sequence" {
        Some(LogicalReportTelemetry::default().csv_bytes()?)
    } else {
        None
    };
    let mut operator = ConsoleOperatorPort::new();
    let interrupt = InterruptToken::default();
    let handler_install = install_console_interrupt_handler(&interrupt);
    let execution = if let Err(error) = handler_install {
        Err(error)
    } else {
        let mut control = RunControl::new(&mut reservation, &mut operator, interrupt);
        if control.interrupt.is_requested() {
            let failure = control
                .checkpoint("command_dispatch_interrupt")
                .expect_err("requested interrupt must stop dispatch");
            Ok(pre_harness_cancellation_result(command, failure))
        } else {
            match command {
                "checkpoint" => checkpoint::run(
                    checkpoint_inputs
                        .as_ref()
                        .expect("checkpoint inputs were parsed before reservation"),
                    &mut control,
                ),
                "discover" => run_discover(&arguments, &mut control),
                "handshake" => run_with_device_admission(
                    "handshake",
                    &arguments,
                    &mut control,
                    |target, discovery, control| {
                        run_handshake(&arguments, target, discovery, control)
                    },
                ),
                "smoke" => run_with_device_admission(
                    "smoke",
                    &arguments,
                    &mut control,
                    |target, discovery, control| run_smoke(&arguments, target, discovery, control),
                ),
                "home-wake" => run_with_device_admission(
                    "home-wake",
                    &arguments,
                    &mut control,
                    |target, discovery, control| {
                        run_home_wake(&arguments, target, discovery, control)
                    },
                ),
                "faults" => {
                    run_with_device_admission("faults", &arguments, &mut control, run_faults)
                }
                "hotplug" => run_with_device_admission(
                    "hotplug",
                    &arguments,
                    &mut control,
                    |target, discovery, control| {
                        run_hotplug(&arguments, target, discovery, control)
                    },
                ),
                "lifecycle" => run_with_device_admission(
                    "lifecycle",
                    &arguments,
                    &mut control,
                    |target, discovery, control| {
                        run_lifecycle(&arguments, target, discovery, control)
                    },
                ),
                "sequence" => run_with_device_admission(
                    "sequence",
                    &arguments,
                    &mut control,
                    |target, discovery, control| {
                        run_sequence(&arguments, target, discovery, control).map(
                            |(result, timings)| {
                                sequence_timings = timings;
                                result
                            },
                        )
                    },
                ),
                "amiibo" => match prepare_amiibo_command(&arguments, control.lease_id()) {
                    Ok(AmiiboCommand::Unauthorized) => run_unauthorized_amiibo(),
                    Ok(AmiiboCommand::Authorized(authorization)) => run_with_device_admission(
                        "amiibo",
                        &arguments,
                        &mut control,
                        move |target, discovery, control| {
                            run_amiibo(authorization, target, discovery, control)
                        },
                    ),
                    Err(error) => Err(error),
                },
                _ => Err(format!("unknown command: {command}")),
            }
        }
    };
    reservation.record_event(
        JournalEventKind::OperationTerminal,
        execution_journal_payload(command, &execution),
    )?;
    reservation.record_event(
        JournalEventKind::CleanupTerminal,
        cleanup_journal_payload(command, &execution),
    )?;
    let (mut document, failure) = finalize_result(command, execution);
    apply_provenance_policy(&mut document, &provenance);
    let ended_unix_ns = current_unix_ns()?;
    document["run"] = reservation.run_identity_json(ended_unix_ns);
    document["auxiliary_artifacts"] = json!([]);
    if let Some(timings) = sequence_timings.take() {
        let bytes = u64::try_from(timings.len())
            .map_err(|_| "sequence timing CSV length does not fit u64".to_owned())?;
        let sha256 = sha256_bytes(&timings);
        reservation.stage_auxiliary(AuxiliaryKind::SequenceTimingsCsv, timings)?;
        document["auxiliary_artifacts"] = json!([{
            "kind": "sequence_timings_csv",
            "relative_path": SEQUENCE_TIMINGS_FILE_NAME,
            "bytes": bytes,
            "sha256": sha256,
        }]);
    }
    reservation.record_event(
        JournalEventKind::RunProjectionFinalized,
        json!({
            "ended_unix_ns": ended_unix_ns,
            "document": document.clone(),
        }),
    )?;
    reservation.seal_journal(json!({
        "primary_artifact": format!("{command}.json"),
    }))?;
    document["run"]["journal_projection"] = reservation.journal_projection()?;
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

fn execution_journal_payload(command: &str, execution: &Result<Value, String>) -> Value {
    match execution {
        Ok(result) => json!({
            "command": command,
            "runner_outcome": "returned",
            "result": result,
        }),
        Err(error) => json!({
            "command": command,
            "runner_outcome": "error",
            "error": error,
        }),
    }
}

fn cleanup_journal_payload(command: &str, execution: &Result<Value, String>) -> Value {
    match execution {
        Ok(result) => json!({
            "command": command,
            "cleanup_contract_succeeded": cleanup_contract_succeeded(command, result),
            "result": result,
        }),
        Err(error) => json!({
            "command": command,
            "cleanup_contract_succeeded": false,
            "runner_error": error,
        }),
    }
}

fn normalized_arguments(arguments: &[String]) -> Value {
    let mut normalized = Vec::with_capacity(arguments.len());
    let mut redact_next = false;
    for argument in arguments {
        if redact_next {
            normalized.push(json!("<redacted-path>"));
            redact_next = false;
            continue;
        }
        normalized.push(json!(argument));
        redact_next = matches!(
            argument.as_str(),
            "--output-dir" | "--data" | "--runs-root" | "--attestations-root"
        );
    }
    Value::Array(normalized)
}

fn current_unix_ns() -> Result<u64, String> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
            .as_nanos(),
    )
    .map_err(|_| "current Unix time does not fit u64 nanoseconds".to_owned())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QualificationStatus {
    Passed,
    Failed,
    Unverified,
    NotRun,
}

struct QualificationDecision {
    status: QualificationStatus,
    checks: Vec<Value>,
    failure: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunOutcome {
    Passed,
    QualificationFailed,
    ExecutionFailed,
    NotRun,
    Unverified,
    Cancelled,
}

impl RunOutcome {
    const fn execution_status(self) -> &'static str {
        match self {
            Self::Passed | Self::QualificationFailed | Self::NotRun | Self::Unverified => {
                "completed"
            }
            Self::ExecutionFailed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    const fn qualification_status(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::QualificationFailed | Self::ExecutionFailed => "failed",
            Self::NotRun => "not_run",
            Self::Unverified | Self::Cancelled => "unverified",
        }
    }

    const fn exit_code(self) -> i32 {
        match self {
            Self::Passed => 0,
            Self::QualificationFailed | Self::ExecutionFailed => 1,
            Self::NotRun | Self::Unverified => 2,
            Self::Cancelled => 130,
        }
    }

    fn from_document(document: &Value) -> Option<Self> {
        let outcome = match (
            document["execution_status"].as_str(),
            document["qualification_status"].as_str(),
        ) {
            (Some("completed"), Some("passed")) => Self::Passed,
            (Some("completed"), Some("failed")) => Self::QualificationFailed,
            (Some("failed"), Some("failed")) => Self::ExecutionFailed,
            (Some("completed"), Some("not_run")) => Self::NotRun,
            (Some("completed"), Some("unverified")) => Self::Unverified,
            (Some("cancelled"), Some("unverified")) => Self::Cancelled,
            _ => return None,
        };
        (document["exit_code"].as_i64() == Some(i64::from(outcome.exit_code()))).then_some(outcome)
    }
}

impl From<QualificationStatus> for RunOutcome {
    fn from(status: QualificationStatus) -> Self {
        match status {
            QualificationStatus::Passed => Self::Passed,
            QualificationStatus::Failed => Self::QualificationFailed,
            QualificationStatus::Unverified => Self::Unverified,
            QualificationStatus::NotRun => Self::NotRun,
        }
    }
}

fn apply_run_outcome(document: &mut Value, outcome: RunOutcome) {
    document["execution_status"] = json!(outcome.execution_status());
    document["qualification_status"] = json!(outcome.qualification_status());
    document["exit_code"] = json!(outcome.exit_code());
}

fn final_document(
    command: &str,
    outcome: RunOutcome,
    checks: Vec<Value>,
    error: Option<&str>,
    result: Option<Value>,
) -> Value {
    let mut document = json!({
        "schema_version": 2,
        "command": command,
        "execution_status": null,
        "qualification_status": null,
        "exit_code": null,
        "checks": checks,
    });
    apply_run_outcome(&mut document, outcome);
    if let Some(error) = error {
        document["error"] = json!(error);
    }
    if let Some(result) = result {
        document["result"] = result;
    }
    document
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
            let checks = vec![
                qualification_check("execution", "failed"),
                qualification_check("cleanup_evidence", cleanup_status),
            ];
            let document = final_document(
                command,
                RunOutcome::ExecutionFailed,
                checks,
                Some(&error),
                Some(result),
            );
            (document, Some(error))
        }
        Ok(result) if !cleanup_contract_succeeded(command, &result) => {
            let error = "deterministic cleanup did not complete".to_owned();
            let document = final_document(
                command,
                RunOutcome::ExecutionFailed,
                vec![qualification_check("cleanup", "failed")],
                Some(&error),
                Some(result),
            );
            (document, Some(error))
        }
        Ok(result) if result["operator_terminal_outcome"] == "interrupted" => {
            let document = final_document(
                command,
                RunOutcome::Cancelled,
                vec![
                    qualification_check("operator_interrupt", "cancelled"),
                    qualification_check("cleanup", "passed"),
                ],
                None,
                Some(result),
            );
            (document, None)
        }
        Ok(result) => {
            let decision = qualification_decision(command, &result);
            let document = final_document(
                command,
                decision.status.into(),
                decision.checks,
                decision.failure.as_deref(),
                Some(result),
            );
            (document, decision.failure)
        }
        Err(error) => {
            let document = final_document(
                command,
                RunOutcome::ExecutionFailed,
                Vec::new(),
                Some(&error),
                None,
            );
            (document, Some(error))
        }
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

fn hotplug_success_transitions_are_exact(result: &Value) -> bool {
    const EXPECTED: [&str; 9] = [
        "requested",
        "initial_admitted",
        "initial_connected",
        "awaiting_expected_absent",
        "initial_closed",
        "awaiting_expected_return",
        "rebound_admitted",
        "reconnected",
        "closed",
    ];
    result["hotplug_transitions"]
        .as_array()
        .is_some_and(|transitions| {
            transitions.len() == EXPECTED.len()
                && transitions.iter().zip(EXPECTED).enumerate().all(
                    |(index, (transition, expected))| {
                        transition["sequence"].as_u64() == u64::try_from(index + 1).ok()
                            && transition["state"].as_str() == Some(expected)
                    },
                )
        })
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
        "checkpoint" => {
            if checkpoint::projection_is_valid(&result["checkpoint"]) {
                QualificationDecision {
                    status: QualificationStatus::Unverified,
                    checks: vec![
                        qualification_check("checkpoint_projection", "passed"),
                        qualification_check("hardware_qualification", "unverified"),
                    ],
                    failure: None,
                }
            } else {
                QualificationDecision {
                    status: QualificationStatus::Failed,
                    checks: vec![qualification_check("checkpoint_projection", "failed")],
                    failure: Some("checkpoint projection is malformed".to_owned()),
                }
            }
        }
        "discover" => discovery_qualification(result),
        "handshake" => required_checks(&[
            (
                "operation_succeeded",
                result["operation"]["state"] == "Succeeded",
            ),
            ("actual_baud_recorded", result["actual_baud"].is_u64()),
            (
                "logical_report_telemetry",
                harness_report_telemetry_succeeded(&result["harness_evidence"]),
            ),
        ]),
        "smoke" | "home-wake" => operator_observation_qualification(command, result),
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
            (
                "fault_logical_report_telemetry",
                [
                    "occupier_report_telemetry",
                    "occupied_probe_report_telemetry",
                    "cancel_report_telemetry",
                    "deadline_report_telemetry",
                ]
                .into_iter()
                .all(|field| telemetry_projection_succeeded(&result[field])),
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
            (
                "identity_rebound_from_discovery",
                result["return_poll"]["outcome"] == "expected_present"
                    && result["rebound_admission"]["status"] == "rebound"
                    && result["rebound_admission"]["observed_expected"]["stable_id"].as_str()
                        == result["stable_id"].as_str(),
            ),
            (
                "hotplug_state_machine_closed",
                hotplug_success_transitions_are_exact(result),
            ),
            (
                "initial_logical_report_telemetry",
                harness_report_telemetry_succeeded(&result["initial_harness_evidence"]),
            ),
            (
                "reconnected_logical_report_telemetry",
                harness_report_telemetry_succeeded(&result["reconnected_harness_evidence"]),
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
                (
                    "logical_report_telemetry",
                    records.is_some_and(|records| {
                        records.iter().all(|record| {
                            harness_report_telemetry_succeeded(&record["harness_evidence"])
                        })
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
                ("telemetry_integrity", telemetry_integrity_succeeded(result)),
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
        "amiibo" => amiibo_qualification(result),
        _ => required_checks(&[("known_qualification_command", false)]),
    }
}

fn amiibo_qualification(result: &Value) -> QualificationDecision {
    let authorization = &result["authorization"];
    let limits = &result["declared_limits"];
    let payload = &result["payload"];
    let slot = result["slot"].as_u64();
    let slot_count = limits["slot_count"].as_u64();
    let maximum_data_len = limits["maximum_data_len"].as_u64();
    let actual_length = payload["actual_length"].as_u64();
    let expected_hash = payload["expected_sha256"].as_str();
    let recomputed_hash = payload["recomputed_sha256"].as_str();
    let expected_identity = authorization["expected_stable_id"].as_str();
    let observed_identity = authorization["observed_stable_id"].as_str();
    let authorization_valid = authorization["status"] == "authorized"
        && authorization["one_time"] == true
        && authorization["all_predicates_satisfied"] == true
        && authorization["lease_id"]
            .as_str()
            .is_some_and(|lease| !lease.is_empty())
        && slot.is_some()
        && authorization["slot"].as_u64() == slot
        && authorization["disposable_slot"].as_u64() == slot
        && expected_identity.is_some()
        && expected_identity == observed_identity
        && result["device_target"]["expected_stable_id"].as_str() == expected_identity
        && result["identity_admission"]["observed_expected"]["stable_id"].as_str()
            == observed_identity;
    let limits_valid = limits["classification"] == "declared_external"
        && limits["measured"] == false
        && slot
            .zip(slot_count)
            .is_some_and(|(slot, count)| count != 0 && count <= 256 && slot < count)
        && maximum_data_len.is_some_and(|length| (1..=16_384).contains(&length))
        && limits["source"]
            .as_str()
            .is_some_and(|source| validate_limits_source(source).is_ok());
    let hash_valid = expected_hash.is_some_and(valid_sha256)
        && expected_hash == recomputed_hash
        && payload["hash_matches"] == true
        && payload["raw_bytes_recorded"] == false
        && payload["source"]["kind"] == "local_file"
        && payload["source"]["path_recorded"] == false
        && actual_length
            .zip(maximum_data_len)
            .is_some_and(|(actual, maximum)| actual != 0 && actual <= maximum);
    let operation_evidence_valid = result["write_attempted"] == true
        && result["write_performed"] == true
        && result["select_performed"] == true
        && result["write_intent_durable"] == true
        && result["save_terminal_durable"] == true
        && result["select_intent_durable"] == true
        && result["select_terminal_durable"] == true
        && result["save"]["state"] == "Succeeded"
        && result["select"]["state"] == "Succeeded";
    let protocol_evidence_valid = actual_length
        .and_then(|length| usize::try_from(length).ok())
        .is_some_and(|length| {
            valid_amiibo_protocol_evidence(&result["amiibo_protocol_evidence"], length)
        });
    let boundary_valid =
        result["capability_inference"] == "none" && result["o_02_status"] == "open";

    let checks = [
        ("destructive_authorization", authorization_valid),
        ("declared_external_limits", limits_valid),
        ("payload_hash_and_length", hash_valid),
        ("save_select_operations", operation_evidence_valid),
        ("chunk_and_cleanup_journal", protocol_evidence_valid),
        (
            "logical_report_telemetry",
            harness_report_telemetry_succeeded(&result["harness_evidence"]),
        ),
        ("o_02_remains_open", boundary_valid),
    ];
    if checks.iter().any(|(_, passed)| !passed) {
        let mut decision = required_checks(&checks);
        decision.failure =
            Some("Amiibo qualification evidence is incomplete or contradictory".to_owned());
        return decision;
    }
    QualificationDecision {
        status: QualificationStatus::Unverified,
        checks: checks
            .into_iter()
            .map(|(name, _)| qualification_check(name, "passed"))
            .chain([qualification_check("hardware_capacity", "unverified")])
            .collect(),
        failure: None,
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
}

fn valid_amiibo_protocol_evidence(evidence: &Value, payload_len: usize) -> bool {
    let Some(chunks) = evidence["chunks"].as_array() else {
        return false;
    };
    if chunks.is_empty()
        || evidence["evidence_errors"]
            .as_array()
            .is_none_or(|errors| !errors.is_empty())
        || evidence["transport_closed_in_state"] != "complete"
        || evidence["selects"].as_array().is_none_or(|selects| {
            selects.len() != 1
                || selects[0]["accepted_bytes"] != 3
                || selects[0]["acknowledged"] != true
                || selects[0]["status"] != "acked"
                || !selects[0]["error"].is_null()
        })
        || evidence["cleanup_resets"].as_array().is_none_or(|resets| {
            resets.iter().any(|reset| {
                reset["accepted_bytes"] != 6
                    || reset["acknowledged"] != true
                    || reset["status"] != "acked"
                    || !reset["error"].is_null()
            })
        })
    {
        return false;
    }

    let mut next_offset = 0_usize;
    let mut next_attempt = 1_u64;
    for chunk in chunks {
        let Some(offset) = chunk["offset"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
        else {
            return false;
        };
        let Some(length) = chunk["length"]
            .as_u64()
            .and_then(|value| usize::try_from(value).ok())
        else {
            return false;
        };
        if offset != next_offset
            || length != (payload_len - next_offset).min(20)
            || chunk["attempt"].as_u64() != Some(next_attempt)
        {
            return false;
        }
        match chunk["status"].as_str() {
            Some("failed")
                if chunk["error"].is_object()
                    && chunk["header_accepted_bytes"]
                        .as_u64()
                        .is_some_and(|accepted| accepted <= 7)
                    && chunk["payload_accepted_bytes"]
                        .as_u64()
                        .is_some_and(|accepted| accepted <= u64::try_from(length).unwrap_or(0))
                    && chunk["payload_acknowledged"] == false
                    && (chunk["header_acknowledged"] == true
                        || chunk["payload_accepted_bytes"] == 0) =>
            {
                next_attempt = next_attempt.saturating_add(1);
            }
            Some("acked")
                if chunk["header_accepted_bytes"] == 7
                    && chunk["header_acknowledged"] == true
                    && chunk["payload_accepted_bytes"].as_u64() == u64::try_from(length).ok()
                    && chunk["payload_acknowledged"] == true
                    && chunk["error"].is_null() =>
            {
                next_offset += length;
                next_attempt = 1;
            }
            _ => return false,
        }
    }
    next_offset == payload_len
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

fn operator_observation_qualification(command: &str, result: &Value) -> QualificationDecision {
    if !telemetry_integrity_succeeded(result) {
        return QualificationDecision {
            status: QualificationStatus::Failed,
            checks: vec![qualification_check("telemetry_integrity", "failed")],
            failure: Some("logical report telemetry is incomplete or contradictory".to_owned()),
        };
    }
    if result["final_snapshot"]["desired_report_neutral"].as_bool() != Some(true) {
        return required_checks(&[("final_report_neutral", false)]);
    }
    let Some(observations) = result["operator_observations"].as_array() else {
        return QualificationDecision {
            status: QualificationStatus::Unverified,
            checks: vec![
                qualification_check("final_report_neutral", "passed"),
                qualification_check("operator_observations", "unverified"),
            ],
            failure: None,
        };
    };
    let Some(expected_action_ids) = expected_operator_action_ids(command, result) else {
        return failed_operator_observation_decision("operator action plan is invalid");
    };
    if result["required_operator_action_ids"] != json!(expected_action_ids) {
        return failed_operator_observation_decision(
            "required operator action IDs do not match the command plan",
        );
    }
    if observations.len() > expected_action_ids.len() {
        return failed_operator_observation_decision(
            "operator observation count exceeds the command plan",
        );
    }
    let expected_stable_id = result["device_target"]["expected_stable_id"].as_str();
    let observed_stable_id =
        result["identity_admission"]["observed_expected"]["stable_id"].as_str();
    let mut lease_id = None;
    for (index, observation) in observations.iter().enumerate() {
        let current_lease = observation["lease_id"].as_str();
        if observation["sequence"].as_u64() != u64::try_from(index + 1).ok()
            || observation["command"].as_str() != Some(command)
            || observation["action_id"].as_str()
                != expected_action_ids.get(index).map(String::as_str)
            || expected_stable_id.is_none()
            || observation["expected_stable_id"].as_str() != expected_stable_id
            || observed_stable_id.is_none()
            || observation["observed_stable_id"].as_str() != observed_stable_id
            || current_lease.is_none_or(str::is_empty)
            || lease_id.is_some_and(|expected| current_lease != Some(expected))
            || observation["neutral_snapshot"]["desired_report_neutral"] != true
            || observation["active_operation"]["state"] != "Succeeded"
            || observation["release_operation"]["state"] != "Succeeded"
            || observation["neutral_operation"]["state"] != "Succeeded"
        {
            return failed_operator_observation_decision(
                "operator observation binding or operation evidence is invalid",
            );
        }
        lease_id = current_lease;
        if !matches!(
            observation["outcome"].as_str(),
            Some("yes" | "no" | "eof" | "timeout" | "ambiguous" | "interrupted")
        ) {
            return failed_operator_observation_decision("operator observation outcome is invalid");
        }
    }

    let terminal = result["operator_terminal_outcome"].as_str();
    let last_outcome = observations
        .last()
        .and_then(|observation| observation["outcome"].as_str());
    if terminal == Some("no") && last_outcome == Some("no") {
        return failed_operator_observation_decision("operator reported no");
    }
    if matches!(terminal, Some("eof" | "timeout" | "ambiguous")) && terminal == last_outcome {
        return QualificationDecision {
            status: QualificationStatus::Unverified,
            checks: vec![
                qualification_check("final_report_neutral", "passed"),
                qualification_check("operator_observations", "unverified"),
            ],
            failure: None,
        };
    }
    if terminal == Some("all_yes")
        && observations.len() == expected_action_ids.len()
        && observations
            .iter()
            .all(|observation| observation["outcome"] == "yes")
    {
        return QualificationDecision {
            status: QualificationStatus::Passed,
            checks: vec![
                qualification_check("final_report_neutral", "passed"),
                qualification_check("operator_observations", "passed"),
            ],
            failure: None,
        };
    }
    if observations.is_empty() || observations.len() < expected_action_ids.len() {
        return QualificationDecision {
            status: QualificationStatus::Unverified,
            checks: vec![
                qualification_check("final_report_neutral", "passed"),
                qualification_check("operator_observations", "unverified"),
            ],
            failure: None,
        };
    }
    failed_operator_observation_decision("operator observation terminal is inconsistent")
}

fn telemetry_integrity_succeeded(result: &Value) -> bool {
    telemetry_projection_succeeded(&result["latency"])
}

fn harness_report_telemetry_succeeded(evidence: &Value) -> bool {
    telemetry_projection_succeeded(&evidence["logical_report_telemetry"])
}

fn telemetry_projection_succeeded(projection: &Value) -> bool {
    let Some(object) = projection.as_object() else {
        return false;
    };
    for field in [
        "integrity",
        "logical_reports",
        "sample_count",
        "direct_sample_count",
        "command_admitted_to_write_entered_ns",
        "dispatch_to_write_entered_ns",
        "write_entered_to_os_acceptance_ns",
        "uart_complete_frame",
        "usb_hid",
        "switch_physical_order",
    ] {
        if !object.contains_key(field) {
            return false;
        }
    }
    if projection["integrity"]["status"] != "passed"
        || projection["integrity"]["errors"]
            .as_array()
            .is_none_or(|errors| !errors.is_empty())
    {
        return false;
    }
    let reports = &projection["logical_reports"];
    let Some(attempt_count) = reports["attempt_count"].as_u64() else {
        return false;
    };
    let Some(accepted_count) = reports["accepted_count"].as_u64() else {
        return false;
    };
    let Some(failed_count) = reports["failed_count"].as_u64() else {
        return false;
    };
    let Some(pending_count) = reports["pending_count"].as_u64() else {
        return false;
    };
    let Some(contradiction_count) = reports["contradiction_count"].as_u64() else {
        return false;
    };
    if pending_count != 0
        || contradiction_count != 0
        || accepted_count
            .checked_add(failed_count)
            .and_then(|count| count.checked_add(pending_count))
            .and_then(|count| count.checked_add(contradiction_count))
            != Some(attempt_count)
        || projection["sample_count"].as_u64() != Some(accepted_count)
        || projection["direct_sample_count"]
            .as_u64()
            .is_none_or(|count| count > accepted_count)
    {
        return false;
    }
    let detail_valid = match reports["detail"]["kind"].as_str() {
        Some("inline") => {
            reports["detail"]["rows"]
                .as_array()
                .and_then(|rows| u64::try_from(rows.len()).ok())
                == Some(attempt_count)
        }
        Some("csv") => reports["detail"]["relative_path"] == SEQUENCE_TIMINGS_FILE_NAME,
        Some(_) | None => false,
    };
    let uart_valid = projection["uart_complete_frame"].is_null()
        || (projection["uart_complete_frame"]["classification"] == "theoretical"
            && projection["uart_complete_frame"]["measured"] == false
            && projection["uart_complete_frame"]["theoretical_ns"].is_u64());
    detail_valid
        && uart_valid
        && projection["usb_hid"]["qualification_status"] == "unverified"
        && projection["usb_hid"]["measured"] == false
        && projection["switch_physical_order"]["qualification_status"] == "unverified"
        && projection["switch_physical_order"]["measured"] == false
}

fn expected_operator_action_ids(command: &str, result: &Value) -> Option<Vec<String>> {
    match command {
        "smoke" => match result["stage"].as_str() {
            Some("a_only") => Some(
                smoke_observation_specs(false)
                    .into_iter()
                    .map(|spec| spec.action_id)
                    .collect(),
            ),
            Some("full") => Some(
                smoke_observation_specs(true)
                    .into_iter()
                    .map(|spec| spec.action_id)
                    .collect(),
            ),
            _ => None,
        },
        "home-wake" => {
            let attempts = usize::try_from(result["attempts"].as_u64()?).ok()?;
            (1..=100).contains(&attempts).then(|| {
                (1..=attempts)
                    .map(|attempt| format!("home-wake.attempt.{attempt}"))
                    .collect()
            })
        }
        _ => None,
    }
}

fn failed_operator_observation_decision(message: &str) -> QualificationDecision {
    QualificationDecision {
        status: QualificationStatus::Failed,
        checks: vec![
            qualification_check("final_report_neutral", "passed"),
            qualification_check("operator_observations", "failed"),
        ],
        failure: Some(message.to_owned()),
    }
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
    if result["operator_terminal_outcome"] == "interrupted" {
        return interrupted_cleanup_contract_succeeded(command, result);
    }
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
            "checkpoint" => {
                result["resources"]
                    .as_object()
                    .is_some_and(|resources| resources.is_empty())
                    && cleanup_slot_count(result) == 0
                    && runtime_cleanup_count(result) == 0
            }
            "faults" => partial_fault_cleanup_contract_succeeded(result),
            "hotplug" => partial_hotplug_cleanup_contract_succeeded(result),
            "lifecycle" => partial_lifecycle_cleanup_contract_succeeded(result),
            "handshake" | "smoke" | "home-wake" | "sequence" | "amiibo" => {
                partial_single_harness_cleanup_contract_succeeded(result)
            }
            _ => false,
        };
    }
    if command == "hotplug" {
        return completed_hotplug_cleanup_contract_succeeded(result);
    }

    let expected_count = match command {
        "handshake" | "smoke" | "home-wake" | "sequence" => 1,
        "faults" => 4,
        "hotplug" => 2,
        "lifecycle" => result["records"].as_array().map_or(0, Vec::len),
        "amiibo" if result["write_performed"].as_bool() == Some(true) => 1,
        "checkpoint" | "discover" | "amiibo" => 0,
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
        "checkpoint" | "discover" | "amiibo" => true,
        _ => true,
    }
}

fn interrupted_cleanup_contract_succeeded(command: &str, result: &Value) -> bool {
    let Some(resources) = result.get("resources").and_then(Value::as_object) else {
        return false;
    };
    if resources.is_empty() {
        return cleanup_slot_count(result) == 0 && runtime_cleanup_count(result) == 0;
    }
    match command {
        "handshake" | "smoke" | "home-wake" | "sequence" | "amiibo" => {
            partial_single_harness_cleanup_contract_succeeded(result)
        }
        "hotplug" => interrupted_hotplug_cleanup_succeeded(result),
        "lifecycle" => interrupted_lifecycle_cleanup_succeeded(result),
        "faults" => interrupted_fault_cleanup_succeeded(result),
        _ => false,
    }
}

fn interrupted_hotplug_cleanup_succeeded(result: &Value) -> bool {
    let Some(resources) = result["resources"].as_object() else {
        return false;
    };
    let created = |role: &str| {
        resources
            .get(role)
            .and_then(Value::as_object)
            .and_then(|resource| resource.get("created"))
            .and_then(Value::as_bool)
    };
    if resources.len() != 2 || created("initial") != Some(true) {
        return false;
    }
    let Some(reconnected) = created("reconnected") else {
        return false;
    };
    let expected = 1 + usize::from(reconnected);
    cleanup_slot_count(result) == expected
        && runtime_cleanup_count(result) == expected
        && cleanup_succeeded(&result["disconnect_cleanup"])
        && (!reconnected || cleanup_succeeded(&result["cleanup"]))
}

fn interrupted_lifecycle_cleanup_succeeded(result: &Value) -> bool {
    let Some(records) = result["records"].as_array() else {
        return false;
    };
    let created_cycles = result["resources"]["created_cycles"]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok());
    created_cycles == Some(records.len())
        && records.iter().enumerate().all(|(index, record)| {
            record["cycle"].as_u64() == u64::try_from(index + 1).ok()
                && matches!(record["status"].as_str(), Some("completed" | "cancelled"))
                && primary_resource_was_created(record)
                && cleanup_succeeded(&record["cleanup"])
        })
        && cleanup_slot_count(result) == records.len()
        && runtime_cleanup_count(result) == records.len()
}

fn interrupted_fault_cleanup_succeeded(result: &Value) -> bool {
    let Some(resources) = result["resources"].as_object() else {
        return false;
    };
    if resources.len() != FAULT_CLEANUP_ROLES.len() {
        return false;
    }
    let mut expected = 0;
    let mut uncreated_seen = false;
    for (role, cleanup) in FAULT_CLEANUP_ROLES {
        let Some(created) = resources[role]["created"].as_bool() else {
            return false;
        };
        if created && uncreated_seen {
            return false;
        }
        uncreated_seen |= !created;
        if created {
            expected += 1;
            if !cleanup_succeeded(&result[cleanup]) {
                return false;
            }
        } else if result.get(cleanup).is_some() {
            return false;
        }
    }
    cleanup_slot_count(result) == expected && runtime_cleanup_count(result) == expected
}

fn completed_hotplug_cleanup_contract_succeeded(result: &Value) -> bool {
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
    if created("initial") != Some(true) {
        return false;
    }
    let Some(reconnected_created) = created("reconnected") else {
        return false;
    };
    if !cleanup_succeeded(&result["disconnect_cleanup"]) {
        return false;
    }

    if reconnected_created {
        result.get("hotplug_machine_failure").is_none()
            && result.get("cleanup").is_some()
            && cleanup_slot_count(result) == 2
            && runtime_cleanup_count(result) == 2
            && cleanup_succeeded(&result["cleanup"])
    } else {
        matches!(
            result["hotplug_machine_failure"]["kind"].as_str(),
            Some("expected_absent_timeout" | "expected_return_timeout")
        ) && result.get("cleanup").is_none()
            && cleanup_slot_count(result) == 1
            && runtime_cleanup_count(result) == 1
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
    RunOutcome::from_document(document).map_or(1, RunOutcome::exit_code)
}

fn apply_provenance_policy(document: &mut Value, provenance: &RuntimeProvenance) {
    let trusted = provenance.trusted();
    document["provenance"] = provenance.to_json();
    if let Some(checks) = document.get_mut("checks").and_then(Value::as_array_mut) {
        checks.push(qualification_check(
            "build_and_executable_provenance",
            if trusted { "passed" } else { "unverified" },
        ));
    }
    if document["qualification_status"] == "passed" && !trusted {
        apply_run_outcome(document, RunOutcome::Unverified);
    }
}

fn run_discover(arguments: &[String], control: &mut RunControl<'_>) -> Result<Value, String> {
    let samples = value_or(arguments, "--samples", 3_usize)?;
    if samples == 0 {
        return Err("--samples must be non-zero".to_owned());
    }
    let mut snapshots = Vec::with_capacity(samples);
    for index in 0..samples {
        if let Err(failure) = control.checkpoint("discover_interrupt") {
            return Ok(pre_harness_cancellation_result("discover", failure));
        }
        let ports = discover_system_ports().map_err(|error| error.to_string())?;
        snapshots.push(Value::Array(ports.iter().map(port_json).collect()));
        if index + 1 != samples
            && let Err(failure) = wait_controlled(
                control,
                Duration::from_millis(250),
                &ThreadDelay,
                "discover_interrupt",
            )
        {
            return Ok(pre_harness_cancellation_result("discover", failure));
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

fn run_handshake(
    _arguments: &[String],
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    let harness = Harness::new(&target, discovery, ControllerOptions::default())?;
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
            result["operation"] = connect_for_command_controlled(
                harness,
                control,
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

fn run_smoke(
    arguments: &[String],
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    let full = arguments.iter().any(|argument| argument == "--full");
    let wake_left_stick = arguments
        .iter()
        .any(|argument| argument == "--wake-left-stick");
    let wake_home = arguments.iter().any(|argument| argument == "--wake-home");
    let post_home_neutral_delay_ms = smoke_home_delay(arguments, wake_home)?;
    let hold_ms = value_or(arguments, "--hold-ms", 0_u64)?;
    validate_hold_ms(hold_ms)?;
    let (observation_window_ms, operator_timeout) =
        operator_timing(arguments, (hold_ms != 0).then_some(hold_ms))?;
    let expected_stable_id = target.request().expected_stable_id().to_owned();
    let observed_stable_id = target.descriptor().stable_id().to_owned();
    let action_specs = smoke_observation_specs(full);
    let required_action_ids = action_specs
        .iter()
        .map(|spec| spec.action_id.clone())
        .collect::<Vec<_>>();
    let harness = Harness::new(&target, discovery, ControllerOptions::default())?;
    let telemetry = harness.telemetry.clone();
    let result = json!({
        "command": "smoke",
        "stage": if full { "full" } else { "a_only" },
        "wake_left_stick": wake_left_stick,
        "wake_home": wake_home,
        "configured_post_home_neutral_delay_ms": post_home_neutral_delay_ms,
        "diagnostic_prelude_steps": [],
        "a_hold_ms": hold_ms,
        "observation_window_ms": observation_window_ms,
        "operator_timeout_ms": operator_timeout.as_millis(),
        "required_operator_action_ids": required_action_ids,
        "operator_observations": [],
        "operator_terminal_outcome": null,
        "port": port_json(&harness.descriptor),
        "actions": [],
        "resources": {"primary": {"created": true}},
    });
    let mut result = finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "smoke_close",
        |harness, result| {
            result["connect_operation"] = connect_for_command_controlled(
                harness,
                control,
                ConnectOptions::default(),
                "smoke_connect_admit",
                "smoke_connect_wait",
                "smoke_connect_terminal",
            )?;
            if wake_home {
                for (label, action) in home_wake_actions() {
                    exercise_smoke_action_controlled(
                        harness, control, label, action, result, true,
                    )?;
                }
                record_configured_home_delay_controlled(
                    result,
                    control,
                    post_home_neutral_delay_ms.expect("wake-home requires a configured delay"),
                    &ThreadDelay,
                )?;
            }
            if wake_left_stick {
                for (label, action) in left_stick_wake_actions() {
                    exercise_smoke_action_controlled(
                        harness, control, label, action, result, wake_home,
                    )?;
                }
            }
            for spec in action_specs {
                let outcome = exercise_observed_action_unit(
                    harness,
                    control,
                    "smoke",
                    &expected_stable_id,
                    &observed_stable_id,
                    &spec,
                    observation_window_ms,
                    operator_timeout,
                    result,
                    &ThreadDelay,
                )?;
                if outcome != OperatorOutcome::Yes {
                    result["operator_terminal_outcome"] = json!(outcome.as_str());
                    break;
                }
            }
            if result["operator_terminal_outcome"].is_null() {
                result["operator_terminal_outcome"] = json!("all_yes");
            }
            result["actual_baud"] = json!(harness.actual_baud());
            result["final_snapshot"] = snapshot_json(&harness.controller);
            Ok(())
        },
    );
    result["latency"] = latency_json(&telemetry, telemetry_actual_baud(&telemetry), None);
    Ok(result)
}

fn run_home_wake(
    arguments: &[String],
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    let attempts = value_or(arguments, "--attempts", 20_usize)?;
    let interval_seconds = value_or(arguments, "--interval-seconds", 3_u64)?;
    validate_home_wake(attempts, interval_seconds)?;
    let (observation_window_ms, operator_timeout) = operator_timing(arguments, None)?;
    let expected_stable_id = target.request().expected_stable_id().to_owned();
    let observed_stable_id = target.descriptor().stable_id().to_owned();
    let required_action_ids = (1..=attempts)
        .map(|attempt| format!("home-wake.attempt.{attempt}"))
        .collect::<Vec<_>>();
    let harness = Harness::new(&target, discovery, ControllerOptions::default())?;
    let telemetry = harness.telemetry.clone();
    let result = json!({
        "command": "home-wake",
        "attempts": attempts,
        "interval_seconds": interval_seconds,
        "port": port_json(&harness.descriptor),
        "records": [],
        "observation_window_ms": observation_window_ms,
        "operator_timeout_ms": operator_timeout.as_millis(),
        "required_operator_action_ids": required_action_ids,
        "operator_observations": [],
        "operator_terminal_outcome": null,
        "resources": {"primary": {"created": true}},
    });
    let mut result = finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "home_wake_close",
        |harness, result| {
            result["connect_operation"] = connect_for_command_controlled(
                harness,
                control,
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
                    wait_controlled(
                        control,
                        remaining,
                        &ThreadDelay,
                        "home_wake_interval_interrupt",
                    )?;
                }
                let attempt_started_ms =
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                let action_id = format!("home-wake.attempt.{}", attempt + 1);
                let spec = ObservedActionSpec {
                    label: action_id.clone(),
                    active_label: "button.Home.down".to_owned(),
                    active: ControllerAction::ButtonDown(Button::Home),
                    release_label: "button.Home.up".to_owned(),
                    release: ControllerAction::ButtonUp(Button::Home),
                    action_id,
                };
                let mut unit = json!({"actions": [], "operator_observations": []});
                let outcome = exercise_observed_action_unit(
                    harness,
                    control,
                    "home-wake",
                    &expected_stable_id,
                    &observed_stable_id,
                    &spec,
                    observation_window_ms,
                    operator_timeout,
                    &mut unit,
                    &ThreadDelay,
                )?;
                let observation = unit["operator_observations"][0].clone();
                result["operator_observations"]
                    .as_array_mut()
                    .expect("operator observations are an array")
                    .push(observation.clone());
                result["records"]
                    .as_array_mut()
                    .expect("home-wake records are an array")
                    .push(json!({
                        "attempt": attempt + 1,
                        "started_after_ms": attempt_started_ms,
                        "status": "completed",
                        "actions": unit["actions"],
                        "operator_observation": observation,
                    }));
                if outcome != OperatorOutcome::Yes {
                    result["operator_terminal_outcome"] = json!(outcome.as_str());
                    break;
                }
            }
            if result["operator_terminal_outcome"].is_null() {
                result["operator_terminal_outcome"] = json!("all_yes");
            }
            result["actual_baud"] = json!(harness.actual_baud());
            result["final_snapshot"] = snapshot_json(&harness.controller);
            Ok(())
        },
    );
    result["latency"] = latency_json(&telemetry, telemetry_actual_baud(&telemetry), None);
    Ok(result)
}

fn run_faults(
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    if let Err(failure) = control.checkpoint("faults_interrupt") {
        return Ok(pre_harness_cancellation_result("faults", failure));
    }
    Ok(faults::run(target, discovery, control))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HotplugExpectedState {
    Absent,
    Present,
}

trait HotplugPollTimer {
    fn elapsed(&self) -> Duration;
    fn wait(&mut self, duration: Duration);
}

struct SystemHotplugPollTimer {
    started: Instant,
}

impl SystemHotplugPollTimer {
    fn new() -> Self {
        Self {
            started: Instant::now(),
        }
    }
}

impl HotplugPollTimer for SystemHotplugPollTimer {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    fn wait(&mut self, duration: Duration) {
        thread::sleep(duration);
    }
}

enum HotplugPollOutcome {
    ExpectedAbsent,
    Rebound(Box<AdmittedDevice>),
    TimedOut,
    DiscoveryError(SerialError),
    AmbiguousSnapshot,
}

struct HotplugPollTrace {
    observations: Vec<AdmissionEvidence>,
    outcome: HotplugPollOutcome,
}

#[cfg(test)]
fn poll_hotplug_identity(
    discovery: &dyn DeviceDiscovery,
    request: &DeviceTargetRequest,
    expected_state: HotplugExpectedState,
    timeout: Duration,
    timer: &mut dyn HotplugPollTimer,
) -> HotplugPollTrace {
    let mut observations = Vec::new();
    loop {
        if timer.elapsed() >= timeout {
            return HotplugPollTrace {
                observations,
                outcome: HotplugPollOutcome::TimedOut,
            };
        }
        let snapshot = match discovery.discover() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return HotplugPollTrace {
                    observations,
                    outcome: HotplugPollOutcome::DiscoveryError(error),
                };
            }
        };
        match rebind_device(request, snapshot) {
            RebindDecision::Rebound(target) => {
                observations.push(target.evidence().clone());
                if expected_state == HotplugExpectedState::Present {
                    return HotplugPollTrace {
                        observations,
                        outcome: HotplugPollOutcome::Rebound(Box::new(target)),
                    };
                }
            }
            RebindDecision::ExpectedAbsent(evidence) => {
                observations.push(evidence);
                if expected_state == HotplugExpectedState::Absent {
                    return HotplugPollTrace {
                        observations,
                        outcome: HotplugPollOutcome::ExpectedAbsent,
                    };
                }
            }
            RebindDecision::Ambiguous(evidence) => {
                observations.push(evidence);
                return HotplugPollTrace {
                    observations,
                    outcome: HotplugPollOutcome::AmbiguousSnapshot,
                };
            }
        }

        let elapsed = timer.elapsed();
        if elapsed >= timeout {
            return HotplugPollTrace {
                observations,
                outcome: HotplugPollOutcome::TimedOut,
            };
        }
        timer.wait(HOTPLUG_POLL_INTERVAL.min(timeout - elapsed));
    }
}

fn poll_hotplug_identity_controlled(
    discovery: &dyn DeviceDiscovery,
    request: &DeviceTargetRequest,
    expected_state: HotplugExpectedState,
    timeout: Duration,
    timer: &mut dyn HotplugPollTimer,
    control: &mut RunControl<'_>,
) -> Result<HotplugPollTrace, CommandFailure> {
    let mut observations = Vec::new();
    loop {
        control.checkpoint("hotplug_poll_interrupt")?;
        if timer.elapsed() >= timeout {
            return Ok(HotplugPollTrace {
                observations,
                outcome: HotplugPollOutcome::TimedOut,
            });
        }
        let snapshot = match discovery.discover() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Ok(HotplugPollTrace {
                    observations,
                    outcome: HotplugPollOutcome::DiscoveryError(error),
                });
            }
        };
        match rebind_device(request, snapshot) {
            RebindDecision::Rebound(target) => {
                observations.push(target.evidence().clone());
                if expected_state == HotplugExpectedState::Present {
                    return Ok(HotplugPollTrace {
                        observations,
                        outcome: HotplugPollOutcome::Rebound(Box::new(target)),
                    });
                }
            }
            RebindDecision::ExpectedAbsent(evidence) => {
                observations.push(evidence);
                if expected_state == HotplugExpectedState::Absent {
                    return Ok(HotplugPollTrace {
                        observations,
                        outcome: HotplugPollOutcome::ExpectedAbsent,
                    });
                }
            }
            RebindDecision::Ambiguous(evidence) => {
                observations.push(evidence);
                return Ok(HotplugPollTrace {
                    observations,
                    outcome: HotplugPollOutcome::AmbiguousSnapshot,
                });
            }
        }

        let elapsed = timer.elapsed();
        if elapsed >= timeout {
            return Ok(HotplugPollTrace {
                observations,
                outcome: HotplugPollOutcome::TimedOut,
            });
        }
        let mut remaining = HOTPLUG_POLL_INTERVAL.min(timeout - elapsed);
        while !remaining.is_zero() {
            control.checkpoint("hotplug_poll_interrupt")?;
            let current = remaining.min(INTERRUPT_POLL_SLICE);
            timer.wait(current);
            remaining = remaining.saturating_sub(current);
        }
    }
}

fn hotplug_poll_trace_json(trace: &HotplugPollTrace) -> Value {
    let (outcome, structured_error) = match &trace.outcome {
        HotplugPollOutcome::ExpectedAbsent => ("expected_absent", Value::Null),
        HotplugPollOutcome::Rebound(_) => ("expected_present", Value::Null),
        HotplugPollOutcome::TimedOut => ("timed_out", Value::Null),
        HotplugPollOutcome::DiscoveryError(error) => ("discovery_error", serial_error_json(error)),
        HotplugPollOutcome::AmbiguousSnapshot => ("ambiguous_snapshot", Value::Null),
    };
    json!({
        "outcome": outcome,
        "observations": trace.observations.iter().map(|evidence| {
            let status = match evidence.reason().map(|reason| reason.as_str()) {
                None => "expected_present",
                Some("expected_absent") => "expected_absent",
                Some("ambiguous_snapshot") => "ambiguous_snapshot",
                Some(_) => "conflict",
            };
            admission_evidence_json(evidence, status)
        }).collect::<Vec<_>>(),
        "structured_error": structured_error,
    })
}

fn push_hotplug_transition(result: &mut Value, state: &str) {
    let transitions = result["hotplug_transitions"]
        .as_array_mut()
        .expect("hotplug transitions are an array");
    transitions.push(json!({
        "sequence": transitions.len() + 1,
        "state": state,
    }));
}

fn set_hotplug_machine_failure(result: &mut Value, kind: &str) {
    result["hotplug_machine_failure"] = json!({"kind": kind});
}

fn run_hotplug(
    arguments: &[String],
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    let timeout_seconds = value_or(arguments, "--timeout-seconds", 180_u64)?;
    let timeout = Duration::from_secs(timeout_seconds);
    let request = target.request().clone();
    let port = target.descriptor().port_name().to_owned();
    let harness = Harness::new(&target, discovery.clone(), ControllerOptions::default())?;
    let stable_id = harness.descriptor.stable_id().to_owned();
    let mut result = json!({
        "command": "hotplug",
        "stable_id": stable_id,
        "initial_port": port_json(&harness.descriptor),
        "hotplug_transitions": [],
        "resources": {
            "initial": {"created": true},
            "reconnected": {"created": false},
        },
    });
    push_hotplug_transition(&mut result, "requested");
    push_hotplug_transition(&mut result, "initial_admitted");
    let mut result = finish_harness_phase(
        harness,
        result,
        "disconnect_cleanup",
        "initial_harness_evidence",
        "hotplug_initial_close",
        |harness, result| {
            result["initial_connect_operation"] = connect_for_command_controlled(
                harness,
                control,
                ConnectOptions::default(),
                "hotplug_initial_connect_admit",
                "hotplug_initial_connect_wait",
                "hotplug_initial_connect_terminal",
            )?;
            push_hotplug_transition(result, "initial_connected");
            println!("HOTPLUG_READY: unplug {port} now");
            std::io::stdout().flush().map_err(|error| {
                CommandFailure::new("hotplug_unplug_marker_flush", error.to_string())
            })?;
            push_hotplug_transition(result, "awaiting_expected_absent");
            let mut timer = SystemHotplugPollTimer::new();
            let trace = poll_hotplug_identity_controlled(
                discovery.as_ref(),
                &request,
                HotplugExpectedState::Absent,
                timeout,
                &mut timer,
                control,
            )?;
            result["absence_poll"] = hotplug_poll_trace_json(&trace);
            match trace.outcome {
                HotplugPollOutcome::ExpectedAbsent => {}
                HotplugPollOutcome::TimedOut => {
                    result["disconnect_detected"] = json!(false);
                    set_hotplug_machine_failure(result, "expected_absent_timeout");
                    return Ok(());
                }
                HotplugPollOutcome::DiscoveryError(error) => {
                    return Err(CommandFailure::new("hotplug_wait_absent", error.message()));
                }
                HotplugPollOutcome::AmbiguousSnapshot => {
                    return Err(CommandFailure::new(
                        "hotplug_wait_absent",
                        "ambiguous serial snapshot while waiting for expected identity absence",
                    ));
                }
                HotplugPollOutcome::Rebound(_) => {
                    unreachable!("absence poll cannot return a present target")
                }
            }
            control.checkpoint("hotplug_disconnect_admit")?;
            let disconnected = harness.controller.reset().map_err(|error| {
                CommandFailure::new("hotplug_disconnect_admit", error.to_string())
            })?;
            let (disconnected, state) = wait_for_command_terminal_controlled(
                disconnected,
                control,
                OPERATION_TIMEOUT,
                "hotplug_disconnect_wait",
            )?;
            result["disconnect_operation"] = operation_json(&disconnected);
            result["disconnect_detected"] = json!(state == OperationState::Failed);
            Ok(())
        },
    );
    if cleanup_succeeded(&result["disconnect_cleanup"]) {
        push_hotplug_transition(&mut result, "initial_closed");
    }
    if result["operator_terminal_outcome"] == "interrupted" {
        push_hotplug_transition(&mut result, "cancelled");
        return Ok(result);
    }
    if result.get("execution_error").is_some() {
        push_hotplug_transition(&mut result, "failed");
        return Ok(result);
    }
    if result.get("hotplug_machine_failure").is_some() {
        push_hotplug_transition(&mut result, "failed");
        return Ok(result);
    }

    println!("HOTPLUG_DISCONNECTED: reconnect the same device now");
    if let Err(error) = std::io::stdout().flush() {
        set_command_failure(
            &mut result,
            CommandFailure::new("hotplug_reconnect_marker_flush", error.to_string()),
        );
        push_hotplug_transition(&mut result, "failed");
        return Ok(result);
    }
    push_hotplug_transition(&mut result, "awaiting_expected_return");
    let mut timer = SystemHotplugPollTimer::new();
    let trace = match poll_hotplug_identity_controlled(
        discovery.as_ref(),
        &request,
        HotplugExpectedState::Present,
        timeout,
        &mut timer,
        control,
    ) {
        Ok(trace) => trace,
        Err(failure) => {
            set_command_failure(&mut result, failure);
            push_hotplug_transition(&mut result, "cancelled");
            return Ok(result);
        }
    };
    result["return_poll"] = hotplug_poll_trace_json(&trace);
    let rebound = match trace.outcome {
        HotplugPollOutcome::Rebound(target) => target,
        HotplugPollOutcome::TimedOut => {
            set_hotplug_machine_failure(&mut result, "expected_return_timeout");
            push_hotplug_transition(&mut result, "failed");
            return Ok(result);
        }
        HotplugPollOutcome::DiscoveryError(error) => {
            set_command_failure(
                &mut result,
                CommandFailure::new("hotplug_wait_return", error.message()),
            );
            push_hotplug_transition(&mut result, "failed");
            return Ok(result);
        }
        HotplugPollOutcome::AmbiguousSnapshot => {
            set_command_failure(
                &mut result,
                CommandFailure::new(
                    "hotplug_wait_return",
                    "ambiguous serial snapshot while waiting for expected identity return",
                ),
            );
            push_hotplug_transition(&mut result, "failed");
            return Ok(result);
        }
        HotplugPollOutcome::ExpectedAbsent => {
            unreachable!("return poll cannot complete with an absent target")
        }
    };
    result["rebound_admission"] = admission_evidence_json(rebound.evidence(), "rebound");
    result["reconnected_identity"] = port_json(rebound.descriptor());
    push_hotplug_transition(&mut result, "rebound_admitted");
    if let Err(failure) = control.checkpoint("hotplug_reconnect_interrupt") {
        set_command_failure(&mut result, failure);
        push_hotplug_transition(&mut result, "cancelled");
        return Ok(result);
    }
    let reconnected = match Harness::new(&rebound, discovery, ControllerOptions::default()) {
        Ok(harness) => harness,
        Err(error) => {
            set_command_failure(
                &mut result,
                CommandFailure::new("hotplug_reconnected_create", error),
            );
            push_hotplug_transition(&mut result, "failed");
            return Ok(result);
        }
    };
    result["resources"]["reconnected"]["created"] = json!(true);
    let mut result = finish_harness_phase(
        reconnected,
        result,
        "cleanup",
        "reconnected_harness_evidence",
        "hotplug_reconnected_close",
        |harness, result| {
            result["reconnect_operation"] = connect_for_command_controlled(
                harness,
                control,
                ConnectOptions::default(),
                "hotplug_reconnect_admit",
                "hotplug_reconnect_wait",
                "hotplug_reconnect_terminal",
            )?;
            result["reconnect_baud"] = json!(harness.actual_baud());
            push_hotplug_transition(result, "reconnected");
            Ok(())
        },
    );
    if result["operator_terminal_outcome"] == "interrupted" {
        push_hotplug_transition(&mut result, "cancelled");
    } else if result.get("execution_error").is_none() && cleanup_succeeded(&result["cleanup"]) {
        push_hotplug_transition(&mut result, "closed");
    } else {
        push_hotplug_transition(&mut result, "failed");
    }
    Ok(result)
}

fn run_lifecycle(
    arguments: &[String],
    initial_target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
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
        if let Err(failure) = control.checkpoint("lifecycle_cycle_interrupt") {
            set_command_failure(&mut result, failure);
            return Ok(result);
        }
        let target = next_target
            .take()
            .expect("a successful prior admission provides the next lifecycle target");
        let cycle_admission = admission_evidence_json(target.evidence(), "admitted");
        let harness = match Harness::new(&target, discovery.clone(), ControllerOptions::default()) {
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
                record["connect_operation"] = connect_for_command_controlled(
                    harness,
                    control,
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
        if let Err(failure) = control.checkpoint("lifecycle_discovery_interrupt") {
            record["status"] = json!("cancelled");
            result["records"]
                .as_array_mut()
                .expect("lifecycle records are an array")
                .push(record);
            set_command_failure(&mut result, failure);
            return Ok(result);
        }
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
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<(Value, Option<Vec<u8>>), String> {
    let steps = value_or(arguments, "--steps", 10_000_usize)?;
    if !(1..=10_000).contains(&steps) {
        return Err("--steps must be in 1..=10000".to_owned());
    }
    let harness = Harness::new(&target, discovery, ControllerOptions::default())?;
    let telemetry = harness.telemetry.clone();
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
            result["connect_operation"] = connect_for_command_controlled(
                harness,
                control,
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
            control.checkpoint("sequence_admit")?;
            let operation = harness
                .controller
                .precise_sequence(sequence)
                .map_err(|error| CommandFailure::new("sequence_admit", error.to_string()))?;
            let expected_seconds = u64::try_from(steps)
                .expect("step count fits u64")
                .saturating_mul(30)
                .div_ceil(1_000)
                .saturating_add(60);
            let (operation, state) = wait_for_command_terminal_controlled(
                operation,
                control,
                Duration::from_secs(expected_seconds),
                "sequence_terminal_wait",
            )?;
            let ended = harness.now_ns();
            result["operation"] = operation_json(&operation);
            result["functional_success"] = json!(state == OperationState::Succeeded);
            control.checkpoint("sequence_reset_admit")?;
            let reset = harness
                .controller
                .reset()
                .map_err(|error| CommandFailure::new("sequence_reset_admit", error.to_string()))?;
            result["reset_operation"] = wait_for_command_success_controlled(
                reset,
                control,
                OPERATION_TIMEOUT,
                "sequence_reset_wait",
                "sequence_reset_terminal",
            )?;
            let timing_count = harness
                .telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .logical_reports
                .accepted_count();
            result["accepted_report_count_before_final_reset"] = json!(
                harness
                    .controller
                    .snapshot()
                    .accepted_report_count
                    .checked_sub(1)
                    .ok_or_else(|| CommandFailure::new(
                        "sequence_report_count",
                        "final reset was not reflected in the accepted report count",
                    ))?
            );
            result["recorded_complete_writes"] = json!(timing_count);
            result["elapsed_ns"] = json!(ended.checked_sub(started).ok_or_else(|| {
                CommandFailure::new(
                    "sequence_elapsed_time",
                    "sequence terminal timestamp preceded its start timestamp",
                )
            })?);
            result["actual_baud"] = json!(harness.actual_baud());
            Ok(())
        },
    );
    let mut result = result;
    match timing_csv_bytes(&telemetry) {
        Ok(bytes) => timing_csv = Some(bytes),
        Err(error) => {
            result["execution_error"] = json!({
                "stage": "sequence_timing_render",
                "message": error,
            });
        }
    }
    result["latency"] = latency_json(
        &telemetry,
        telemetry_actual_baud(&telemetry),
        Some(SEQUENCE_TIMINGS_FILE_NAME),
    );
    Ok((result, timing_csv))
}

enum AmiiboCommand {
    Unauthorized,
    Authorized(AmiiboAuthorization),
}

struct AmiiboAuthorization {
    lease_id: String,
    expected_stable_id: String,
    initial_port_hint: String,
    slot: u8,
    disposable_slot: u8,
    slot_count: u16,
    maximum_data_len: usize,
    limits_source: String,
    expected_sha256: String,
    recomputed_sha256: String,
    payload: Arc<[u8]>,
    limits: AmiiboLimits,
}

fn prepare_amiibo_command(arguments: &[String], lease_id: &str) -> Result<AmiiboCommand, String> {
    match arguments
        .iter()
        .filter(|argument| argument.as_str() == "--authorize-write")
        .count()
    {
        0 => return Ok(AmiiboCommand::Unauthorized),
        1 => {}
        _ => return Err("--authorize-write may be supplied only once".to_owned()),
    }
    if arguments
        .iter()
        .any(|argument| argument == "--confirm-disposable")
    {
        return Err("--confirm-disposable is not slot-bound; use --disposable-slot N".to_owned());
    }

    let target = device_target_request(arguments)?;
    let slot: u8 = required_value(arguments, "--slot")?;
    let disposable_slot: u8 = required_value(arguments, "--disposable-slot")?;
    if disposable_slot != slot {
        return Err("--disposable-slot must exactly match --slot".to_owned());
    }
    let slot_count: u16 = required_value(arguments, "--slot-count")?;
    let maximum_data_len: usize = required_value(arguments, "--maximum-data-len")?;
    let limits =
        AmiiboLimits::new(slot_count, maximum_data_len).map_err(|error| error.to_string())?;
    if u16::from(slot) >= slot_count {
        return Err("--slot is outside the declared slot count".to_owned());
    }

    let limits_source = required_non_option_string(arguments, "--limits-source")?;
    validate_limits_source(&limits_source)?;
    let data_path = PathBuf::from(required_non_option_string(arguments, "--data")?);
    let payload =
        fs::read(&data_path).map_err(|error| format!("cannot read Amiibo data: {error}"))?;
    if payload.is_empty() {
        return Err("Amiibo payload must be non-empty".to_owned());
    }
    if payload.len() > maximum_data_len {
        return Err("Amiibo payload exceeds --maximum-data-len".to_owned());
    }
    let expected_sha256 = required_non_option_string(arguments, "--expected-sha256")?;
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("--expected-sha256 must contain exactly 64 hexadecimal digits".to_owned());
    }
    let expected_sha256 = expected_sha256.to_ascii_uppercase();
    let recomputed_sha256 = sha256_bytes(&payload);
    if expected_sha256 != recomputed_sha256 {
        return Err("Amiibo payload SHA-256 does not match --expected-sha256".to_owned());
    }

    Ok(AmiiboCommand::Authorized(AmiiboAuthorization {
        lease_id: lease_id.to_owned(),
        expected_stable_id: target.expected_stable_id().to_owned(),
        initial_port_hint: target.initial_port_hint().to_owned(),
        slot,
        disposable_slot,
        slot_count,
        maximum_data_len,
        limits_source,
        expected_sha256,
        recomputed_sha256,
        payload: Arc::from(payload),
        limits,
    }))
}

fn validate_limits_source(source: &str) -> Result<(), String> {
    if source.trim().is_empty()
        || source.trim() != source
        || source.chars().any(char::is_control)
        || Path::new(source).is_absolute()
    {
        return Err(
            "--limits-source must be a non-empty external reference, not a machine path".to_owned(),
        );
    }
    Ok(())
}

fn run_unauthorized_amiibo() -> Result<Value, String> {
    Ok(json!({
        "command": "amiibo",
        "capability": "unknown",
        "capability_inference": "none",
        "o_02_status": "open",
        "write_performed": false,
        "reason": "the v1 protocol has no safe capacity query; explicit limits and write authorization are required",
    }))
}

fn run_amiibo(
    authorization: AmiiboAuthorization,
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    if authorization.expected_stable_id != target.request().expected_stable_id()
        || authorization.initial_port_hint != target.request().initial_port_hint()
        || authorization.lease_id != control.lease_id()
    {
        return Err("Amiibo authorization is not bound to this run and device target".to_owned());
    }
    let AmiiboAuthorization {
        lease_id,
        expected_stable_id,
        initial_port_hint,
        slot,
        disposable_slot,
        slot_count,
        maximum_data_len,
        limits_source,
        expected_sha256,
        recomputed_sha256,
        payload,
        limits,
    } = authorization;
    let observed_stable_id = target.descriptor().stable_id().to_owned();
    let recorder = AmiiboEvidenceRecorder::default();
    let amiibo_evidence = AmiiboEvidenceConfig {
        journal: control.journal_writer()?,
        binding: AmiiboEvidenceBinding {
            lease_id: lease_id.clone(),
            expected_stable_id: expected_stable_id.clone(),
            observed_stable_id: observed_stable_id.clone(),
            slot,
            payload_len: payload.len(),
            payload_sha256: recomputed_sha256.clone(),
        },
        recorder: recorder.clone(),
    };
    let harness = Harness::new_with_amiibo_evidence(
        &target,
        discovery,
        ControllerOptions {
            amiibo_limits: Some(limits),
            ..ControllerOptions::default()
        },
        Some(amiibo_evidence),
    )?;
    let result = json!({
        "command": "amiibo",
        "write_attempted": false,
        "write_performed": false,
        "select_performed": false,
        "write_intent_durable": false,
        "save_terminal_durable": false,
        "select_intent_durable": false,
        "select_terminal_durable": false,
        "slot": slot,
        "authorization": {
            "status": "authorized",
            "one_time": true,
            "lease_id": lease_id,
            "expected_stable_id": expected_stable_id,
            "observed_stable_id": observed_stable_id,
            "initial_port_hint": initial_port_hint,
            "slot": slot,
            "disposable_slot": disposable_slot,
            "all_predicates_satisfied": true,
        },
        "declared_limits": {
            "classification": "declared_external",
            "slot_count": slot_count,
            "maximum_data_len": maximum_data_len,
            "source": limits_source,
            "measured": false,
        },
        "payload": {
            "source": {"kind": "local_file", "path_recorded": false},
            "actual_length": payload.len(),
            "expected_sha256": expected_sha256,
            "recomputed_sha256": recomputed_sha256,
            "hash_matches": true,
            "raw_bytes_recorded": false,
        },
        "capability_inference": "none",
        "o_02_status": "open",
        "resources": {"primary": {"created": true}},
    });
    let mut result = finish_harness_phase(
        harness,
        result,
        "cleanup",
        "harness_evidence",
        "amiibo_close",
        move |harness, result| execute_amiibo(harness, control, slot, payload, result),
    );
    result["amiibo_protocol_evidence"] = recorder.projection();
    Ok(result)
}

fn execute_amiibo(
    harness: &Harness,
    control: &mut RunControl<'_>,
    slot: u8,
    payload: Arc<[u8]>,
    result: &mut Value,
) -> Result<(), CommandFailure> {
    result["connect_operation"] = connect_for_command_controlled(
        harness,
        control,
        ConnectOptions::default(),
        "amiibo_connect_admit",
        "amiibo_connect_wait",
        "amiibo_connect_terminal",
    )?;
    control.checkpoint("amiibo_save_admit")?;
    control
        .record_event(
            JournalEventKind::AmiiboWriteIntent,
            json!({
                "command": "amiibo",
                "authorization": result["authorization"].clone(),
                "declared_limits": result["declared_limits"].clone(),
                "payload": result["payload"].clone(),
                "capability_inference": "none",
                "o_02_status": "open",
            }),
        )
        .map_err(|error| CommandFailure::new("amiibo_write_intent_journal", error))?;
    result["write_intent_durable"] = json!(true);
    result["write_attempted"] = json!(true);
    let save = harness
        .controller
        .save_amiibo(slot, payload, AmiiboSaveOptions::default())
        .map_err(|error| CommandFailure::new("amiibo_save_admit", error.to_string()))?;
    wait_for_amiibo_operation(
        save,
        control,
        result,
        AmiiboOperationWaitSpec {
            timeout: Duration::from_secs(60),
            wait_stage: "amiibo_save_wait",
            terminal_stage: "amiibo_save_terminal",
            journal_kind: JournalEventKind::AmiiboSaveTerminal,
            result_field: "save",
            durable_field: "save_terminal_durable",
        },
    )?;
    result["write_performed"] = json!(true);
    control.checkpoint("amiibo_select_admit")?;
    control
        .record_event(
            JournalEventKind::AmiiboSelectIntent,
            json!({
                "layer": "operation_admission",
                "authorization": result["authorization"].clone(),
                "slot": slot,
            }),
        )
        .map_err(|error| CommandFailure::new("amiibo_select_intent_journal", error))?;
    result["select_intent_durable"] = json!(true);
    let select = harness
        .controller
        .select_amiibo(slot, AmiiboSelectOptions::default())
        .map_err(|error| CommandFailure::new("amiibo_select_admit", error.to_string()))?;
    wait_for_amiibo_operation(
        select,
        control,
        result,
        AmiiboOperationWaitSpec {
            timeout: OPERATION_TIMEOUT,
            wait_stage: "amiibo_select_wait",
            terminal_stage: "amiibo_select_terminal",
            journal_kind: JournalEventKind::AmiiboSelectTerminal,
            result_field: "select",
            durable_field: "select_terminal_durable",
        },
    )?;
    result["select_performed"] = json!(true);
    Ok(())
}

#[derive(Clone, Debug)]
struct ObservedActionSpec {
    action_id: String,
    label: String,
    active_label: String,
    active: ControllerAction,
    release_label: String,
    release: ControllerAction,
}

fn smoke_observation_specs(full: bool) -> Vec<ObservedActionSpec> {
    let buttons: &[Button] = if full { &Button::ALL } else { &[Button::A] };
    let mut specs = buttons
        .iter()
        .copied()
        .map(|button| {
            let action_id = format!("button.{button:?}");
            ObservedActionSpec {
                label: action_id.clone(),
                active_label: format!("{action_id}.down"),
                active: ControllerAction::ButtonDown(button),
                release_label: format!("{action_id}.up"),
                release: ControllerAction::ButtonUp(button),
                action_id,
            }
        })
        .collect::<Vec<_>>();
    if !full {
        return specs;
    }
    specs.extend(
        Hat::ALL
            .into_iter()
            .filter(|hat| *hat != Hat::Center)
            .map(|hat| {
                let action_id = format!("hat.{hat:?}");
                ObservedActionSpec {
                    label: action_id.clone(),
                    active_label: action_id.clone(),
                    active: ControllerAction::Hat(hat),
                    release_label: "hat.Center".to_owned(),
                    release: ControllerAction::Hat(Hat::Center),
                    action_id,
                }
            }),
    );
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
            let action_id = format!("stick.{side}.{},{}", position.x, position.y);
            specs.push(ObservedActionSpec {
                label: action_id.clone(),
                active_label: action_id.clone(),
                active: constructor(position),
                release_label: format!("stick.{side}.center"),
                release: constructor(StickPosition::CENTER),
                action_id,
            });
        }
    }
    specs
}

fn operator_timing(
    arguments: &[String],
    legacy_hold_ms: Option<u64>,
) -> Result<(u64, Duration), String> {
    if arguments.iter().any(|argument| argument == "--yes") {
        return Err("blanket --yes is not supported for operator observations".to_owned());
    }
    let configured_window = option_value(arguments, "--observation-window-ms")?;
    if configured_window.is_some() && legacy_hold_ms.is_some() {
        return Err("--hold-ms and --observation-window-ms cannot be used together".to_owned());
    }
    let observation_window_ms = match configured_window {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| "invalid value for --observation-window-ms".to_owned())?,
        None => legacy_hold_ms.unwrap_or(1_000),
    };
    if !(100..=5_000).contains(&observation_window_ms) {
        return Err("--observation-window-ms must be in 100..=5000".to_owned());
    }
    let timeout_seconds = value_or(arguments, "--operator-timeout-seconds", 30_u64)?;
    if !(1..=300).contains(&timeout_seconds) {
        return Err("--operator-timeout-seconds must be in 1..=300".to_owned());
    }
    Ok((observation_window_ms, Duration::from_secs(timeout_seconds)))
}

#[allow(clippy::too_many_arguments)]
fn exercise_observed_action_unit(
    harness: &Harness,
    control: &mut RunControl<'_>,
    command: &str,
    expected_stable_id: &str,
    observed_stable_id: &str,
    spec: &ObservedActionSpec,
    observation_window_ms: u64,
    response_timeout: Duration,
    result: &mut Value,
    delay: &dyn QualificationDelay,
) -> Result<OperatorOutcome, CommandFailure> {
    let marker = control.marker(
        command,
        expected_stable_id,
        observed_stable_id,
        &spec.action_id,
        &spec.label,
    );
    control.announce(&marker)?;
    let active = exercise_action_for_command_controlled(
        harness,
        control,
        &spec.active_label,
        spec.active,
        &mut result["actions"],
    )?;
    wait_controlled(
        control,
        Duration::from_millis(observation_window_ms),
        delay,
        "observation_window_interrupt",
    )?;
    let release = exercise_action_for_command_controlled(
        harness,
        control,
        &spec.release_label,
        spec.release,
        &mut result["actions"],
    )?;
    let neutral = exercise_action_for_command_controlled(
        harness,
        control,
        "neutral",
        ControllerAction::Reset,
        &mut result["actions"],
    )?;
    let neutral_snapshot = snapshot_json(&harness.controller);
    if neutral_snapshot["desired_report_neutral"] != true {
        return Err(CommandFailure::new(
            "operator_observation_neutral",
            format!("action {} did not return to neutral", spec.action_id),
        ));
    }
    let (outcome, observation) = control.observe_action(
        marker,
        observation_window_ms,
        response_timeout,
        ActionOperationEvidence {
            active,
            release,
            neutral,
        },
        neutral_snapshot,
    )?;
    result["operator_observations"]
        .as_array_mut()
        .expect("operator observations are an array")
        .push(observation);
    Ok(outcome)
}

#[cfg(test)]
fn exercise_action_for_command(
    harness: &Harness,
    label: &str,
    action: ControllerAction,
    output: &mut Value,
) -> Result<Value, CommandFailure> {
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
        .push(json!({"label": label, "operation": operation.clone()}));
    Ok(operation)
}

fn exercise_action_for_command_controlled(
    harness: &Harness,
    control: &mut RunControl<'_>,
    label: &str,
    action: ControllerAction,
    output: &mut Value,
) -> Result<Value, CommandFailure> {
    control.checkpoint("action_admit")?;
    let operation = harness
        .controller
        .direct(action)
        .map_err(|error| CommandFailure::new("action_admit", format!("{label}: {error}")))?;
    let operation = wait_for_command_success_controlled(
        operation,
        control,
        OPERATION_TIMEOUT,
        "action_wait",
        "action_terminal",
    )?;
    output
        .as_array_mut()
        .expect("action output is an array")
        .push(json!({"label": label, "operation": operation.clone()}));
    Ok(operation)
}

fn exercise_smoke_action_controlled(
    harness: &Harness,
    control: &mut RunControl<'_>,
    label: &str,
    action: ControllerAction,
    result: &mut Value,
    record_diagnostic_step: bool,
) -> Result<(), CommandFailure> {
    exercise_action_for_command_controlled(
        harness,
        control,
        label,
        action,
        &mut result["actions"],
    )?;
    if record_diagnostic_step {
        record_last_smoke_action_in_diagnostic(result);
    }
    Ok(())
}

fn record_last_smoke_action_in_diagnostic(result: &mut Value) {
    let action = result["actions"]
        .as_array()
        .and_then(|actions| actions.last())
        .cloned()
        .expect("a completed smoke action was just recorded");
    result["diagnostic_prelude_steps"]
        .as_array_mut()
        .expect("diagnostic prelude steps are an array")
        .push(json!({
            "kind": "action",
            "label": action["label"],
            "operation": action["operation"],
        }));
}

fn smoke_home_delay(arguments: &[String], wake_home: bool) -> Result<Option<u64>, String> {
    let configured = option_value(arguments, "--post-home-neutral-delay-ms")?;
    match (wake_home, configured) {
        (true, Some(value)) => {
            let delay_ms = value
                .parse::<u64>()
                .map_err(|_| "invalid value for --post-home-neutral-delay-ms".to_owned())?;
            if delay_ms > 60_000 {
                return Err("--post-home-neutral-delay-ms must be in 0..=60000".to_owned());
            }
            Ok(Some(delay_ms))
        }
        (true, None) => {
            Err("--wake-home requires --post-home-neutral-delay-ms in 0..=60000".to_owned())
        }
        (false, Some(_)) => Err("--post-home-neutral-delay-ms requires --wake-home".to_owned()),
        (false, None) => Ok(None),
    }
}

#[cfg(test)]
fn record_configured_home_delay(
    result: &mut Value,
    configured_delay_ms: u64,
    delay: &dyn QualificationDelay,
) {
    delay.wait(Duration::from_millis(configured_delay_ms));
    result["diagnostic_prelude_steps"]
        .as_array_mut()
        .expect("diagnostic prelude steps are an array")
        .push(json!({
            "kind": "configured_wait",
            "configured_post_home_neutral_delay_ms": configured_delay_ms,
            "status": "completed",
        }));
}

fn record_configured_home_delay_controlled(
    result: &mut Value,
    control: &mut RunControl<'_>,
    configured_delay_ms: u64,
    delay: &dyn QualificationDelay,
) -> Result<(), CommandFailure> {
    wait_controlled(
        control,
        Duration::from_millis(configured_delay_ms),
        delay,
        "diagnostic_delay_interrupt",
    )?;
    result["diagnostic_prelude_steps"]
        .as_array_mut()
        .expect("diagnostic prelude steps are an array")
        .push(json!({
            "kind": "configured_wait",
            "configured_post_home_neutral_delay_ms": configured_delay_ms,
            "status": "completed",
        }));
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

fn wait_terminal_sliced(
    operation: &Operation,
    timeout: Duration,
) -> Result<easycon_runtime::OperationSnapshot, String> {
    let started = Instant::now();
    loop {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(format!("operation {} wait timed out", operation.id().get()));
        };
        if remaining.is_zero() {
            return Err(format!("operation {} wait timed out", operation.id().get()));
        }
        match operation.wait(WaitTimeout::For(remaining.min(INTERRUPT_POLL_SLICE))) {
            WaitResult::Completed(snapshot) => return Ok(snapshot),
            WaitResult::Timeout => {}
        }
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

fn operation_snapshot_json(snapshot: &easycon_runtime::OperationSnapshot) -> Value {
    json!({
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

fn identity_open_attempts_json(recorder: &IdentityOpenRecorder) -> Value {
    Value::Array(
        recorder
            .attempts()
            .iter()
            .map(|attempt| {
                let inner_open = match &attempt.inner_open {
                    InnerOpenOutcome::NotAttempted => json!({"status": "not_attempted"}),
                    InnerOpenOutcome::Opened => json!({"status": "opened"}),
                    InnerOpenOutcome::Failed(error) => json!({
                        "status": "failed",
                        "structured_error": serial_error_json(error),
                    }),
                };
                json!({
                    "baud": attempt.baud,
                    "pre_open": open_guard_check_json(&attempt.pre_open),
                    "inner_open": inner_open,
                    "post_open": attempt.post_open.as_ref().map(open_guard_check_json),
                    "stream_returned": attempt.stream_returned,
                })
            })
            .collect(),
    )
}

fn open_guard_check_json(check: &OpenGuardCheck) -> Value {
    json!({
        "status": check.status.as_str(),
        "snapshot": check.snapshot.iter().map(port_json).collect::<Vec<_>>(),
        "observed_expected": check.expected.as_ref().map(port_json),
        "observed_hint": check.hint.as_ref().map(port_json),
        "structured_error": check.error.as_ref().map(serial_error_json),
    })
}

fn latency_json(
    telemetry: &Arc<Mutex<Telemetry>>,
    baud: Option<u32>,
    csv_path: Option<&str>,
) -> Value {
    telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .logical_reports
        .projection_json(baud, csv_path)
}

fn telemetry_actual_baud(telemetry: &Arc<Mutex<Telemetry>>) -> Option<u32> {
    telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .actual_baud
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
    telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .logical_reports
        .csv_bytes()
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
         easycon-hardware-qualification smoke --port COMx --expected-identity ID [--full] [--wake-left-stick] [--wake-home --post-home-neutral-delay-ms N] [--hold-ms N | --observation-window-ms N] [--operator-timeout-seconds N]\n  \
         easycon-hardware-qualification home-wake --port COMx --expected-identity ID [--attempts 20] [--interval-seconds 3] [--observation-window-ms N] [--operator-timeout-seconds N]\n  \
         easycon-hardware-qualification faults --port COMx --expected-identity ID\n  \
         easycon-hardware-qualification hotplug --port COMx --expected-identity ID [--timeout-seconds N]\n  \
         easycon-hardware-qualification lifecycle --port COMx --expected-identity ID [--cycles 100]\n  \
         easycon-hardware-qualification sequence --port COMx --expected-identity ID [--steps 10000]\n  \
         easycon-hardware-qualification amiibo [--port COMx --expected-identity ID --slot N --disposable-slot N \
         --slot-count N --maximum-data-len N --limits-source REF --data FILE --expected-sha256 HEX \
         --authorize-write]\n  \
         easycon-hardware-qualification checkpoint --runs-root PATH [--attestations-root PATH] --output-dir PATH\n\n  \
         Every command accepts --output-dir PATH."
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc::{Receiver, SyncSender};

    use easycon_controller::{DirectWriteTiming, TransportErrorKind};
    use easycon_model::{EasyConError, ErrorCode, ErrorDomain, OperationId};
    use easycon_runtime::{
        CancellationReason, CancellationToken, ClockChangeRegistration, DeadlineId,
        ManagedResource, OperationValue, VirtualClock,
    };
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

    struct ScriptedOperatorPort {
        outcomes: VecDeque<OperatorOutcome>,
        announced: Vec<ActionMarker>,
        observed: Vec<ObservationRequest>,
    }

    struct ScriptedNowClock {
        times: Mutex<VecDeque<u64>>,
        fallback: VirtualClock,
    }

    impl ScriptedNowClock {
        fn new(times: impl IntoIterator<Item = u64>) -> Self {
            Self {
                times: Mutex::new(times.into_iter().collect()),
                fallback: VirtualClock::default(),
            }
        }
    }

    impl Clock for ScriptedNowClock {
        fn now_ns(&self) -> u64 {
            self.times
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front()
                .expect("scripted monotonic timestamp")
        }

        fn on_change(&self, hook: Arc<dyn Fn() + Send + Sync>) -> ClockChangeRegistration {
            self.fallback.on_change(hook)
        }

        fn register_deadline(&self, target_ns: u64) -> DeadlineId {
            self.fallback.register_deadline(target_ns)
        }

        fn record_dispatch(&self, id: DeadlineId, actual_ns: u64) {
            self.fallback.record_dispatch(id, actual_ns);
        }

        fn real_wait_duration(&self, target_ns: u64) -> Option<Duration> {
            self.fallback.real_wait_duration(target_ns)
        }
    }

    #[derive(Default)]
    struct ThreeThenFiveTransport {
        calls: usize,
    }

    struct NativeFailureByteIo {
        controller_write_calls: usize,
    }

    impl ByteIo for NativeFailureByteIo {
        fn read(
            &mut self,
            buffer: &mut [u8],
            _request: ByteIoRequest,
        ) -> Result<usize, SerialError> {
            buffer[0] = 0x80;
            Ok(1)
        }

        fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError> {
            if matches!(request.operation, ByteIoOperation::ControllerWrite(_)) {
                self.controller_write_calls += 1;
                if self.controller_write_calls == 1 {
                    return Ok(3);
                }
                return Err(SerialError::with_os_code(
                    SerialErrorKind::Io,
                    "injected native controller write failure",
                    995,
                ));
            }
            Ok(buffer.len())
        }

        fn discard_input(&mut self, _request: ByteIoRequest) -> Result<(), SerialError> {
            Ok(())
        }

        fn close(&mut self) {}
    }

    struct NativeFailureFactory;

    impl ByteIoFactory for NativeFailureFactory {
        fn open(
            &mut self,
            _port: &SerialPortDescriptor,
            _baud_rate: u32,
            _request: ByteIoRequest,
        ) -> Result<Box<dyn ByteIo>, SerialError> {
            Ok(Box::new(NativeFailureByteIo {
                controller_write_calls: 0,
            }))
        }
    }

    impl ControllerTransport for ThreeThenFiveTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            self.calls += 1;
            if self.calls == 1 {
                Ok(3)
            } else {
                Ok(request.bytes.len())
            }
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "ACK is not used by the partial report regression",
            ))
        }

        fn close(&mut self) {}
    }

    impl ScriptedOperatorPort {
        fn new(outcomes: impl IntoIterator<Item = OperatorOutcome>) -> Self {
            Self {
                outcomes: outcomes.into_iter().collect(),
                announced: Vec::new(),
                observed: Vec::new(),
            }
        }
    }

    impl OperatorPort for ScriptedOperatorPort {
        fn announce(&mut self, marker: &ActionMarker) -> Result<(), String> {
            self.announced.push(marker.clone());
            Ok(())
        }

        fn observe(
            &mut self,
            request: &ObservationRequest,
            _interrupt: &InterruptToken,
        ) -> Result<OperatorOutcome, String> {
            self.observed.push(request.clone());
            self.outcomes
                .pop_front()
                .ok_or_else(|| "scripted operator response exhausted".to_owned())
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

    fn passing_latency() -> Value {
        json!({
            "integrity": {"status": "passed", "errors": []},
            "logical_reports": {
                "attempt_count": 0,
                "accepted_count": 0,
                "failed_count": 0,
                "pending_count": 0,
                "contradiction_count": 0,
                "detail": {"kind": "inline", "rows": []},
            },
            "sample_count": 0,
            "direct_sample_count": 0,
            "command_admitted_to_write_entered_ns": null,
            "dispatch_to_write_entered_ns": null,
            "write_entered_to_os_acceptance_ns": null,
            "uart_complete_frame": null,
            "usb_hid": {
                "qualification_status": "unverified",
                "measured": false,
                "reason": "synthetic fixture has no USB analyzer",
            },
            "switch_physical_order": {
                "qualification_status": "unverified",
                "measured": false,
                "reason": "synthetic fixture has no physical order evidence",
            },
        })
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
        let mut result = json!({
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
        });
        for field in [
            "occupier_report_telemetry",
            "occupied_probe_report_telemetry",
            "cancel_report_telemetry",
            "deadline_report_telemetry",
        ] {
            result[field] = passing_latency();
        }
        result
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

    struct SuccessfulAmiiboTransport;

    impl ControllerTransport for SuccessfulAmiiboTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
            Ok(AckFrame {
                generation: request.generation,
                byte: request.expected_reply,
            })
        }

        fn close(&mut self) {}
    }

    struct AmiiboFinalNeutralFailureTransport;

    impl ControllerTransport for AmiiboFinalNeutralFailureTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            if request.context.kind == WriteKind::Neutralize
                && request.context.operation_id.is_none()
            {
                Err(TransportError::new(
                    TransportErrorKind::Io,
                    "injected Amiibo final neutral failure",
                ))
            } else {
                Ok(request.bytes.len())
            }
        }

        fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
            Ok(AckFrame {
                generation: request.generation,
                byte: request.expected_reply,
            })
        }

        fn close(&mut self) {}
    }

    struct BlockingAmiiboAckTransport {
        block_at_ack: usize,
        ack_count: usize,
        entered: Option<SyncSender<()>>,
    }

    impl ControllerTransport for BlockingAmiiboAckTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
            self.ack_count += 1;
            if self.ack_count == self.block_at_ack {
                self.entered
                    .take()
                    .expect("one blocking ACK")
                    .send(())
                    .expect("announce blocking ACK");
                let (cancelled, wait_cancelled) = std::sync::mpsc::sync_channel(1);
                request.cancellation.on_cancel(move || {
                    let _ = cancelled.send(());
                });
                wait_cancelled.recv().expect("operation cancellation");
                return Err(TransportError::new(
                    TransportErrorKind::Cancelled,
                    "injected cancellable Amiibo ACK",
                ));
            }
            Ok(AckFrame {
                generation: request.generation,
                byte: request.expected_reply,
            })
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
            identity_open_recorder: IdentityOpenRecorder::default(),
            descriptor: SerialPortDescriptor::new(format!("TEST\\{label}"), "COM1")
                .expect("descriptor"),
        }
    }

    fn observed_amiibo_harness(
        transport: Box<dyn ControllerTransport>,
        writer: journal::JournalWriter,
        recorder: AmiiboEvidenceRecorder,
        lease_id: &str,
        label: &str,
        payload: &[u8],
    ) -> Harness {
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let runtime = Runtime::new(clock.clone());
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let stable_id = format!("TEST\\{label}");
        let instrumented = AmiiboEvidenceTransport::new(
            transport,
            writer,
            AmiiboEvidenceBinding {
                lease_id: lease_id.to_owned(),
                expected_stable_id: stable_id.clone(),
                observed_stable_id: stable_id.clone(),
                slot: 3,
                payload_len: payload.len(),
                payload_sha256: sha256_bytes(payload),
            },
            recorder,
        );
        let observed = ObservedTransport {
            inner: Box::new(instrumented),
            clock: clock.clone(),
            telemetry: telemetry.clone(),
        };
        let controller = ControllerSession::new(
            &runtime,
            Box::new(observed),
            ControllerOptions {
                amiibo_limits: Some(AmiiboLimits::new(8, 64).expect("limits")),
                ..ControllerOptions::default()
            },
        )
        .expect("controller");
        Harness {
            runtime,
            clock,
            controller,
            telemetry,
            identity_open_recorder: IdentityOpenRecorder::default(),
            descriptor: SerialPortDescriptor::new(stable_id, "COM1").expect("descriptor"),
        }
    }

    fn amiibo_execution_result(lease_id: &str, stable_id: &str, payload: &[u8]) -> Value {
        let payload_sha256 = sha256_bytes(payload);
        json!({
            "command": "amiibo",
            "device_target": {
                "expected_stable_id": stable_id,
                "initial_port_hint": "COM1",
            },
            "identity_admission": {
                "status": "admitted",
                "observed_expected": {"stable_id": stable_id, "port": "COM1"},
            },
            "write_attempted": false,
            "write_performed": false,
            "select_performed": false,
            "write_intent_durable": false,
            "save_terminal_durable": false,
            "select_intent_durable": false,
            "select_terminal_durable": false,
            "slot": 3,
            "authorization": {
                "status": "authorized",
                "one_time": true,
                "lease_id": lease_id,
                "expected_stable_id": stable_id,
                "observed_stable_id": stable_id,
                "initial_port_hint": "COM1",
                "slot": 3,
                "disposable_slot": 3,
                "all_predicates_satisfied": true,
            },
            "declared_limits": {
                "classification": "declared_external",
                "slot_count": 8,
                "maximum_data_len": 64,
                "source": "operator-attestation:test-fixture",
                "measured": false,
            },
            "payload": {
                "source": {"kind": "local_file", "path_recorded": false},
                "actual_length": payload.len(),
                "expected_sha256": payload_sha256,
                "recomputed_sha256": payload_sha256,
                "hash_matches": true,
                "raw_bytes_recorded": false,
            },
            "capability_inference": "none",
            "o_02_status": "open",
            "resources": {"primary": {"created": true}},
        })
    }

    fn valid_amiibo_qualification_result() -> Value {
        let payload = [1_u8, 2, 3];
        let mut result = amiibo_execution_result("lease-amiibo", "DEVICE\\EXPECTED", &payload);
        result["write_attempted"] = json!(true);
        result["write_performed"] = json!(true);
        result["select_performed"] = json!(true);
        result["write_intent_durable"] = json!(true);
        result["save_terminal_durable"] = json!(true);
        result["select_intent_durable"] = json!(true);
        result["select_terminal_durable"] = json!(true);
        result["save"] = json!({"state": "Succeeded"});
        result["select"] = json!({"state": "Succeeded"});
        result["amiibo_protocol_evidence"] = json!({
            "chunks": [{
                "operation_id": 1,
                "offset": 0,
                "length": 3,
                "attempt": 1,
                "header_write_sequence": 1,
                "header_accepted_bytes": 7,
                "header_acknowledged": true,
                "payload_write_sequence": 2,
                "payload_accepted_bytes": 3,
                "payload_acknowledged": true,
                "status": "acked",
                "error": null,
            }],
            "selects": [{
                "operation_id": 2,
                "slot": 3,
                "write_sequence": 3,
                "accepted_bytes": 3,
                "acknowledged": true,
                "status": "acked",
                "error": null,
            }],
            "cleanup_resets": [],
            "evidence_errors": [],
            "transport_closed_in_state": "complete",
        });
        result["harness_evidence"] = json!({
            "logical_report_telemetry": passing_latency(),
        });
        result["cleanup"] = successful_cleanup();
        result
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

    struct SequencedDeviceDiscovery {
        results: Mutex<VecDeque<Result<Vec<SerialPortDescriptor>, SerialError>>>,
        calls: Arc<AtomicUsize>,
    }

    impl DeviceDiscovery for ScriptedDeviceDiscovery {
        fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    impl DeviceDiscovery for SequencedDeviceDiscovery {
        fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front()
                .expect("scripted hotplug discovery result")
        }
    }

    #[derive(Default)]
    struct FakeHotplugTimer {
        elapsed: Duration,
        waits: Vec<Duration>,
    }

    impl HotplugPollTimer for FakeHotplugTimer {
        fn elapsed(&self) -> Duration {
            self.elapsed
        }

        fn wait(&mut self, duration: Duration) {
            self.waits.push(duration);
            self.elapsed += duration;
        }
    }

    #[derive(Default)]
    struct FakeDelay {
        waits: Mutex<Vec<Duration>>,
    }

    impl QualificationDelay for FakeDelay {
        fn wait(&self, duration: Duration) {
            self.waits
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(duration);
        }
    }

    struct InterruptingDelay {
        token: InterruptToken,
        calls: AtomicUsize,
    }

    impl QualificationDelay for InterruptingDelay {
        fn wait(&self, _duration: Duration) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.token.request();
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
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document["exit_code"], 1);
        assert_eq!(document["command"], "handshake");
        assert_eq!(document["error"], "protocol timeout");
        assert_eq!(error.as_deref(), Some("protocol timeout"));
    }

    #[test]
    fn untrusted_binary_provenance_cannot_remain_passed() {
        let mut document = final_document("synthetic", RunOutcome::Passed, Vec::new(), None, None);
        apply_provenance_policy(
            &mut document,
            &RuntimeProvenance::synthetic_with_trust(false),
        );

        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 2);
        assert_eq!(
            document["checks"][0],
            qualification_check("build_and_executable_provenance", "unverified")
        );
        assert_eq!(document["provenance"]["trusted"], false);
    }

    #[test]
    fn trusted_binary_provenance_preserves_the_command_decision() {
        let mut document = final_document("synthetic", RunOutcome::Passed, Vec::new(), None, None);
        apply_provenance_policy(
            &mut document,
            &RuntimeProvenance::synthetic_with_trust(true),
        );

        assert_eq!(document["qualification_status"], "passed");
        assert_eq!(document_exit_code(&document), 0);
        assert_eq!(document["provenance"]["trusted"], true);
    }

    #[test]
    fn pre_harness_interrupt_skips_discovery_and_action_admission() {
        let directory = TestDirectory::new("pre-harness-interrupt");
        let mut reservation = ArtifactReservation::begin_journaled(
            "handshake",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["handshake"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        let discovery_calls = Arc::new(AtomicUsize::new(0));
        let runner_calls = Arc::new(AtomicUsize::new(0));
        let discovery: Arc<dyn DeviceDiscovery> = Arc::new(ScriptedDeviceDiscovery {
            result: Ok(vec![
                SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM8").expect("descriptor"),
            ]),
            calls: Arc::clone(&discovery_calls),
        });
        let arguments = vec![
            "handshake".to_owned(),
            "--port".to_owned(),
            "COM8".to_owned(),
            "--expected-identity".to_owned(),
            "DEVICE\\EXPECTED".to_owned(),
        ];
        let mut port = ScriptedOperatorPort::new([]);
        let token = InterruptToken::default();
        token.request();
        let observed_runner_calls = Arc::clone(&runner_calls);
        let result = {
            let mut control = RunControl::new(&mut reservation, &mut port, token);
            run_with_device_admission_controlled_using(
                "handshake",
                &arguments,
                discovery,
                &mut control,
                move |_, _, _| {
                    observed_runner_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(json!({"unexpected": true}))
                },
            )
            .expect("cancelled admission")
        };

        assert_eq!(discovery_calls.load(Ordering::SeqCst), 0);
        assert_eq!(runner_calls.load(Ordering::SeqCst), 0);
        assert_eq!(result["identity_admission"]["status"], "not_run");
        assert_eq!(result["operator_terminal_outcome"], "interrupted");
        assert_eq!(result["resources"], json!({}));
        assert!(cleanup_contract_succeeded("handshake", &result));
        let events = journal::parse_journal(
            &fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal"),
        )
        .expect("events");
        assert_eq!(events[1]["event"], "interrupt_requested");
        assert_eq!(events[2]["event"], "cancellation_terminal");
        let (document, failure) = finalize_result("handshake", Ok(result));
        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "cancelled");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 130);
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
    fn admitted_identity_is_durable_before_the_runner_is_called() {
        let calls = Arc::new(AtomicUsize::new(0));
        let trace = Arc::new(Mutex::new(Vec::<Value>::new()));
        let recorder_trace = Arc::clone(&trace);
        let runner_trace = Arc::clone(&trace);
        let arguments = vec![
            "handshake".to_owned(),
            "--port".to_owned(),
            "COM8".to_owned(),
            "--expected-identity".to_owned(),
            "DEVICE\\EXPECTED".to_owned(),
        ];

        let result = run_with_device_admission_core(
            "handshake",
            &arguments,
            Arc::new(ScriptedDeviceDiscovery {
                result: Ok(vec![
                    SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM8").expect("descriptor"),
                ]),
                calls,
            }),
            &mut (),
            move |(), payload| {
                recorder_trace.lock().expect("trace").push(payload);
                Ok(())
            },
            move |_, _, ()| {
                assert_eq!(runner_trace.lock().expect("trace").len(), 1);
                Ok(json!({"runner": "called"}))
            },
        )
        .expect("journaled admission");

        assert_eq!(result["runner"], "called");
        let trace = trace.lock().expect("trace");
        assert_eq!(trace[0]["identity_admission"]["status"], "admitted");
        assert_eq!(
            trace[0]["device_target"]["expected_stable_id"],
            "DEVICE\\EXPECTED"
        );
    }

    #[test]
    fn normalized_journal_arguments_do_not_retain_machine_paths() {
        let arguments = vec![
            "checkpoint".to_owned(),
            "--data".to_owned(),
            r"C:\private\payload.bin".to_owned(),
            "--output-dir".to_owned(),
            r"D:\private\run".to_owned(),
            "--runs-root".to_owned(),
            r"E:\private\runs".to_owned(),
            "--attestations-root".to_owned(),
            r"F:\private\attestations".to_owned(),
            "--slot".to_owned(),
            "1".to_owned(),
        ];

        let normalized = normalized_arguments(&arguments);

        assert_eq!(normalized[2], "<redacted-path>");
        assert_eq!(normalized[4], "<redacted-path>");
        assert_eq!(normalized[6], "<redacted-path>");
        assert_eq!(normalized[8], "<redacted-path>");
        assert_eq!(normalized[10], "1");
        let text = serde_json::to_string(&normalized).expect("normalized arguments");
        assert!(!text.contains("private"));
    }

    fn scripted_smoke_observation(
        outcome: OperatorOutcome,
    ) -> (Value, Vec<Value>, ScriptedOperatorPort, Vec<Duration>) {
        let directory = TestDirectory::new(outcome.as_str());
        let mut reservation = ArtifactReservation::begin_journaled(
            "smoke",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["smoke"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        let mut port = ScriptedOperatorPort::new([outcome]);
        let harness = observed_harness(Box::new(NoopTransport), "OPERATOR");
        harness
            .connect(ConnectOptions::default())
            .expect("synthetic connect");
        let expected_stable_id = harness.descriptor.stable_id().to_owned();
        let specs = smoke_observation_specs(false);
        let mut result = json!({
            "command": "smoke",
            "stage": "a_only",
            "device_target": {"expected_stable_id": expected_stable_id},
            "identity_admission": {
                "status": "admitted",
                "observed_expected": {"stable_id": expected_stable_id},
            },
            "required_operator_action_ids": ["button.A"],
            "operator_observations": [],
            "actions": [],
            "resources": {"primary": {"created": true}},
        });
        let delay = FakeDelay::default();
        {
            let mut control =
                RunControl::new(&mut reservation, &mut port, InterruptToken::default());
            let actual = exercise_observed_action_unit(
                &harness,
                &mut control,
                "smoke",
                &expected_stable_id,
                &expected_stable_id,
                &specs[0],
                100,
                Duration::from_secs(7),
                &mut result,
                &delay,
            )
            .unwrap_or_else(|failure| {
                panic!(
                    "scripted observation unit: {}: {}",
                    failure.stage, failure.message
                )
            });
            assert_eq!(actual, outcome);
        }
        result["operator_terminal_outcome"] = json!(if outcome == OperatorOutcome::Yes {
            "all_yes"
        } else {
            outcome.as_str()
        });
        result["final_snapshot"] = snapshot_json(&harness.controller);
        result["latency"] = latency_json(&harness.telemetry, harness.actual_baud(), None);
        result["cleanup"] = harness.close();
        let journal = fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal");
        let events = journal::parse_journal(&journal).expect("journal events");
        let waits = delay
            .waits
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        (result, events, port, waits)
    }

    #[test]
    fn operator_outcomes_map_to_exact_status_and_durable_events() {
        for (outcome, execution, qualification, exit_code) in [
            (OperatorOutcome::Yes, "completed", "passed", 0),
            (OperatorOutcome::No, "completed", "failed", 1),
            (OperatorOutcome::Eof, "completed", "unverified", 2),
            (OperatorOutcome::Timeout, "completed", "unverified", 2),
            (OperatorOutcome::Ambiguous, "completed", "unverified", 2),
            (OperatorOutcome::Interrupted, "cancelled", "unverified", 130),
        ] {
            let (result, events, port, waits) = scripted_smoke_observation(outcome);
            assert_eq!(port.announced.len(), 1);
            assert_eq!(port.observed.len(), 1);
            assert_eq!(port.announced[0].action_id, "button.A");
            assert_eq!(
                waits,
                [Duration::from_millis(50), Duration::from_millis(50)]
            );
            assert_eq!(result["actions"].as_array().map(Vec::len), Some(3));
            assert_eq!(
                result["operator_observations"][0]["response_timeout_ms"],
                7_000
            );
            let event_names = events
                .iter()
                .map(|event| event["event"].as_str().expect("event"))
                .collect::<Vec<_>>();
            if outcome == OperatorOutcome::Interrupted {
                assert_eq!(
                    &event_names[1..],
                    [
                        "interrupt_requested",
                        "operator_observation",
                        "cancellation_terminal",
                    ]
                );
            } else {
                assert_eq!(&event_names[1..], ["operator_observation"]);
            }
            let (document, _) = finalize_result("smoke", Ok(result));
            assert_eq!(document["execution_status"], execution);
            assert_eq!(document["qualification_status"], qualification);
            assert_eq!(document_exit_code(&document), exit_code);
        }
    }

    #[test]
    fn observation_plan_rejects_blanket_and_misbound_evidence() {
        let arguments = vec!["smoke".to_owned(), "--yes".to_owned()];
        assert!(operator_timing(&arguments, None).is_err());

        let full = smoke_observation_specs(true);
        let unique = full
            .iter()
            .map(|spec| spec.action_id.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique.len(), full.len());
        assert!(full.len() > Button::ALL.len());

        let (mut result, _, _, _) = scripted_smoke_observation(OperatorOutcome::Yes);
        result["operator_observations"][0]["action_id"] = json!("button.B");
        let (document, _) = finalize_result("smoke", Ok(result));
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn interrupt_cancels_one_owned_operation_and_preserves_the_unique_terminal() {
        for success_wins in [false, true] {
            let directory = TestDirectory::new(if success_wins {
                "interrupt-success-race"
            } else {
                "interrupt-cancel"
            });
            let mut reservation = ArtifactReservation::begin_journaled(
                "handshake",
                &directory.0,
                RunStartMetadata {
                    normalized_arguments: json!(["handshake"]),
                    provenance: json!({"trusted": true}),
                },
            )
            .expect("journaled reservation");
            let mut port = ScriptedOperatorPort::new([]);
            let token = InterruptToken::default();
            let runtime = Runtime::new(Arc::new(SystemClock::default()));
            let operation = runtime.create_operation(None).expect("operation");
            assert_eq!(operation.start(), TransitionOutcome::Applied);
            if success_wins {
                assert_eq!(
                    operation.succeed(OperationValue::Unit),
                    TransitionOutcome::Applied
                );
            } else {
                let completing = operation.clone();
                operation.on_cancel(move || {
                    assert_eq!(completing.finish_cancelled(), TransitionOutcome::Applied);
                });
            }
            token.request();
            let failure = {
                let mut control = RunControl::new(&mut reservation, &mut port, token);
                match wait_for_command_terminal_controlled(
                    operation.clone(),
                    &mut control,
                    Duration::from_secs(1),
                    "interrupt_wait",
                ) {
                    Ok(_) => panic!("interrupt must stop the wait"),
                    Err(failure) => failure,
                }
            };
            assert_eq!(failure.kind, CommandFailureKind::Interrupted);
            let snapshot = operation.snapshot();
            assert_eq!(
                snapshot.state,
                if success_wins {
                    OperationState::Succeeded
                } else {
                    OperationState::Cancelled
                }
            );
            if !success_wins {
                assert_eq!(
                    snapshot.cancellation_reason,
                    Some(CancellationReason::Requested)
                );
            }
            let events = journal::parse_journal(
                &fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal"),
            )
            .expect("events");
            assert_eq!(events[1]["event"], "interrupt_requested");
            assert_eq!(events[2]["event"], "cancellation_terminal");
            assert_eq!(
                events[2]["payload"]["request_outcome"],
                if success_wins {
                    "AlreadyTerminal"
                } else {
                    "Applied"
                }
            );
            assert_eq!(runtime.close(), Ok(CloseOutcome::Closed));
        }
    }

    #[test]
    fn nonneutral_interrupt_closes_cleanly_before_cancelled_artifact_commit() {
        let directory = TestDirectory::new("interrupt-action-window");
        let mut reservation = ArtifactReservation::begin_journaled(
            "smoke",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["smoke"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        let mut port = ScriptedOperatorPort::new([]);
        let token = InterruptToken::default();
        let delay = InterruptingDelay {
            token: token.clone(),
            calls: AtomicUsize::new(0),
        };
        let harness = observed_harness(Box::new(NoopTransport), "INTERRUPT-WINDOW");
        let expected_stable_id = harness.descriptor.stable_id().to_owned();
        let spec = smoke_observation_specs(false).remove(0);
        let result = json!({
            "command": "smoke",
            "stage": "a_only",
            "device_target": {"expected_stable_id": expected_stable_id},
            "identity_admission": {
                "status": "admitted",
                "observed_expected": {"stable_id": expected_stable_id},
            },
            "required_operator_action_ids": ["button.A"],
            "operator_observations": [],
            "actions": [],
            "resources": {"primary": {"created": true}},
        });
        let result = {
            let mut control = RunControl::new(&mut reservation, &mut port, token);
            finish_harness_phase(
                harness,
                result,
                "cleanup",
                "harness_evidence",
                "smoke_close",
                |harness, result| {
                    result["connect_operation"] = connect_for_command_controlled(
                        harness,
                        &mut control,
                        ConnectOptions::default(),
                        "smoke_connect_admit",
                        "smoke_connect_wait",
                        "smoke_connect_terminal",
                    )?;
                    exercise_observed_action_unit(
                        harness,
                        &mut control,
                        "smoke",
                        &expected_stable_id,
                        &expected_stable_id,
                        &spec,
                        100,
                        Duration::from_secs(1),
                        result,
                        &delay,
                    )?;
                    Ok(())
                },
            )
        };
        assert_eq!(delay.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result["actions"].as_array().map(Vec::len), Some(1));
        assert_eq!(result["operator_terminal_outcome"], "interrupted");
        assert!(cleanup_succeeded(&result["cleanup"]));
        assert_eq!(
            result["cleanup"]["controller"]["neutralization"],
            "accepted"
        );

        let execution = Ok(result.clone());
        reservation
            .record_event(
                JournalEventKind::OperationTerminal,
                execution_journal_payload("smoke", &execution),
            )
            .expect("operation terminal");
        reservation
            .record_event(
                JournalEventKind::CleanupTerminal,
                cleanup_journal_payload("smoke", &execution),
            )
            .expect("cleanup terminal");
        let (mut document, _) = finalize_result("smoke", execution);
        let ended_unix_ns = current_unix_ns().expect("end time");
        document["run"] = reservation.run_identity_json(ended_unix_ns);
        document["auxiliary_artifacts"] = json!([]);
        reservation
            .record_event(
                JournalEventKind::RunProjectionFinalized,
                json!({"ended_unix_ns": ended_unix_ns, "document": document.clone()}),
            )
            .expect("projection");
        reservation
            .seal_journal(json!({"primary_artifact": "smoke.json"}))
            .expect("seal");
        document["run"]["journal_projection"] =
            reservation.journal_projection().expect("projection");
        reservation.commit(&document).expect("cancelled artifact");

        let saved: Value = serde_json::from_slice(
            &fs::read(directory.0.join("smoke.json")).expect("saved artifact"),
        )
        .expect("saved JSON");
        assert_eq!(saved["execution_status"], "cancelled");
        assert_eq!(saved["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&saved), 130);
        let events = journal::parse_journal(
            &fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal"),
        )
        .expect("events");
        let event_names = events
            .iter()
            .map(|event| event["event"].as_str().expect("event"))
            .collect::<Vec<_>>();
        assert_eq!(
            &event_names[1..],
            [
                "interrupt_requested",
                "cancellation_terminal",
                "operation_terminal",
                "cleanup_terminal",
                "run_projection_finalized",
                "artifact_finalization_started",
            ]
        );
    }

    #[test]
    fn cleanup_failure_overrides_an_interrupted_action_outcome() {
        let directory = TestDirectory::new("interrupt-cleanup-failure");
        let mut reservation = ArtifactReservation::begin_journaled(
            "smoke",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["smoke"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        let mut port = ScriptedOperatorPort::new([]);
        let token = InterruptToken::default();
        let delay = InterruptingDelay {
            token: token.clone(),
            calls: AtomicUsize::new(0),
        };
        let harness = observed_harness(
            Box::new(FinalNeutralFailureTransport),
            "INTERRUPT-CLEANUP-FAILURE",
        );
        let expected_stable_id = harness.descriptor.stable_id().to_owned();
        let spec = smoke_observation_specs(false).remove(0);
        let result = json!({
            "command": "smoke",
            "stage": "a_only",
            "device_target": {"expected_stable_id": expected_stable_id},
            "identity_admission": {
                "status": "admitted",
                "observed_expected": {"stable_id": expected_stable_id},
            },
            "required_operator_action_ids": ["button.A"],
            "operator_observations": [],
            "actions": [],
            "resources": {"primary": {"created": true}},
        });
        let result = {
            let mut control = RunControl::new(&mut reservation, &mut port, token);
            finish_harness_phase(
                harness,
                result,
                "cleanup",
                "harness_evidence",
                "smoke_close",
                |harness, result| {
                    result["connect_operation"] = connect_for_command_controlled(
                        harness,
                        &mut control,
                        ConnectOptions::default(),
                        "smoke_connect_admit",
                        "smoke_connect_wait",
                        "smoke_connect_terminal",
                    )?;
                    exercise_observed_action_unit(
                        harness,
                        &mut control,
                        "smoke",
                        &expected_stable_id,
                        &expected_stable_id,
                        &spec,
                        100,
                        Duration::from_secs(1),
                        result,
                        &delay,
                    )?;
                    Ok(())
                },
            )
        };

        assert_eq!(delay.calls.load(Ordering::SeqCst), 1);
        assert_eq!(result["execution_error"]["stage"], "smoke_close");
        assert_eq!(result["cleanup_errors"][0]["stage"], "smoke_close");
        assert!(result.get("operator_terminal_outcome").is_none());
        assert!(!cleanup_succeeded(&result["cleanup"]));
        assert_eq!(
            result["cleanup"]["controller"]["neutralization"],
            "not_delivered"
        );
        let (document, failure) = finalize_result("smoke", Ok(result));
        assert_eq!(
            failure.as_deref(),
            Some("smoke_close: cleanup did not complete")
        );
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn durable_terminal_events_preserve_failure_cancellation_and_cleanup() {
        let directory = TestDirectory::new("durable-terminal-events");
        let mut cleanup = successful_cleanup();
        cleanup["succeeded"] = json!(false);
        cleanup["runtime"]["succeeded"] = json!(false);
        cleanup["runtime"]["outcome"] = json!("Failed");
        let execution = Ok(json!({
            "command": "handshake",
            "execution_error": {
                "stage": "action_terminal",
                "message": "injected action failure",
            },
            "cancelled_operation": {
                "state": "Cancelled",
                "cancellation_reason": "Requested",
            },
            "resources": {"primary": {"created": true}},
            "cleanup": cleanup,
        }));
        let mut reservation = ArtifactReservation::begin_journaled(
            "handshake",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["handshake"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        reservation
            .record_event(
                JournalEventKind::OperationTerminal,
                execution_journal_payload("handshake", &execution),
            )
            .expect("operation terminal");
        reservation
            .record_event(
                JournalEventKind::CleanupTerminal,
                cleanup_journal_payload("handshake", &execution),
            )
            .expect("cleanup terminal");

        let bytes = fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal");
        let events = journal::parse_journal(&bytes).expect("events");
        assert_eq!(events[1]["event"], "operation_terminal");
        assert_eq!(
            events[1]["payload"]["result"]["execution_error"]["stage"],
            "action_terminal"
        );
        assert_eq!(
            events[1]["payload"]["result"]["cancelled_operation"]["cancellation_reason"],
            "Requested"
        );
        assert_eq!(events[2]["event"], "cleanup_terminal");
        assert_eq!(events[2]["payload"]["cleanup_contract_succeeded"], false);
        assert_eq!(
            events[2]["payload"]["result"]["cleanup"]["runtime"]["outcome"],
            "Failed"
        );
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
    fn hotplug_poll_rebinds_new_port_and_records_the_old_port_conflict() {
        let request = DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request");
        let expected_old =
            || SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM8").expect("expected old");
        let other_old = || SerialPortDescriptor::new("DEVICE\\OTHER", "COM8").expect("other old");
        let expected_new =
            || SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM11").expect("expected new");
        let calls = Arc::new(AtomicUsize::new(0));
        let discovery = SequencedDeviceDiscovery {
            results: Mutex::new(
                vec![
                    Ok(vec![expected_old()]),
                    Ok(vec![other_old()]),
                    Ok(vec![other_old()]),
                    Ok(vec![other_old(), expected_new()]),
                ]
                .into(),
            ),
            calls: Arc::clone(&calls),
        };

        let mut absence_timer = FakeHotplugTimer::default();
        let absence = poll_hotplug_identity(
            &discovery,
            &request,
            HotplugExpectedState::Absent,
            Duration::from_secs(1),
            &mut absence_timer,
        );
        assert!(matches!(
            absence.outcome,
            HotplugPollOutcome::ExpectedAbsent
        ));
        assert_eq!(absence.observations.len(), 2);
        assert_eq!(absence_timer.waits, [HOTPLUG_POLL_INTERVAL]);
        assert_eq!(
            absence.observations[1]
                .hint()
                .map(SerialPortDescriptor::stable_id),
            Some("DEVICE\\OTHER")
        );

        let mut return_timer = FakeHotplugTimer::default();
        let returned = poll_hotplug_identity(
            &discovery,
            &request,
            HotplugExpectedState::Present,
            Duration::from_secs(1),
            &mut return_timer,
        );
        let returned_json = hotplug_poll_trace_json(&returned);
        let HotplugPollOutcome::Rebound(target) = returned.outcome else {
            panic!("expected identity must return");
        };
        assert_eq!(target.descriptor().port_name(), "COM11");
        assert_eq!(target.request().initial_port_hint(), "COM8");
        assert_eq!(return_timer.waits, [HOTPLUG_POLL_INTERVAL]);
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        assert_eq!(returned_json["outcome"], "expected_present");
        assert_eq!(
            returned_json["observations"][1]["observed_hint"]["stable_id"],
            "DEVICE\\OTHER"
        );
        assert_eq!(
            returned_json["observations"][1]["observed_expected"]["port"],
            "COM11"
        );
    }

    #[test]
    fn hotplug_poll_timeout_error_and_ambiguity_are_deterministic() {
        let request = DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request");
        let expected = || SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM8").expect("expected");
        let calls = Arc::new(AtomicUsize::new(0));
        let timeout_discovery = SequencedDeviceDiscovery {
            results: Mutex::new(vec![Ok(vec![expected()]), Ok(vec![expected()])].into()),
            calls: Arc::clone(&calls),
        };
        let mut timer = FakeHotplugTimer::default();
        let timed_out = poll_hotplug_identity(
            &timeout_discovery,
            &request,
            HotplugExpectedState::Absent,
            Duration::from_millis(500),
            &mut timer,
        );
        assert!(matches!(timed_out.outcome, HotplugPollOutcome::TimedOut));
        assert_eq!(timed_out.observations.len(), 2);
        assert_eq!(timer.waits, [HOTPLUG_POLL_INTERVAL, HOTPLUG_POLL_INTERVAL]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let discovery_error = SerialError::with_os_code(
            easycon_serial::SerialErrorKind::Io,
            "injected hotplug discovery failure",
            31,
        );
        let error_discovery = SequencedDeviceDiscovery {
            results: Mutex::new(vec![Err(discovery_error.clone())].into()),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let failed = poll_hotplug_identity(
            &error_discovery,
            &request,
            HotplugExpectedState::Present,
            Duration::from_secs(1),
            &mut FakeHotplugTimer::default(),
        );
        assert!(matches!(
            &failed.outcome,
            HotplugPollOutcome::DiscoveryError(error) if error == &discovery_error
        ));
        assert_eq!(
            hotplug_poll_trace_json(&failed)["structured_error"]["os_code"],
            31
        );

        let ambiguous_discovery = SequencedDeviceDiscovery {
            results: Mutex::new(
                vec![Ok(vec![
                    expected(),
                    SerialPortDescriptor::new("DEVICE\\EXPECTED", "COM11")
                        .expect("duplicate expected"),
                ])]
                .into(),
            ),
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let ambiguous = poll_hotplug_identity(
            &ambiguous_discovery,
            &request,
            HotplugExpectedState::Present,
            Duration::from_secs(1),
            &mut FakeHotplugTimer::default(),
        );
        assert!(matches!(
            ambiguous.outcome,
            HotplugPollOutcome::AmbiguousSnapshot
        ));
        assert_eq!(
            hotplug_poll_trace_json(&ambiguous)["outcome"],
            "ambiguous_snapshot"
        );
    }

    #[test]
    fn hotplug_timeouts_are_completed_failures_when_initial_cleanup_succeeds() {
        for kind in ["expected_absent_timeout", "expected_return_timeout"] {
            let result = json!({
                "command": "hotplug",
                "disconnect_detected": kind == "expected_return_timeout",
                "stable_id": "DEVICE\\EXPECTED",
                "hotplug_machine_failure": {"kind": kind},
                "resources": {
                    "initial": {"created": true},
                    "reconnected": {"created": false},
                },
                "disconnect_cleanup": successful_cleanup(),
            });
            assert!(cleanup_contract_succeeded("hotplug", &result));
            let (document, failure) = finalize_result("hotplug", Ok(result));
            assert_eq!(document["execution_status"], "completed");
            assert_eq!(document["qualification_status"], "failed");
            assert!(failure.is_some());
            assert_eq!(document_exit_code(&document), 1);
        }
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
            assert_ne!(document["qualification_status"], "passed", "{command}");
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
                    "return_poll": {"outcome": "expected_present"},
                    "rebound_admission": {
                        "status": "rebound",
                        "observed_expected": {"stable_id": "DEVICE\\EXPECTED"},
                    },
                    "hotplug_transitions": [
                        {"sequence": 1, "state": "requested"},
                        {"sequence": 2, "state": "initial_admitted"},
                        {"sequence": 3, "state": "initial_connected"},
                        {"sequence": 4, "state": "awaiting_expected_absent"},
                        {"sequence": 5, "state": "initial_closed"},
                        {"sequence": 6, "state": "awaiting_expected_return"},
                        {"sequence": 7, "state": "rebound_admitted"},
                        {"sequence": 8, "state": "reconnected"},
                        {"sequence": 9, "state": "closed"},
                    ],
                    "resources": {
                        "initial": {"created": true},
                        "reconnected": {"created": true},
                    },
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
                "latency": passing_latency(),
                "cleanup": successful_cleanup(),
            })),
        );
        assert_eq!(smoke["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&smoke), 2);

        let (home, _) = finalize_result(
            "home-wake",
            Ok(json!({
                "final_snapshot": {"desired_report_neutral": true},
                "switch_observation": "requires operator confirmation",
                "latency": passing_latency(),
                "cleanup": successful_cleanup(),
            })),
        );
        assert_eq!(home["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&home), 2);

        let (amiibo, _) = finalize_result(
            "amiibo",
            Ok(json!({"write_performed": false, "capability": "unknown"})),
        );
        assert_eq!(amiibo["qualification_status"], "not_run");
        assert_eq!(document_exit_code(&amiibo), 2);
    }

    fn authorized_amiibo_arguments(data_path: &Path, expected_sha256: &str) -> Vec<String> {
        vec![
            "amiibo".to_owned(),
            "--port".to_owned(),
            "COM8".to_owned(),
            "--expected-identity".to_owned(),
            "DEVICE\\EXPECTED".to_owned(),
            "--slot".to_owned(),
            "3".to_owned(),
            "--disposable-slot".to_owned(),
            "3".to_owned(),
            "--slot-count".to_owned(),
            "8".to_owned(),
            "--maximum-data-len".to_owned(),
            "64".to_owned(),
            "--limits-source".to_owned(),
            "operator-attestation:test-fixture".to_owned(),
            "--data".to_owned(),
            data_path.to_string_lossy().into_owned(),
            "--expected-sha256".to_owned(),
            expected_sha256.to_owned(),
            "--authorize-write".to_owned(),
        ]
    }

    #[test]
    fn amiibo_authorization_binds_identity_slot_limits_and_payload_hash() {
        let directory = TestDirectory::new("amiibo-authorization-valid");
        let data_path = directory.0.join("payload.bin");
        let payload = b"qualification-only-amiibo-payload";
        fs::write(&data_path, payload).expect("payload");
        let expected_sha256 = sha256_bytes(payload);
        let arguments = authorized_amiibo_arguments(&data_path, &expected_sha256);

        let AmiiboCommand::Authorized(authorization) =
            prepare_amiibo_command(&arguments, "lease-amiibo").expect("authorization")
        else {
            panic!("write flag must produce an authorization");
        };
        assert_eq!(authorization.lease_id, "lease-amiibo");
        assert_eq!(authorization.expected_stable_id, "DEVICE\\EXPECTED");
        assert_eq!(authorization.slot, 3);
        assert_eq!(authorization.disposable_slot, 3);
        assert_eq!(authorization.slot_count, 8);
        assert_eq!(authorization.maximum_data_len, 64);
        assert_eq!(authorization.payload.as_ref(), payload);
        assert_eq!(authorization.expected_sha256, expected_sha256);
        assert_eq!(authorization.recomputed_sha256, expected_sha256);
        assert_eq!(
            authorization.limits_source,
            "operator-attestation:test-fixture"
        );
    }

    #[test]
    fn amiibo_authorization_rejects_every_unbound_or_unverified_input() {
        let directory = TestDirectory::new("amiibo-authorization-invalid");
        let data_path = directory.0.join("payload.bin");
        let payload = b"qualification-only-amiibo-payload";
        fs::write(&data_path, payload).expect("payload");
        let expected_sha256 = sha256_bytes(payload);
        let valid = authorized_amiibo_arguments(&data_path, &expected_sha256);
        let option_index = |arguments: &[String], option: &str| {
            arguments
                .iter()
                .position(|argument| argument == option)
                .expect("option")
        };

        let mut invalid = Vec::new();
        let mut duplicate_authorization = valid.clone();
        duplicate_authorization.push("--authorize-write".to_owned());
        invalid.push(duplicate_authorization);

        let mut blanket_disposable = valid.clone();
        blanket_disposable.push("--confirm-disposable".to_owned());
        invalid.push(blanket_disposable);

        let mut mismatched_slot = valid.clone();
        let index = option_index(&mismatched_slot, "--disposable-slot");
        mismatched_slot[index + 1] = "4".to_owned();
        invalid.push(mismatched_slot);

        let mut missing_source = valid.clone();
        let index = option_index(&missing_source, "--limits-source");
        missing_source.drain(index..=index + 1);
        invalid.push(missing_source);

        let mut absolute_source = valid.clone();
        let index = option_index(&absolute_source, "--limits-source");
        absolute_source[index + 1] = directory
            .0
            .join("capacity.txt")
            .to_string_lossy()
            .into_owned();
        invalid.push(absolute_source);

        let mut malformed_hash = valid.clone();
        let index = option_index(&malformed_hash, "--expected-sha256");
        malformed_hash[index + 1] = "not-a-sha256".to_owned();
        invalid.push(malformed_hash);

        let mut mismatched_hash = valid.clone();
        let index = option_index(&mismatched_hash, "--expected-sha256");
        mismatched_hash[index + 1] = "0".repeat(64);
        invalid.push(mismatched_hash);

        let mut slot_out_of_range = valid.clone();
        let index = option_index(&slot_out_of_range, "--slot-count");
        slot_out_of_range[index + 1] = "3".to_owned();
        invalid.push(slot_out_of_range);

        let mut length_out_of_range = valid.clone();
        let index = option_index(&length_out_of_range, "--maximum-data-len");
        length_out_of_range[index + 1] = "4".to_owned();
        invalid.push(length_out_of_range);

        let empty_path = directory.0.join("empty.bin");
        fs::write(&empty_path, []).expect("empty payload");
        invalid.push(authorized_amiibo_arguments(&empty_path, &sha256_bytes(&[])));

        for arguments in invalid {
            assert!(
                prepare_amiibo_command(&arguments, "lease-amiibo").is_err(),
                "unsafe authorization was accepted: {arguments:?}"
            );
        }
    }

    #[test]
    fn amiibo_without_write_flag_remains_not_run_without_reading_payload() {
        let directory = TestDirectory::new("amiibo-unauthorized-missing-payload");
        let arguments = vec![
            "amiibo".to_owned(),
            "--data".to_owned(),
            directory
                .0
                .join("does-not-exist.bin")
                .to_string_lossy()
                .into_owned(),
        ];
        assert!(matches!(
            prepare_amiibo_command(&arguments, "lease-amiibo"),
            Ok(AmiiboCommand::Unauthorized)
        ));
    }

    #[test]
    fn amiibo_runner_journals_chunks_save_select_and_cleanup_in_order() {
        let directory = TestDirectory::new("amiibo-runner-success");
        let mut reservation = ArtifactReservation::begin_journaled(
            "amiibo",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["amiibo"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        let lease_id = reservation.lease_id().to_owned();
        let writer = reservation.journal_writer().expect("journal writer");
        let payload = Arc::<[u8]>::from([0x42_u8; 23]);
        let recorder = AmiiboEvidenceRecorder::default();
        let harness = observed_amiibo_harness(
            Box::new(SuccessfulAmiiboTransport),
            writer,
            recorder.clone(),
            &lease_id,
            "AMIIBO-SUCCESS",
            &payload,
        );
        let stable_id = harness.descriptor.stable_id().to_owned();
        let result = amiibo_execution_result(&lease_id, &stable_id, &payload);
        let mut port = ScriptedOperatorPort::new([]);
        let mut result = {
            let mut control =
                RunControl::new(&mut reservation, &mut port, InterruptToken::default());
            finish_harness_phase(
                harness,
                result,
                "cleanup",
                "harness_evidence",
                "amiibo_close",
                |harness, result| execute_amiibo(harness, &mut control, 3, payload, result),
            )
        };

        assert_eq!(result["write_intent_durable"], true);
        assert_eq!(result["save_terminal_durable"], true);
        assert_eq!(result["select_intent_durable"], true);
        assert_eq!(result["select_terminal_durable"], true);
        assert_eq!(result["write_performed"], true);
        assert_eq!(result["select_performed"], true);
        assert!(cleanup_succeeded(&result["cleanup"]));
        let projection = recorder.projection();
        assert_eq!(projection["chunks"].as_array().map(Vec::len), Some(2));
        assert_eq!(projection["chunks"][0]["status"], "acked");
        assert_eq!(projection["chunks"][1]["status"], "acked");
        assert_eq!(projection["selects"][0]["status"], "acked");
        assert_eq!(projection["transport_closed_in_state"], "complete");
        result["amiibo_protocol_evidence"] = projection;

        let events = journal::parse_journal(
            &fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal"),
        )
        .expect("events");
        let names = events
            .iter()
            .map(|event| event["event"].as_str().expect("event"))
            .collect::<Vec<_>>();
        let write_intent = names
            .iter()
            .position(|event| *event == "amiibo_write_intent")
            .expect("write intent");
        let first_chunk_intent = names
            .iter()
            .position(|event| *event == "amiibo_chunk_intent")
            .expect("chunk intent");
        let save_terminal = names
            .iter()
            .position(|event| *event == "amiibo_save_terminal")
            .expect("save terminal");
        let first_select_intent = names
            .iter()
            .position(|event| *event == "amiibo_select_intent")
            .expect("select intent");
        assert!(write_intent < first_chunk_intent);
        assert!(save_terminal < first_select_intent);
        assert_eq!(
            names
                .iter()
                .filter(|event| **event == "amiibo_chunk_terminal")
                .count(),
            2
        );
        assert_eq!(
            names
                .iter()
                .filter(|event| **event == "amiibo_select_intent")
                .count(),
            2
        );
        assert_eq!(
            names
                .iter()
                .filter(|event| **event == "amiibo_select_terminal")
                .count(),
            2
        );
        let (document, failure) = finalize_result("amiibo", Ok(result));
        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 2);
    }

    #[test]
    fn amiibo_cleanup_failures_preserve_destructive_progress_and_fail_closed() {
        let directory = TestDirectory::new("amiibo-cleanup-failure");
        let mut reservation = ArtifactReservation::begin_journaled(
            "amiibo",
            &directory.0,
            RunStartMetadata {
                normalized_arguments: json!(["amiibo"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journaled reservation");
        let lease_id = reservation.lease_id().to_owned();
        let writer = reservation.journal_writer().expect("journal writer");
        let payload = Arc::<[u8]>::from([0x31_u8; 20]);
        let recorder = AmiiboEvidenceRecorder::default();
        let harness = observed_amiibo_harness(
            Box::new(AmiiboFinalNeutralFailureTransport),
            writer,
            recorder.clone(),
            &lease_id,
            "AMIIBO-CLEANUP-FAILURE",
            &payload,
        );
        let resource: Arc<dyn ManagedResource> = Arc::new(PanickingResource);
        let _registration = harness
            .runtime
            .register_resource(resource.clone())
            .expect("failing resource");
        let stable_id = harness.descriptor.stable_id().to_owned();
        let result = amiibo_execution_result(&lease_id, &stable_id, &payload);
        let mut port = ScriptedOperatorPort::new([]);
        let mut result = {
            let mut control =
                RunControl::new(&mut reservation, &mut port, InterruptToken::default());
            finish_harness_phase(
                harness,
                result,
                "cleanup",
                "harness_evidence",
                "amiibo_close",
                |harness, result| execute_amiibo(harness, &mut control, 3, payload, result),
            )
        };
        result["amiibo_protocol_evidence"] = recorder.projection();

        assert_eq!(result["write_performed"], true);
        assert_eq!(result["select_performed"], true);
        assert_eq!(
            result["amiibo_protocol_evidence"]["chunks"][0]["status"],
            "acked"
        );
        assert_eq!(
            result["amiibo_protocol_evidence"]["selects"][0]["status"],
            "acked"
        );
        assert_eq!(
            result["cleanup"]["controller"]["neutralization"],
            "not_delivered"
        );
        assert_eq!(result["cleanup"]["runtime"]["outcome"], "Failed");
        assert_eq!(result["execution_error"]["stage"], "amiibo_close");
        assert!(!cleanup_succeeded(&result["cleanup"]));
        let names = journal::parse_journal(
            &fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal"),
        )
        .expect("events")
        .into_iter()
        .map(|event| event["event"].as_str().expect("event").to_owned())
        .collect::<Vec<_>>();
        assert!(names.contains(&"amiibo_chunk_terminal".to_owned()));
        assert!(names.contains(&"amiibo_save_terminal".to_owned()));
        assert_eq!(
            names
                .iter()
                .filter(|event| **event == "amiibo_select_terminal")
                .count(),
            2
        );

        let (document, failure) = finalize_result("amiibo", Ok(result));
        assert!(failure.is_some());
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
    }

    fn interrupt_after_ack_entry(
        entered: Receiver<()>,
        token: InterruptToken,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            entered.recv().expect("blocking ACK entered");
            token.request();
        })
    }

    #[test]
    fn amiibo_interrupt_during_save_or_select_preserves_progress_and_cleanup() {
        for (label, block_at_ack) in [("save", 2_usize), ("select", 3_usize)] {
            let directory = TestDirectory::new(&format!("amiibo-interrupt-{label}"));
            let mut reservation = ArtifactReservation::begin_journaled(
                "amiibo",
                &directory.0,
                RunStartMetadata {
                    normalized_arguments: json!(["amiibo"]),
                    provenance: json!({"trusted": true}),
                },
            )
            .expect("journaled reservation");
            let lease_id = reservation.lease_id().to_owned();
            let writer = reservation.journal_writer().expect("journal writer");
            let payload = Arc::<[u8]>::from([0x24_u8; 20]);
            let recorder = AmiiboEvidenceRecorder::default();
            let (entered, wait_entered) = std::sync::mpsc::sync_channel(0);
            let harness = observed_amiibo_harness(
                Box::new(BlockingAmiiboAckTransport {
                    block_at_ack,
                    ack_count: 0,
                    entered: Some(entered),
                }),
                writer,
                recorder.clone(),
                &lease_id,
                &format!("AMIIBO-INTERRUPT-{label}"),
                &payload,
            );
            let stable_id = harness.descriptor.stable_id().to_owned();
            let result = amiibo_execution_result(&lease_id, &stable_id, &payload);
            let mut port = ScriptedOperatorPort::new([]);
            let token = InterruptToken::default();
            let requester = interrupt_after_ack_entry(wait_entered, token.clone());
            let result = {
                let mut control = RunControl::new(&mut reservation, &mut port, token);
                finish_harness_phase(
                    harness,
                    result,
                    "cleanup",
                    "harness_evidence",
                    "amiibo_close",
                    |harness, result| execute_amiibo(harness, &mut control, 3, payload, result),
                )
            };
            requester.join().expect("interrupt requester");

            assert_eq!(
                result["operator_terminal_outcome"], "interrupted",
                "{label}"
            );
            assert!(cleanup_succeeded(&result["cleanup"]), "{label}");
            assert_eq!(result["save_terminal_durable"], true, "{label}");
            assert_eq!(result["write_performed"], label == "select", "{label}");
            assert_eq!(
                result["select_terminal_durable"],
                label == "select",
                "{label}"
            );
            let projection = recorder.projection();
            assert_eq!(
                projection["chunks"][0]["status"],
                if label == "save" { "failed" } else { "acked" }
            );
            assert_eq!(projection["cleanup_resets"][0]["status"], "acked");
            if label == "select" {
                assert_eq!(projection["selects"][0]["status"], "failed");
            }
            let events = journal::parse_journal(
                &fs::read(directory.0.join(journal::JOURNAL_FILE_NAME)).expect("journal"),
            )
            .expect("events");
            let names = events
                .iter()
                .map(|event| event["event"].as_str().expect("event"))
                .collect::<Vec<_>>();
            let interrupt = names
                .iter()
                .position(|event| *event == "interrupt_requested")
                .expect("interrupt event");
            let cleanup_terminal = names
                .iter()
                .position(|event| *event == "amiibo_cleanup_terminal")
                .expect("cleanup terminal");
            let cancellation = names
                .iter()
                .position(|event| *event == "cancellation_terminal")
                .expect("cancellation terminal");
            assert!(interrupt < cleanup_terminal && cleanup_terminal < cancellation);
            let (document, failure) = finalize_result("amiibo", Ok(result));
            assert!(failure.is_none());
            assert_eq!(document["execution_status"], "cancelled", "{label}");
            assert_eq!(document["qualification_status"], "unverified", "{label}");
            assert_eq!(document_exit_code(&document), 130, "{label}");
        }
    }

    #[test]
    fn amiibo_qualification_accepts_retry_evidence_but_rejects_contradictions() {
        let valid = valid_amiibo_qualification_result();
        let (document, failure) = finalize_result("amiibo", Ok(valid.clone()));
        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 2);

        let mut retried = valid.clone();
        retried["amiibo_protocol_evidence"]["chunks"]
            .as_array_mut()
            .expect("chunks")
            .insert(
                0,
                json!({
                    "operation_id": 1,
                    "offset": 0,
                    "length": 3,
                    "attempt": 1,
                    "header_write_sequence": 1,
                    "header_accepted_bytes": 7,
                    "header_acknowledged": false,
                    "payload_write_sequence": null,
                    "payload_accepted_bytes": 0,
                    "payload_acknowledged": false,
                    "status": "failed",
                    "error": {"kind": "Timeout", "message": "injected"},
                }),
            );
        retried["amiibo_protocol_evidence"]["chunks"][1]["attempt"] = json!(2);
        retried["amiibo_protocol_evidence"]["cleanup_resets"] = json!([{
            "operation_id": 1,
            "write_sequence": 2,
            "accepted_bytes": 6,
            "acknowledged": true,
            "resume": "save",
            "status": "acked",
            "error": null,
        }]);
        let (document, failure) = finalize_result("amiibo", Ok(retried));
        assert!(failure.is_none());
        assert_eq!(document["qualification_status"], "unverified");

        let mut invalid = Vec::new();
        let mut missing_authorization = valid.clone();
        missing_authorization["authorization"]["one_time"] = json!(false);
        invalid.push(missing_authorization);
        let mut hash_mismatch = valid.clone();
        hash_mismatch["payload"]["recomputed_sha256"] = json!("0".repeat(64));
        invalid.push(hash_mismatch);
        let mut chunk_gap = valid.clone();
        chunk_gap["amiibo_protocol_evidence"]["chunks"][0]["offset"] = json!(1);
        invalid.push(chunk_gap);
        let mut missing_select = valid.clone();
        missing_select["select_terminal_durable"] = json!(false);
        invalid.push(missing_select);
        let mut false_capability = valid.clone();
        false_capability["o_02_status"] = json!("closed");
        invalid.push(false_capability);
        let mut path_leak = valid;
        path_leak["payload"]["source"]["path_recorded"] = json!(true);
        invalid.push(path_leak);

        for result in invalid {
            let (document, failure) = finalize_result("amiibo", Ok(result));
            assert!(failure.is_some());
            assert_eq!(document["execution_status"], "completed");
            assert_eq!(document["qualification_status"], "failed");
            assert_eq!(document_exit_code(&document), 1);
        }
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
                "latency": passing_latency(),
                "cleanup": successful_cleanup(),
            })),
        );

        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 2);
    }

    #[test]
    fn telemetry_contradiction_cannot_pass_sequence_qualification() {
        let mut latency = passing_latency();
        latency["integrity"] = json!({
            "status": "failed",
            "errors": [{
                "code": "dispatch_after_write_entry",
                "message": "injected time reversal",
                "write_sequence": 7,
            }],
        });
        latency["logical_reports"]["attempt_count"] = json!(1);
        latency["logical_reports"]["contradiction_count"] = json!(1);
        latency["logical_reports"]["detail"]["rows"] = json!([{
            "write_sequence": 7,
            "outcome": "contradiction",
        }]);
        let (document, failure) = finalize_result(
            "sequence",
            Ok(json!({
                "requested_steps": 10_000,
                "functional_success": true,
                "accepted_report_count_before_final_reset": 10_000,
                "recorded_complete_writes": 10_001,
                "physical_order_evidence": "open: no logic analyzer or firmware trace",
                "latency": latency,
                "cleanup": successful_cleanup(),
            })),
        );

        assert!(failure.is_some());
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document["exit_code"], 1);
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
            identity_open_recorder: IdentityOpenRecorder::default(),
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
    fn home_diagnostic_delay_is_explicit_exclusive_and_bounded() {
        let arguments = |value: &str| {
            vec![
                "smoke".to_owned(),
                "--wake-home".to_owned(),
                "--post-home-neutral-delay-ms".to_owned(),
                value.to_owned(),
            ]
        };
        assert_eq!(smoke_home_delay(&arguments("0"), true), Ok(Some(0)));
        assert_eq!(
            smoke_home_delay(&arguments("60000"), true),
            Ok(Some(60_000))
        );
        assert!(smoke_home_delay(&arguments("60001"), true).is_err());
        assert!(smoke_home_delay(&["smoke".to_owned()], true).is_err());
        assert!(smoke_home_delay(&arguments("3000"), false).is_err());
        assert_eq!(smoke_home_delay(&["smoke".to_owned()], false), Ok(None));
    }

    #[test]
    fn diagnostic_prelude_records_real_action_order_around_the_fake_delay() {
        let mut result = json!({
            "actions": [],
            "diagnostic_prelude_steps": [],
        });
        let record = |result: &mut Value, label: &str| {
            result["actions"]
                .as_array_mut()
                .expect("actions")
                .push(json!({
                    "label": label,
                    "operation": {"state": "Succeeded"},
                }));
            record_last_smoke_action_in_diagnostic(result);
        };
        for (label, _) in home_wake_actions() {
            record(&mut result, label);
        }
        let delay = FakeDelay::default();
        record_configured_home_delay(&mut result, 3_000, &delay);
        for (label, _) in left_stick_wake_actions() {
            record(&mut result, label);
        }
        record(&mut result, "button.A.down");

        assert_eq!(
            delay.waits.lock().expect("fake delay").as_slice(),
            [Duration::from_secs(3)]
        );
        let steps = result["diagnostic_prelude_steps"]
            .as_array()
            .expect("diagnostic steps");
        assert_eq!(steps[0]["label"], "wake.Home.down");
        assert_eq!(steps[1]["label"], "wake.Home.up");
        assert_eq!(steps[2]["label"], "wake.Home.neutral");
        assert_eq!(steps[3]["kind"], "configured_wait");
        assert_eq!(steps[3]["configured_post_home_neutral_delay_ms"], 3_000);
        assert_eq!(steps[4]["label"], "wake.left_stick.right");
        assert_eq!(steps.last().expect("A action")["label"], "button.A.down");
        let projection = serde_json::to_string(&result).expect("projection");
        assert!(!projection.contains("measured"));
        assert!(!projection.contains("readiness"));
        assert!(!projection.contains("capability"));
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
    fn timing_csv_is_rendered_without_writing_an_artifact_path() {
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));

        let bytes = timing_csv_bytes(&telemetry).expect("render timing CSV");

        assert_eq!(
            bytes,
            format!("{}\n", telemetry::CSV_HEADER.join(",")).into_bytes()
        );
    }

    #[test]
    fn partial_report_keeps_the_first_entry_and_final_acceptance() {
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let clock: Arc<dyn Clock> = Arc::new(ScriptedNowClock::new([100, 110, 120, 130]));
        let mut observed = ObservedTransport {
            inner: Box::new(ThreeThenFiveTransport::default()),
            clock,
            telemetry: telemetry.clone(),
        };
        let context = WriteContext {
            resource_id: ResourceId::new(7),
            operation_id: Some(OperationId::new(11)),
            sequence: 13,
            timestamp_ns: 90,
            direct_timing: Some(DirectWriteTiming {
                command_admitted_ns: 70,
                lane_wake_ns: 80,
            }),
            total_len: 8,
            kind: WriteKind::Report,
        };
        let cancellation = CancellationToken::root();
        let resource_cancellation = CancellationToken::root();
        let bytes = [0_u8; 8];

        assert_eq!(
            observed.write(WriteRequest {
                context,
                bytes: &bytes,
                deadline_ns: u64::MAX,
                cancellation: cancellation.clone(),
                resource_cancellation: resource_cancellation.clone(),
            }),
            Ok(3)
        );
        assert_eq!(
            observed.write(WriteRequest {
                context,
                bytes: &bytes[3..],
                deadline_ns: u64::MAX,
                cancellation,
                resource_cancellation,
            }),
            Ok(5)
        );

        let telemetry = telemetry.lock().expect("telemetry");
        let projection = telemetry.logical_reports.projection_json(None, None);
        let row = &projection["logical_reports"]["detail"]["rows"][0];
        assert_eq!(row["first_write_entered_ns"], 100);
        assert_eq!(row["transport_accepted_ns"], 130);
        assert_eq!(row["partial_count"], 2);
        assert_eq!(row["operation_id"], 11);
        assert_eq!(row["write_sequence"], 13);
    }

    #[test]
    fn backwards_telemetry_is_an_explicit_integrity_failure() {
        let context = WriteContext {
            resource_id: ResourceId::new(1),
            operation_id: Some(OperationId::new(1)),
            sequence: 1,
            timestamp_ns: 30,
            direct_timing: Some(DirectWriteTiming {
                command_admitted_ns: 10,
                lane_wake_ns: 20,
            }),
            total_len: 8,
            kind: WriteKind::Report,
        };
        let mut reports = LogicalReportTelemetry::default();
        reports.begin(context, 8, 29);
        reports.finish(context, 8, 28, &Ok(8));
        let telemetry = Arc::new(Mutex::new(Telemetry {
            logical_reports: reports,
            ..Telemetry::default()
        }));

        let projection = latency_json(&telemetry, Some(115_200), None);

        assert_eq!(projection["integrity"]["status"], "failed");
    }

    #[test]
    fn invalid_terminal_status_pair_fails_closed() {
        let document = json!({
            "execution_status": "cancelled",
            "qualification_status": "failed",
            "exit_code": 130,
        });

        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn byte_io_decorator_preserves_native_error_before_lossy_mapping() {
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let factory = ObservedByteIoFactory {
            inner: Box::new(NativeFailureFactory),
            telemetry: telemetry.clone(),
        };
        let descriptor = SerialPortDescriptor::new("DEVICE\\NATIVE", "COM1").expect("descriptor");
        let serial = SerialControllerTransport::new(clock.clone(), descriptor, Box::new(factory));
        let mut observed = ObservedTransport {
            inner: Box::new(serial),
            clock,
            telemetry: telemetry.clone(),
        };
        let cancellation = CancellationToken::root();
        let resource_cancellation = CancellationToken::root();
        observed
            .handshake(HandshakeRequest {
                operation_id: OperationId::new(1),
                baud_rate: 115_200,
                request_bytes: [0xA5, 0xA5, 0x81],
                expected_reply: 0x80,
                deadline_ns: u64::MAX,
                cancellation: cancellation.clone(),
                resource_cancellation: resource_cancellation.clone(),
            })
            .expect("synthetic handshake");
        let context = WriteContext {
            resource_id: ResourceId::new(7),
            operation_id: Some(OperationId::new(11)),
            sequence: 13,
            timestamp_ns: 0,
            direct_timing: None,
            total_len: 8,
            kind: WriteKind::Report,
        };
        let bytes = [0_u8; 8];
        assert_eq!(
            observed.write(WriteRequest {
                context,
                bytes: &bytes,
                deadline_ns: u64::MAX,
                cancellation: cancellation.clone(),
                resource_cancellation: resource_cancellation.clone(),
            }),
            Ok(3)
        );
        let mapped = observed
            .write(WriteRequest {
                context,
                bytes: &bytes[3..],
                deadline_ns: u64::MAX,
                cancellation,
                resource_cancellation,
            })
            .expect_err("injected native failure");
        assert_eq!(mapped.kind(), TransportErrorKind::Disconnected);

        let projection = telemetry
            .lock()
            .expect("telemetry")
            .logical_reports
            .projection_json(None, None);
        let row = &projection["logical_reports"]["detail"]["rows"][0];
        assert_eq!(row["accepted_bytes"], 3);
        assert_eq!(row["partial_count"], 1);
        assert_eq!(row["transport_error"]["kind"], "Disconnected");
        assert_eq!(row["native_error"]["kind"], "Io");
        assert_eq!(row["native_error"]["os_code"], 995);
    }

    #[test]
    fn qualification_projection_fixture_matches_real_code() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../fixtures/phase2b-qualification-projection-v1.json"
        ))
        .expect("qualification projection fixture");
        assert_eq!(fixture["schema_version"], 1);

        for expected in fixture["outcomes"].as_array().expect("outcomes") {
            let outcome = match expected["name"].as_str().expect("outcome name") {
                "Passed" => RunOutcome::Passed,
                "QualificationFailed" => RunOutcome::QualificationFailed,
                "ExecutionFailed" => RunOutcome::ExecutionFailed,
                "NotRun" => RunOutcome::NotRun,
                "Unverified" => RunOutcome::Unverified,
                "Cancelled" => RunOutcome::Cancelled,
                other => panic!("unknown fixture outcome {other}"),
            };
            let document = final_document("fixture", outcome, Vec::new(), None, None);
            assert_eq!(document["schema_version"], 2);
            assert!(document.get("status").is_none());
            assert_eq!(document["execution_status"], expected["execution_status"]);
            assert_eq!(
                document["qualification_status"],
                expected["qualification_status"]
            );
            assert_eq!(document["exit_code"], expected["exit_code"]);
            assert_eq!(document_exit_code(&document), outcome.exit_code());
        }
        for invalid in fixture["invalid_status_pairs"]
            .as_array()
            .expect("invalid pairs")
        {
            assert_eq!(document_exit_code(invalid), 1);
        }

        assert_eq!(
            fixture["csv_header"],
            json!(telemetry::CSV_HEADER.as_slice())
        );
        let context = |dispatch_ns| WriteContext {
            resource_id: ResourceId::new(7),
            operation_id: Some(OperationId::new(11)),
            sequence: 13,
            timestamp_ns: dispatch_ns,
            direct_timing: Some(DirectWriteTiming {
                command_admitted_ns: 70,
                lane_wake_ns: 80,
            }),
            total_len: 8,
            kind: WriteKind::Report,
        };
        for expected in fixture["logical_report_cases"]
            .as_array()
            .expect("logical report cases")
        {
            let mut reports = LogicalReportTelemetry::default();
            let name = expected["name"].as_str().expect("case name");
            let context = context(90);
            match name {
                "single_8" => {
                    reports.begin(context, 8, 100);
                    reports.finish(context, 8, 110, &Ok(8));
                }
                "partial_3_5" => {
                    reports.begin(context, 8, 100);
                    reports.finish(context, 8, 110, &Ok(3));
                    reports.begin(context, 5, 120);
                    reports.finish(context, 5, 130, &Ok(5));
                }
                "partial_3_native_failure" => {
                    reports.begin(context, 8, 100);
                    reports.finish(context, 8, 110, &Ok(3));
                    reports.begin(context, 5, 120);
                    reports.record_native_error(
                        context,
                        &SerialError::with_os_code(
                            SerialErrorKind::Io,
                            "fixture native failure",
                            995,
                        ),
                    );
                    reports.finish(
                        context,
                        5,
                        130,
                        &Err(TransportError::new(
                            TransportErrorKind::Disconnected,
                            "fixture mapped failure",
                        )),
                    );
                }
                "time_reversal" => {
                    reports.begin(context, 8, 89);
                    reports.finish(context, 8, 88, &Ok(8));
                }
                other => panic!("unknown fixture telemetry case {other}"),
            }
            let projection = reports.projection_json(Some(115_200), None);
            let row = &projection["logical_reports"]["detail"]["rows"][0];
            for field in [
                "first_write_entered_ns",
                "transport_accepted_ns",
                "partial_count",
                "accepted_bytes",
                "outcome",
            ] {
                assert_eq!(row[field], expected[field], "{name} {field}");
            }
            if let Some(kind) = expected.get("transport_error_kind") {
                assert_eq!(row["transport_error"]["kind"], *kind, "{name}");
            }
            if let Some(kind) = expected.get("native_error_kind") {
                assert_eq!(row["native_error"]["kind"], *kind, "{name}");
                assert_eq!(row["native_error"]["os_code"], expected["native_os_code"]);
            }
            if let Some(code) = expected.get("contradiction_code") {
                assert_eq!(row["contradiction"]["code"], *code, "{name}");
            }
        }

        let physical = LogicalReportTelemetry::default().projection_json(Some(115_200), None);
        assert_eq!(
            physical["uart_complete_frame"]["measured"],
            fixture["physical_boundaries"]["uart_complete_frame"]["measured"]
        );
        assert_eq!(
            physical["uart_complete_frame"]["classification"],
            fixture["physical_boundaries"]["uart_complete_frame"]["classification"]
        );
        assert_eq!(
            physical["usb_hid"],
            fixture["physical_boundaries"]["usb_hid"]
        );
        assert_eq!(
            physical["switch_physical_order"],
            fixture["physical_boundaries"]["switch_physical_order"]
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
