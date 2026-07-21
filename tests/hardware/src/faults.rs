use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerOptions, ControllerSnapshot, PreciseSequence,
    SequenceStep,
};
use easycon_model::Button;
use easycon_runtime::{Operation, OperationSnapshot, OperationState, WaitResult, WaitTimeout};
use serde_json::{Value, json};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use super::{
    CancelSnapshotReadiness, CommandFailure, CommandFailureKind, Harness, INTERRUPT_POLL_SLICE,
    OPERATION_TIMEOUT, RunControl, cancel_snapshot_readiness, cleanup_succeeded,
    controller_snapshot_json, operation_failure, operation_json, wait_terminal,
};
use crate::device::{AdmittedDevice, DeviceDiscovery};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultRole {
    Occupier,
    OccupiedProbe,
    Cancel,
    Deadline,
}

impl FaultRole {
    const fn resource_name(self) -> &'static str {
        match self {
            Self::Occupier => "occupier",
            Self::OccupiedProbe => "occupied_probe",
            Self::Cancel => "cancel",
            Self::Deadline => "deadline",
        }
    }

    const fn cleanup_field(self) -> &'static str {
        match self {
            Self::Occupier => "occupier_cleanup",
            Self::OccupiedProbe => "occupied_probe_cleanup",
            Self::Cancel => "cancel_cleanup",
            Self::Deadline => "deadline_cleanup",
        }
    }

    const fn post_close_snapshot_field(self) -> &'static str {
        match self {
            Self::Occupier => "occupier_post_close_snapshot",
            Self::OccupiedProbe => "occupied_probe_post_close_snapshot",
            Self::Cancel => "cancel_post_close_snapshot",
            Self::Deadline => "deadline_post_close_snapshot",
        }
    }

    const fn native_attempts_field(self) -> Option<&'static str> {
        match self {
            Self::OccupiedProbe => Some("port_occupied_native_open_attempts"),
            Self::Deadline => Some("deadline_native_open_attempts"),
            Self::Occupier | Self::Cancel => None,
        }
    }

    const fn identity_attempts_field(self) -> &'static str {
        match self {
            Self::Occupier => "occupier_identity_open_attempts",
            Self::OccupiedProbe => "occupied_probe_identity_open_attempts",
            Self::Cancel => "cancel_identity_open_attempts",
            Self::Deadline => "deadline_identity_open_attempts",
        }
    }

    const fn close_stage(self) -> &'static str {
        match self {
            Self::Occupier => "occupier_close",
            Self::OccupiedProbe => "occupied_probe_close",
            Self::Cancel => "cancel_close",
            Self::Deadline => "deadline_close",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultWaitStage {
    OccupierConnect,
    OccupiedProbe,
    CancelConnect,
    CancelTerminal,
    Deadline,
}

impl FaultWaitStage {
    const fn failure_stage(self) -> &'static str {
        match self {
            Self::OccupierConnect => "occupier_connect_wait",
            Self::OccupiedProbe => "occupied_probe_connect_wait",
            Self::CancelConnect => "cancel_connect_wait",
            Self::CancelTerminal => "cancel_terminal_wait",
            Self::Deadline => "deadline_terminal_wait",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FaultFailure {
    kind: FaultFailureKind,
    stage: &'static str,
    message: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FaultFailureKind {
    Failed,
    Interrupted,
}

impl FaultFailure {
    fn new(stage: &'static str, message: impl Into<String>) -> Self {
        Self {
            kind: FaultFailureKind::Failed,
            stage,
            message: message.into(),
        }
    }

    fn from_command(failure: CommandFailure) -> Self {
        Self {
            kind: match failure.kind {
                CommandFailureKind::Failed => FaultFailureKind::Failed,
                CommandFailureKind::Interrupted => FaultFailureKind::Interrupted,
            },
            stage: failure.stage,
            message: failure.message,
        }
    }
}

struct FaultCloseEvidence {
    cleanup: Value,
    post_controller_snapshot: ControllerSnapshot,
    native_open_attempts: Value,
    identity_open_attempts: Value,
}

trait FaultHarness: Sized {
    fn admit_connect(&self, options: ConnectOptions) -> Result<Operation, String>;
    fn admit_sequence(&self, sequence: PreciseSequence) -> Result<Operation, String>;
    fn controller_snapshot(&self) -> ControllerSnapshot;
    fn now_ns(&self) -> u64;
    fn native_open_attempts(&self) -> Value;
    fn identity_open_attempts(&self) -> Value;
    fn close(self) -> FaultCloseEvidence;
}

trait FaultHarnessFactory {
    type Harness: FaultHarness;

    fn create(&mut self, role: FaultRole, target: &AdmittedDevice)
    -> Result<Self::Harness, String>;
}

trait FaultWaiter {
    fn checkpoint(
        &mut self,
        stage: &'static str,
        operation: Option<&Operation>,
    ) -> Result<(), FaultFailure>;

    fn wait_terminal(
        &mut self,
        stage: FaultWaitStage,
        operation: &Operation,
    ) -> Result<OperationSnapshot, FaultFailure>;

    fn wait_cancel_ready<H: FaultHarness>(
        &mut self,
        harness: &H,
        operation: &Operation,
        baseline: ControllerSnapshot,
    ) -> Result<ControllerSnapshot, FaultFailure>;

    fn settle(&mut self, operation: &Operation) -> Result<OperationSnapshot, FaultFailure>;
}

trait FaultSequenceBuilder {
    fn build(&mut self) -> Result<PreciseSequence, String>;
}

struct ProductionFactory {
    discovery: Arc<dyn DeviceDiscovery>,
}

impl FaultHarnessFactory for ProductionFactory {
    type Harness = Harness;

    fn create(
        &mut self,
        _role: FaultRole,
        target: &AdmittedDevice,
    ) -> Result<Self::Harness, String> {
        Harness::new(target, self.discovery.clone(), ControllerOptions::default())
    }
}

#[derive(Default)]
struct ProductionWaiter<'control, 'run> {
    control: Option<&'control mut RunControl<'run>>,
}

impl FaultWaiter for ProductionWaiter<'_, '_> {
    fn checkpoint(
        &mut self,
        stage: &'static str,
        operation: Option<&Operation>,
    ) -> Result<(), FaultFailure> {
        let Some(control) = self.control.as_deref_mut() else {
            return Ok(());
        };
        if !control.interrupt.is_requested() {
            return Ok(());
        }
        let failure = match operation {
            Some(operation) => control.cancel_operation(operation, stage),
            None => control
                .checkpoint(stage)
                .expect_err("requested interrupt must stop fault admission"),
        };
        Err(FaultFailure::from_command(failure))
    }

    fn wait_terminal(
        &mut self,
        stage: FaultWaitStage,
        operation: &Operation,
    ) -> Result<OperationSnapshot, FaultFailure> {
        let started = Instant::now();
        loop {
            self.checkpoint(stage.failure_stage(), Some(operation))?;
            let Some(remaining) = OPERATION_TIMEOUT.checked_sub(started.elapsed()) else {
                return Err(FaultFailure::new(
                    stage.failure_stage(),
                    format!("operation {} wait timed out", operation.id().get()),
                ));
            };
            if remaining.is_zero() {
                return Err(FaultFailure::new(
                    stage.failure_stage(),
                    format!("operation {} wait timed out", operation.id().get()),
                ));
            }
            match operation.wait(WaitTimeout::For(remaining.min(INTERRUPT_POLL_SLICE))) {
                WaitResult::Completed(snapshot) => return Ok(snapshot),
                WaitResult::Timeout => {}
            }
        }
    }

    fn wait_cancel_ready<H: FaultHarness>(
        &mut self,
        harness: &H,
        operation: &Operation,
        baseline: ControllerSnapshot,
    ) -> Result<ControllerSnapshot, FaultFailure> {
        if !baseline.desired_report.is_neutral()
            || baseline.lease != easycon_controller::ControllerLeaseState::Available
        {
            return Err(FaultFailure::new(
                "cancel_readiness",
                "cancel fault baseline was not neutral and lease-available",
            ));
        }
        let expected_count = baseline
            .accepted_report_count
            .checked_add(1)
            .ok_or_else(|| {
                FaultFailure::new("cancel_readiness", "accepted report count exhausted")
            })?;
        let started = Instant::now();
        loop {
            self.checkpoint("cancel_readiness", Some(operation))?;
            let snapshot = harness.controller_snapshot();
            match cancel_snapshot_readiness(&snapshot, expected_count, operation.id()) {
                CancelSnapshotReadiness::Ready => return Ok(snapshot),
                CancelSnapshotReadiness::Pending => {}
                CancelSnapshotReadiness::Invalid(message) => {
                    return Err(FaultFailure::new("cancel_readiness", message));
                }
            }
            if operation.snapshot().state.is_terminal() {
                return Err(FaultFailure::new(
                    "cancel_readiness",
                    "cancel-fault sequence reached terminal before cancel request",
                ));
            }
            if started.elapsed() >= OPERATION_TIMEOUT {
                return Err(FaultFailure::new(
                    "cancel_readiness",
                    "timed out waiting for first cancel-fault report acceptance",
                ));
            }
            thread::sleep(Duration::from_millis(1));
        }
    }

    fn settle(&mut self, operation: &Operation) -> Result<OperationSnapshot, FaultFailure> {
        wait_terminal(operation, OPERATION_TIMEOUT)
            .map_err(|error| FaultFailure::new("fault_recovery_settle", error))
    }
}

struct ProductionSequenceBuilder;

impl FaultSequenceBuilder for ProductionSequenceBuilder {
    fn build(&mut self) -> Result<PreciseSequence, String> {
        PreciseSequence::new(vec![
            SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
            SequenceStep::new(60_000_000_000, ControllerAction::ButtonUp(Button::A)),
        ])
        .map_err(|error| error.to_string())
    }
}

impl FaultHarness for Harness {
    fn admit_connect(&self, options: ConnectOptions) -> Result<Operation, String> {
        self.controller
            .connect(options)
            .map_err(|error| error.to_string())
    }

    fn admit_sequence(&self, sequence: PreciseSequence) -> Result<Operation, String> {
        self.controller
            .precise_sequence(sequence)
            .map_err(|error| error.to_string())
    }

    fn controller_snapshot(&self) -> ControllerSnapshot {
        self.controller.snapshot()
    }

    fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    fn native_open_attempts(&self) -> Value {
        Harness::native_open_attempts(self)
    }

    fn identity_open_attempts(&self) -> Value {
        Harness::identity_open_attempts(self)
    }

    fn close(self) -> FaultCloseEvidence {
        let cleanup = Harness::close(&self);
        let native_open_attempts = Harness::native_open_attempts(&self);
        let identity_open_attempts = Harness::identity_open_attempts(&self);
        FaultCloseEvidence {
            cleanup,
            post_controller_snapshot: self.controller.snapshot(),
            native_open_attempts,
            identity_open_attempts,
        }
    }
}

struct ActiveOperation {
    operation: Operation,
    evidence_field: &'static str,
    role: FaultRole,
}

struct FaultRun<H> {
    occupier: Option<H>,
    occupied_probe: Option<H>,
    cancel: Option<H>,
    deadline: Option<H>,
    active: Option<ActiveOperation>,
    result: Value,
}

impl<H: FaultHarness> FaultRun<H> {
    fn new() -> Self {
        Self {
            occupier: None,
            occupied_probe: None,
            cancel: None,
            deadline: None,
            active: None,
            result: json!({
                "command": "faults",
                "scenarios": {
                    "port_occupied": {"status": "not_run"},
                    "cancel": {"status": "not_run"},
                    "deadline": {"status": "not_run"},
                },
                "resources": {
                    "occupier": {"created": false},
                    "occupied_probe": {"created": false},
                    "cancel": {"created": false},
                    "deadline": {"created": false},
                },
            }),
        }
    }

    fn begin_scenario(&mut self, scenario: &str) {
        self.result["scenarios"][scenario]["status"] = json!("running");
    }

    fn complete_scenario(&mut self, scenario: &str) {
        self.result["scenarios"][scenario]["status"] = json!("completed");
    }

    fn install(&mut self, role: FaultRole, harness: H) {
        match role {
            FaultRole::Occupier => self.occupier = Some(harness),
            FaultRole::OccupiedProbe => self.occupied_probe = Some(harness),
            FaultRole::Cancel => self.cancel = Some(harness),
            FaultRole::Deadline => self.deadline = Some(harness),
        }
        self.result["resources"][role.resource_name()]["created"] = json!(true);
    }

    fn harness(&self, role: FaultRole) -> &H {
        match role {
            FaultRole::Occupier => self.occupier.as_ref(),
            FaultRole::OccupiedProbe => self.occupied_probe.as_ref(),
            FaultRole::Cancel => self.cancel.as_ref(),
            FaultRole::Deadline => self.deadline.as_ref(),
        }
        .expect("fault harness role is installed")
    }

    fn own_operation(
        &mut self,
        role: FaultRole,
        operation: Operation,
        evidence_field: &'static str,
    ) {
        debug_assert!(self.active.is_none());
        self.active = Some(ActiveOperation {
            operation,
            evidence_field,
            role,
        });
    }

    fn active_operation(&self) -> &Operation {
        &self
            .active
            .as_ref()
            .expect("fault operation is owned")
            .operation
    }

    fn complete_active(&mut self) -> Operation {
        let active = self.active.take().expect("fault operation is owned");
        self.result[active.evidence_field] = operation_json(&active.operation);
        active.operation
    }

    fn close_role(&mut self, role: FaultRole) -> Result<(), FaultFailure> {
        let harness = match role {
            FaultRole::Occupier => self.occupier.take(),
            FaultRole::OccupiedProbe => self.occupied_probe.take(),
            FaultRole::Cancel => self.cancel.take(),
            FaultRole::Deadline => self.deadline.take(),
        };
        let Some(harness) = harness else {
            return Ok(());
        };
        let evidence = harness.close();
        let succeeded = cleanup_succeeded(&evidence.cleanup);
        self.result[role.cleanup_field()] = evidence.cleanup;
        self.result[role.post_close_snapshot_field()] =
            controller_snapshot_json(evidence.post_controller_snapshot);
        if let Some(field) = role.native_attempts_field() {
            self.result[field] = evidence.native_open_attempts;
        }
        self.result[role.identity_attempts_field()] = evidence.identity_open_attempts;
        self.capture_active_after_close(role);
        if succeeded {
            Ok(())
        } else {
            Err(FaultFailure::new(
                role.close_stage(),
                format!("{} cleanup did not complete", role.resource_name()),
            ))
        }
    }

    fn capture_active_after_close(&mut self, role: FaultRole) {
        if self.active.as_ref().map(|active| active.role) != Some(role) {
            return;
        }
        let active = self.active.take().expect("fault operation is owned");
        let operation = operation_json(&active.operation);
        self.result[active.evidence_field] = operation.clone();
        self.result["post_cleanup_operation"] = json!({
            "field": active.evidence_field,
            "operation": operation,
        });
    }

    fn capture_partial_harness_evidence(&mut self) {
        if let Some(occupier) = self.occupier.as_ref() {
            self.result[FaultRole::Occupier.identity_attempts_field()] =
                occupier.identity_open_attempts();
        }
        if let Some(probe) = self.occupied_probe.as_ref() {
            self.result["port_occupied_native_open_attempts"] = probe.native_open_attempts();
            self.result[FaultRole::OccupiedProbe.identity_attempts_field()] =
                probe.identity_open_attempts();
            self.result["occupied_probe_failure_snapshot"] =
                controller_snapshot_json(probe.controller_snapshot());
        }
        if let Some(cancel) = self.cancel.as_ref() {
            self.result[FaultRole::Cancel.identity_attempts_field()] =
                cancel.identity_open_attempts();
            self.result["cancel_failure_snapshot"] =
                controller_snapshot_json(cancel.controller_snapshot());
        }
        if let Some(deadline) = self.deadline.as_ref() {
            self.result["deadline_native_open_attempts"] = deadline.native_open_attempts();
            self.result[FaultRole::Deadline.identity_attempts_field()] =
                deadline.identity_open_attempts();
            self.result["deadline_failure_snapshot"] =
                controller_snapshot_json(deadline.controller_snapshot());
        }
    }

    fn recover_active<W: FaultWaiter>(&mut self, waiter: &mut W) {
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let observed_at_failure = operation_json(&active.operation);
        self.result[active.evidence_field] = observed_at_failure.clone();
        self.result["active_operation_at_failure"] = json!({
            "field": active.evidence_field,
            "operation": observed_at_failure,
        });
        if !active.operation.snapshot().state.is_terminal() {
            let cancel_outcome = active.operation.cancel();
            let settle_error = waiter
                .settle(&active.operation)
                .err()
                .map(|failure| failure.message);
            self.result["recovery_operation"] = json!(active.evidence_field);
            self.result["recovery_cancel_outcome"] = json!(format!("{cancel_outcome:?}"));
            self.result["recovery_settle_error"] = json!(settle_error);
            self.result[active.evidence_field] = operation_json(&active.operation);
        }
        if active.evidence_field == "cancel"
            && let Some(cancel) = self.cancel.as_ref()
        {
            let snapshot = cancel.controller_snapshot();
            self.result["post_cancel_snapshot"] = controller_snapshot_json(snapshot);
            self.result["post_cancel_wait_snapshot"] = controller_snapshot_json(snapshot);
        }
    }

    fn finish<W: FaultWaiter>(
        mut self,
        execution: Result<(), FaultFailure>,
        waiter: &mut W,
    ) -> Value {
        let mut failure = execution.err();
        self.capture_partial_harness_evidence();
        self.recover_active(waiter);

        for role in [
            FaultRole::Deadline,
            FaultRole::Cancel,
            FaultRole::OccupiedProbe,
            FaultRole::Occupier,
        ] {
            if let Err(close_failure) = self.close_role(role) {
                match failure.as_ref().map(|failure| failure.kind) {
                    Some(FaultFailureKind::Failed) => {
                        append_fault_cleanup_error(&mut self.result, &close_failure);
                    }
                    Some(FaultFailureKind::Interrupted) => {
                        append_fault_cleanup_error(&mut self.result, &close_failure);
                        failure = Some(close_failure);
                    }
                    None => failure = Some(close_failure),
                }
            }
        }

        if let Some(failure) = failure {
            let scenario = scenario_for_stage(failure.stage);
            self.result["scenarios"][scenario]["status"] = json!(match failure.kind {
                FaultFailureKind::Failed => "failed",
                FaultFailureKind::Interrupted => "cancelled",
            });
            self.result["scenarios"][scenario]["failure_stage"] = json!(failure.stage);
            let scenarios = ["port_occupied", "cancel", "deadline"];
            let failed_index = scenarios
                .iter()
                .position(|candidate| *candidate == scenario)
                .expect("fault failure maps to a known scenario");
            for later in &scenarios[failed_index + 1..] {
                self.result["scenarios"][*later]["reason"] = json!({
                    "kind": match failure.kind {
                        FaultFailureKind::Failed => "prior_failure",
                        FaultFailureKind::Interrupted => "operator_interrupt",
                    },
                    "stage": failure.stage,
                });
            }
            if scenario == "cancel" && self.result.get("cancel_request_outcome").is_none() {
                self.result["cancel_request_outcome"] = Value::Null;
            }
            match failure.kind {
                FaultFailureKind::Failed => {
                    self.result["execution_error"] = json!({
                        "stage": failure.stage,
                        "message": failure.message,
                    });
                }
                FaultFailureKind::Interrupted => {
                    self.result["operator_terminal_outcome"] = json!("interrupted");
                    self.result["operator_cancellation"] = json!({
                        "stage": failure.stage,
                        "message": failure.message,
                    });
                }
            }
        }
        self.result
    }
}

fn append_fault_cleanup_error(result: &mut Value, failure: &FaultFailure) {
    let entry = json!({
        "stage": failure.stage,
        "message": failure.message,
    });
    match result.get_mut("cleanup_errors") {
        Some(Value::Array(errors)) => errors.push(entry),
        _ => result["cleanup_errors"] = json!([entry]),
    }
}

fn scenario_for_stage(stage: &str) -> &'static str {
    if stage.starts_with("occupier_") || stage.starts_with("occupied_probe_") {
        "port_occupied"
    } else if stage.starts_with("cancel_") {
        "cancel"
    } else {
        "deadline"
    }
}

fn create_and_install<F: FaultHarnessFactory>(
    run: &mut FaultRun<F::Harness>,
    factory: &mut F,
    role: FaultRole,
    target: &AdmittedDevice,
) -> Result<(), FaultFailure> {
    let stage = match role {
        FaultRole::Occupier => "occupier_create",
        FaultRole::OccupiedProbe => "occupied_probe_create",
        FaultRole::Cancel => "cancel_create",
        FaultRole::Deadline => "deadline_create",
    };
    let harness = factory
        .create(role, target)
        .map_err(|error| FaultFailure::new(stage, error))?;
    run.install(role, harness);
    Ok(())
}

fn admit_connect<H: FaultHarness>(
    run: &mut FaultRun<H>,
    role: FaultRole,
    options: ConnectOptions,
    stage: &'static str,
    evidence_field: &'static str,
) -> Result<(), FaultFailure> {
    let operation = run
        .harness(role)
        .admit_connect(options)
        .map_err(|error| FaultFailure::new(stage, error))?;
    run.own_operation(role, operation, evidence_field);
    Ok(())
}

fn execute<F, W, B>(
    run: &mut FaultRun<F::Harness>,
    factory: &mut F,
    waiter: &mut W,
    sequence_builder: &mut B,
    target: &AdmittedDevice,
) -> Result<(), FaultFailure>
where
    F: FaultHarnessFactory,
    W: FaultWaiter,
    B: FaultSequenceBuilder,
{
    waiter.checkpoint("occupier_create", None)?;
    run.begin_scenario("port_occupied");
    create_and_install(run, factory, FaultRole::Occupier, target)?;
    waiter.checkpoint("occupier_connect_admit", None)?;
    admit_connect(
        run,
        FaultRole::Occupier,
        ConnectOptions::default(),
        "occupier_connect_admit",
        "occupier_connect",
    )?;
    let snapshot = waiter.wait_terminal(FaultWaitStage::OccupierConnect, run.active_operation())?;
    if !snapshot.state.is_terminal() {
        return Err(FaultFailure::new(
            "occupier_connect_terminal",
            operation_failure(run.active_operation()),
        ));
    }
    let operation = run.complete_active();
    if snapshot.state != OperationState::Succeeded {
        return Err(FaultFailure::new(
            "occupier_connect_terminal",
            operation_failure(&operation),
        ));
    }

    waiter.checkpoint("occupied_probe_create", None)?;
    create_and_install(run, factory, FaultRole::OccupiedProbe, target)?;
    waiter.checkpoint("occupied_probe_connect_admit", None)?;
    admit_connect(
        run,
        FaultRole::OccupiedProbe,
        ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 300_000_000,
        },
        "occupied_probe_connect_admit",
        "port_occupied",
    )?;
    let snapshot = waiter.wait_terminal(FaultWaitStage::OccupiedProbe, run.active_operation())?;
    if !snapshot.state.is_terminal() {
        return Err(FaultFailure::new(
            "occupied_probe_connect_terminal",
            operation_failure(run.active_operation()),
        ));
    }
    run.complete_active();
    run.result["port_occupied_expected_failed"] = json!(snapshot.state == OperationState::Failed);
    run.result["port_occupied_native_open_attempts"] =
        run.harness(FaultRole::OccupiedProbe).native_open_attempts();
    run.close_role(FaultRole::OccupiedProbe)?;
    run.close_role(FaultRole::Occupier)?;
    run.complete_scenario("port_occupied");

    waiter.checkpoint("cancel_create", None)?;
    run.begin_scenario("cancel");
    create_and_install(run, factory, FaultRole::Cancel, target)?;
    waiter.checkpoint("cancel_connect_admit", None)?;
    admit_connect(
        run,
        FaultRole::Cancel,
        ConnectOptions::default(),
        "cancel_connect_admit",
        "cancel_connect",
    )?;
    let snapshot = waiter.wait_terminal(FaultWaitStage::CancelConnect, run.active_operation())?;
    if !snapshot.state.is_terminal() {
        return Err(FaultFailure::new(
            "cancel_connect_terminal",
            operation_failure(run.active_operation()),
        ));
    }
    let operation = run.complete_active();
    if snapshot.state != OperationState::Succeeded {
        return Err(FaultFailure::new(
            "cancel_connect_terminal",
            operation_failure(&operation),
        ));
    }

    let baseline = run.harness(FaultRole::Cancel).controller_snapshot();
    run.result["cancel_baseline_snapshot"] = controller_snapshot_json(baseline);
    waiter.checkpoint("cancel_sequence_build", None)?;
    let sequence = sequence_builder
        .build()
        .map_err(|error| FaultFailure::new("cancel_sequence_build", error))?;
    waiter.checkpoint("cancel_sequence_admit", None)?;
    let operation = run
        .harness(FaultRole::Cancel)
        .admit_sequence(sequence)
        .map_err(|error| FaultFailure::new("cancel_sequence_admit", error))?;
    run.own_operation(FaultRole::Cancel, operation, "cancel");
    let before_request = match waiter.wait_cancel_ready(
        run.harness(FaultRole::Cancel),
        run.active_operation(),
        baseline,
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let observed = run.harness(FaultRole::Cancel).controller_snapshot();
            run.result["cancel_before_request_snapshot"] = controller_snapshot_json(observed);
            return Err(error);
        }
    };
    run.result["cancel_before_request_snapshot"] = controller_snapshot_json(before_request);
    waiter.checkpoint("cancel_request", Some(run.active_operation()))?;
    let request_outcome = run.active_operation().cancel();
    run.result["cancel_request_outcome"] = json!(format!("{request_outcome:?}"));
    let snapshot = waiter.wait_terminal(FaultWaitStage::CancelTerminal, run.active_operation())?;
    if !snapshot.state.is_terminal() {
        return Err(FaultFailure::new(
            "cancel_operation_terminal",
            operation_failure(run.active_operation()),
        ));
    }
    run.complete_active();
    run.result["cancel_expected_cancelled"] = json!(snapshot.state == OperationState::Cancelled);
    let post_cancel = run.harness(FaultRole::Cancel).controller_snapshot();
    run.result["post_cancel_snapshot"] = controller_snapshot_json(post_cancel);
    run.result["post_cancel_wait_snapshot"] = controller_snapshot_json(post_cancel);
    run.close_role(FaultRole::Cancel)?;
    run.complete_scenario("cancel");

    waiter.checkpoint("deadline_create", None)?;
    run.begin_scenario("deadline");
    create_and_install(run, factory, FaultRole::Deadline, target)?;
    let deadline_ns = run.harness(FaultRole::Deadline).now_ns();
    waiter.checkpoint("deadline_connect_admit", None)?;
    admit_connect(
        run,
        FaultRole::Deadline,
        ConnectOptions {
            operation_deadline_ns: Some(deadline_ns),
            protocol_timeout_ns: 300_000_000,
        },
        "deadline_connect_admit",
        "deadline",
    )?;
    let snapshot = waiter.wait_terminal(FaultWaitStage::Deadline, run.active_operation())?;
    if !snapshot.state.is_terminal() {
        return Err(FaultFailure::new(
            "deadline_connect_terminal",
            operation_failure(run.active_operation()),
        ));
    }
    run.complete_active();
    run.result["deadline_expected_cancelled"] = json!(snapshot.state == OperationState::Cancelled);
    run.result["deadline_native_open_attempts"] =
        run.harness(FaultRole::Deadline).native_open_attempts();
    run.close_role(FaultRole::Deadline)?;
    run.complete_scenario("deadline");
    Ok(())
}

fn run_with<F, W, B>(
    target: &AdmittedDevice,
    factory: &mut F,
    waiter: &mut W,
    sequence_builder: &mut B,
) -> Value
where
    F: FaultHarnessFactory,
    W: FaultWaiter,
    B: FaultSequenceBuilder,
{
    let mut run = FaultRun::new();
    let execution = execute(&mut run, factory, waiter, sequence_builder, target);
    run.finish(execution, waiter)
}

pub(super) fn run(
    target: AdmittedDevice,
    discovery: Arc<dyn DeviceDiscovery>,
    control: &mut RunControl<'_>,
) -> Value {
    let mut factory = ProductionFactory { discovery };
    let mut waiter = ProductionWaiter {
        control: Some(control),
    };
    let mut sequence_builder = ProductionSequenceBuilder;
    run_with(&target, &mut factory, &mut waiter, &mut sequence_builder)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use easycon_controller::{
        AckFrame, AckRequest, ControllerLeaseState, ControllerSession, ControllerState,
        ControllerTransport, HandshakeRequest, SwitchReport, TransportError, TransportErrorKind,
        WriteRequest,
    };
    use easycon_model::{EasyConError, ErrorCode, ErrorDomain, Hat, ResourceId, StickPosition};
    use easycon_runtime::{CancellationReason, Clock, OperationValue, Runtime, SystemClock};
    use easycon_serial::SerialPortDescriptor;

    use super::*;
    use crate::device::{AdmissionDecision, DeviceTargetRequest, select_device};
    use crate::{
        ObservedTransport, Telemetry, cleanup_contract_succeeded, controller_cleanup_json,
        document_exit_code, finalize_result, harness_cleanup_json, qualification_check,
        runtime_close_json,
    };

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FailPoint {
        Create(FaultRole),
        ConnectAdmit(FaultRole),
        Wait(FaultWaitStage),
        CancelReady,
        SequenceBuild,
        SequenceAdmit,
        Close(FaultRole),
    }

    impl FailPoint {
        const fn stage(self) -> &'static str {
            match self {
                Self::Create(FaultRole::Occupier) => "occupier_create",
                Self::Create(FaultRole::OccupiedProbe) => "occupied_probe_create",
                Self::Create(FaultRole::Cancel) => "cancel_create",
                Self::Create(FaultRole::Deadline) => "deadline_create",
                Self::ConnectAdmit(FaultRole::Occupier) => "occupier_connect_admit",
                Self::ConnectAdmit(FaultRole::OccupiedProbe) => "occupied_probe_connect_admit",
                Self::ConnectAdmit(FaultRole::Cancel) => "cancel_connect_admit",
                Self::ConnectAdmit(FaultRole::Deadline) => "deadline_connect_admit",
                Self::Wait(FaultWaitStage::OccupierConnect) => "occupier_connect_wait",
                Self::Wait(FaultWaitStage::OccupiedProbe) => "occupied_probe_connect_wait",
                Self::Wait(FaultWaitStage::CancelConnect) => "cancel_connect_wait",
                Self::Wait(FaultWaitStage::CancelTerminal) => "cancel_terminal_wait",
                Self::Wait(FaultWaitStage::Deadline) => "deadline_terminal_wait",
                Self::CancelReady => "cancel_readiness",
                Self::SequenceBuild => "cancel_sequence_build",
                Self::SequenceAdmit => "cancel_sequence_admit",
                Self::Close(role) => role.close_stage(),
            }
        }
    }

    #[derive(Clone, Copy, Debug, Default)]
    struct FakeScript {
        failure: Option<FailPoint>,
        interrupt_at: Option<&'static str>,
        additional_close_failure: Option<FaultRole>,
        recovery_settle_failure: bool,
        final_native_attempt: bool,
        nonterminal_wait: Option<FaultWaitStage>,
    }

    impl FakeScript {
        fn fails(self, point: FailPoint) -> bool {
            self.failure == Some(point)
        }

        fn close_fails(self, role: FaultRole) -> bool {
            self.fails(FailPoint::Close(role)) || self.additional_close_failure == Some(role)
        }
    }

    #[derive(Clone, Debug, Default)]
    struct Trace {
        attempted: Vec<FaultRole>,
        created: Vec<FaultRole>,
        closed: Vec<FaultRole>,
        final_native_attempt_roles: Vec<FaultRole>,
    }

    type SharedTrace = Arc<Mutex<Trace>>;

    struct FakeFactory {
        script: FakeScript,
        trace: SharedTrace,
    }

    impl FaultHarnessFactory for FakeFactory {
        type Harness = FakeHarness;

        fn create(
            &mut self,
            role: FaultRole,
            _target: &AdmittedDevice,
        ) -> Result<Self::Harness, String> {
            self.trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .attempted
                .push(role);
            if self.script.fails(FailPoint::Create(role)) {
                return Err(format!("injected {}", FailPoint::Create(role).stage()));
            }
            self.trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .created
                .push(role);
            Ok(FakeHarness {
                role,
                runtime: Runtime::new(Arc::new(SystemClock::default())),
                sequence: Arc::new(Mutex::new(None)),
                script: self.script,
                trace: Arc::clone(&self.trace),
            })
        }
    }

    struct FakeHarness {
        role: FaultRole,
        runtime: Runtime,
        sequence: Arc<Mutex<Option<Operation>>>,
        script: FakeScript,
        trace: SharedTrace,
    }

    impl FakeHarness {
        fn operation(&self, deadline_ns: Option<u64>) -> Result<Operation, String> {
            self.runtime
                .create_operation(deadline_ns)
                .map_err(|error| error.to_string())
        }

        fn connected_snapshot(&self) -> ControllerSnapshot {
            let sequence = self
                .sequence
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let Some(operation) = sequence else {
                return ControllerSnapshot {
                    state: ControllerState::Connected,
                    ..ControllerSnapshot::default()
                };
            };
            let state = operation.snapshot().state;
            if state == OperationState::Cancelled {
                ControllerSnapshot {
                    state: ControllerState::Connected,
                    accepted_report_count: 2,
                    last_report_timestamp_ns: Some(2),
                    ..ControllerSnapshot::default()
                }
            } else {
                ControllerSnapshot {
                    state: ControllerState::Connected,
                    desired_report: SwitchReport::new(
                        Button::A.mask(),
                        Hat::Center,
                        StickPosition::CENTER,
                        StickPosition::CENTER,
                    ),
                    accepted_report_count: 1,
                    last_report_timestamp_ns: Some(1),
                    lease: ControllerLeaseState::Sequence(operation.id()),
                }
            }
        }
    }

    impl FaultHarness for FakeHarness {
        fn admit_connect(&self, _options: ConnectOptions) -> Result<Operation, String> {
            let point = FailPoint::ConnectAdmit(self.role);
            if self.script.fails(point) {
                return Err(format!("injected {}", point.stage()));
            }
            let operation = self.operation(None)?;
            let _ = operation.start();
            match self.role {
                FaultRole::Occupier | FaultRole::Cancel => {
                    let _ = operation.succeed(OperationValue::Unit);
                }
                FaultRole::OccupiedProbe => {
                    let _ = operation.fail(EasyConError::new(
                        ErrorDomain::Io,
                        ErrorCode::Transport,
                        "injected native port busy",
                    ));
                }
                FaultRole::Deadline => {
                    let _ = operation.request_cancel(CancellationReason::Deadline);
                    let _ = operation.finish_cancelled();
                }
            }
            Ok(operation)
        }

        fn admit_sequence(&self, _sequence: PreciseSequence) -> Result<Operation, String> {
            if self.script.fails(FailPoint::SequenceAdmit) {
                return Err(format!("injected {}", FailPoint::SequenceAdmit.stage()));
            }
            let operation = self.operation(None)?;
            let _ = operation.start();
            *self
                .sequence
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(operation.clone());
            Ok(operation)
        }

        fn controller_snapshot(&self) -> ControllerSnapshot {
            self.connected_snapshot()
        }

        fn now_ns(&self) -> u64 {
            1
        }

        fn native_open_attempts(&self) -> Value {
            if self.role == FaultRole::OccupiedProbe {
                let mut attempts = vec![
                    json!({
                        "baud": 115_200,
                        "succeeded": false,
                        "error": {
                            "kind": "PortBusy",
                            "os_code": 32,
                            "message": "injected native port busy",
                        },
                    }),
                    json!({
                        "baud": 9_600,
                        "succeeded": false,
                        "error": {
                            "kind": "PortBusy",
                            "os_code": 32,
                            "message": "injected native port busy",
                        },
                    }),
                ];
                if self
                    .trace
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .final_native_attempt_roles
                    .contains(&self.role)
                {
                    attempts.push(json!({
                        "baud": 57_600,
                        "succeeded": false,
                        "error": {
                            "kind": "Interrupted",
                            "os_code": 995,
                            "message": "injected final recovery attempt",
                        },
                    }));
                }
                Value::Array(attempts)
            } else {
                json!([])
            }
        }

        fn identity_open_attempts(&self) -> Value {
            json!([{"role": self.role.resource_name()}])
        }

        fn close(self) -> FaultCloseEvidence {
            self.trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .closed
                .push(self.role);
            if self.script.final_native_attempt
                && matches!(self.role, FaultRole::OccupiedProbe | FaultRole::Deadline)
            {
                self.trace
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .final_native_attempt_roles
                    .push(self.role);
            }
            if let Some(operation) = self
                .sequence
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
                && !operation.snapshot().state.is_terminal()
            {
                let _ = operation.finish_cancelled();
            }
            let controller = controller_cleanup_json(
                ResourceId::new(1),
                ControllerSnapshot::default(),
                ControllerSnapshot {
                    state: ControllerState::Closed,
                    ..ControllerSnapshot::default()
                },
                &[],
            );
            let mut runtime = runtime_close_json(self.runtime.close(), self.runtime.counts());
            if self.script.close_fails(self.role) {
                runtime["succeeded"] = json!(false);
                runtime["outcome"] = json!("Failed");
                runtime["diagnostic"] = json!("injected close failure");
            }
            let cleanup = harness_cleanup_json(controller, runtime);
            FaultCloseEvidence {
                cleanup,
                post_controller_snapshot: ControllerSnapshot {
                    state: ControllerState::Closed,
                    ..ControllerSnapshot::default()
                },
                native_open_attempts: self.native_open_attempts(),
                identity_open_attempts: self.identity_open_attempts(),
            }
        }
    }

    struct FakeWaiter {
        script: FakeScript,
    }

    impl FaultWaiter for FakeWaiter {
        fn checkpoint(
            &mut self,
            stage: &'static str,
            operation: Option<&Operation>,
        ) -> Result<(), FaultFailure> {
            if self.script.interrupt_at == Some(stage) {
                if let Some(operation) = operation {
                    let _ = operation.request_cancel(CancellationReason::Requested);
                    let _ = operation.finish_cancelled();
                }
                return Err(FaultFailure {
                    kind: FaultFailureKind::Interrupted,
                    stage,
                    message: "injected operator interrupt".to_owned(),
                });
            }
            Ok(())
        }

        fn wait_terminal(
            &mut self,
            stage: FaultWaitStage,
            operation: &Operation,
        ) -> Result<OperationSnapshot, FaultFailure> {
            let point = FailPoint::Wait(stage);
            if self.script.fails(point) {
                return Err(FaultFailure::new(
                    stage.failure_stage(),
                    format!("injected {}", point.stage()),
                ));
            }
            if self.script.nonterminal_wait == Some(stage) {
                let mut snapshot = operation.snapshot();
                snapshot.state = OperationState::Running;
                return Ok(snapshot);
            }
            if stage == FaultWaitStage::CancelTerminal {
                let _ = operation.finish_cancelled();
            }
            Ok(operation.snapshot())
        }

        fn wait_cancel_ready<H: FaultHarness>(
            &mut self,
            _harness: &H,
            operation: &Operation,
            baseline: ControllerSnapshot,
        ) -> Result<ControllerSnapshot, FaultFailure> {
            self.checkpoint("cancel_readiness", Some(operation))?;
            if self.script.fails(FailPoint::CancelReady) {
                return Err(FaultFailure::new(
                    "cancel_readiness",
                    format!("injected {}", FailPoint::CancelReady.stage()),
                ));
            }
            Ok(ControllerSnapshot {
                state: ControllerState::Connected,
                desired_report: SwitchReport::new(
                    Button::A.mask(),
                    Hat::Center,
                    StickPosition::CENTER,
                    StickPosition::CENTER,
                ),
                accepted_report_count: baseline.accepted_report_count + 1,
                last_report_timestamp_ns: Some(1),
                lease: ControllerLeaseState::Sequence(operation.id()),
            })
        }

        fn settle(&mut self, operation: &Operation) -> Result<OperationSnapshot, FaultFailure> {
            if self.script.recovery_settle_failure {
                return Err(FaultFailure::new(
                    "fault_recovery_settle",
                    "injected recovery_settle",
                ));
            }
            if !operation.snapshot().state.is_terminal() {
                let _ = operation.finish_cancelled();
            }
            Ok(operation.snapshot())
        }
    }

    struct FakeSequenceBuilder {
        script: FakeScript,
    }

    impl FaultSequenceBuilder for FakeSequenceBuilder {
        fn build(&mut self) -> Result<PreciseSequence, String> {
            if self.script.fails(FailPoint::SequenceBuild) {
                return Err(format!("injected {}", FailPoint::SequenceBuild.stage()));
            }
            ProductionSequenceBuilder.build()
        }
    }

    fn run_fake(script: FakeScript) -> (Value, Trace) {
        let trace = Arc::new(Mutex::new(Trace::default()));
        let mut factory = FakeFactory {
            script,
            trace: Arc::clone(&trace),
        };
        let mut waiter = FakeWaiter { script };
        let mut builder = FakeSequenceBuilder { script };
        let target = fake_target();
        let result = run_with(&target, &mut factory, &mut waiter, &mut builder);
        let observed = trace
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        (result, observed)
    }

    fn fake_target() -> AdmittedDevice {
        let request =
            DeviceTargetRequest::new("TEST\\FAULT", "COM1").expect("fault target request");
        let descriptor = SerialPortDescriptor::new("TEST\\FAULT", "COM1").expect("descriptor");
        let AdmissionDecision::Admitted(target) = select_device(request, vec![descriptor]) else {
            panic!("fault target must be admitted");
        };
        target
    }

    fn assert_closed_once(trace: &Trace, created: &[FaultRole]) {
        assert_eq!(trace.created, created);
        assert_eq!(trace.closed.len(), created.len());
        for role in created {
            assert_eq!(
                trace.closed.iter().filter(|closed| *closed == role).count(),
                1,
                "{role:?} must close exactly once"
            );
        }
    }

    struct AdapterNoopTransport;

    impl ControllerTransport for AdapterNoopTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in faults adapter lifecycle test",
            ))
        }

        fn close(&mut self) {}
    }

    struct AdapterFinalNeutralFailureTransport;

    impl ControllerTransport for AdapterFinalNeutralFailureTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            if request.context.kind == easycon_controller::WriteKind::Neutralize
                && request.context.operation_id.is_none()
            {
                Err(TransportError::new(
                    TransportErrorKind::Io,
                    "injected faults final neutral failure",
                ))
            } else {
                Ok(request.bytes.len())
            }
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in faults final neutral failure test",
            ))
        }

        fn close(&mut self) {}
    }

    fn adapter_harness(transport: Box<dyn ControllerTransport>, label: &str) -> Harness {
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
            identity_open_recorder: crate::device::IdentityOpenRecorder::default(),
            descriptor: SerialPortDescriptor::new(format!("DEVICE\\{label}"), "COM1")
                .expect("descriptor"),
        }
    }

    #[test]
    fn production_harness_adapter_closes_active_controller_and_runtime() {
        let harness = adapter_harness(Box::new(AdapterNoopTransport), "FAULT-ADAPTER");

        let connect = harness
            .admit_connect(ConnectOptions::default())
            .expect("connect operation");
        wait_terminal(&connect, OPERATION_TIMEOUT).expect("connected");
        let baseline = harness.controller_snapshot();
        let sequence = ProductionSequenceBuilder.build().expect("cancel sequence");
        let operation = harness
            .admit_sequence(sequence)
            .expect("sequence operation");
        let mut waiter = ProductionWaiter::default();
        let before_close = waiter
            .wait_cancel_ready(&harness, &operation, baseline)
            .expect("non-neutral acceptance");
        assert!(!before_close.desired_report.is_neutral());

        let evidence = <Harness as FaultHarness>::close(harness);

        assert_eq!(operation.snapshot().state, OperationState::Cancelled);
        assert!(cleanup_succeeded(&evidence.cleanup));
        assert_eq!(evidence.cleanup["controller"]["neutralization"], "accepted");
        assert_eq!(
            evidence.cleanup["controller"]["attempts"][0]["operation_id"],
            Value::Null
        );
        assert_eq!(
            evidence.cleanup["controller"]["attempts"][0]["accepted_bytes"],
            8
        );
        assert_eq!(
            evidence.post_controller_snapshot.state,
            ControllerState::Closed
        );
        assert!(
            evidence
                .post_controller_snapshot
                .desired_report
                .is_neutral()
        );
        assert_eq!(
            evidence.post_controller_snapshot.lease,
            ControllerLeaseState::Available
        );
        assert_eq!(evidence.native_open_attempts, json!([]));
        assert_eq!(evidence.identity_open_attempts, json!([]));
    }

    #[test]
    fn production_neutral_failure_maps_to_the_role_close_stage() {
        let harness = adapter_harness(
            Box::new(AdapterFinalNeutralFailureTransport),
            "FAULT-ADAPTER-NEUTRAL-FAILURE",
        );
        let connect = harness
            .admit_connect(ConnectOptions::default())
            .expect("connect operation");
        wait_terminal(&connect, OPERATION_TIMEOUT).expect("connected");
        let mut run = FaultRun::new();
        run.begin_scenario("port_occupied");
        run.install(FaultRole::Occupier, harness);

        let result = run.finish(Ok(()), &mut ProductionWaiter::default());

        assert_eq!(result["execution_error"]["stage"], "occupier_close");
        assert_eq!(result["scenarios"]["port_occupied"]["status"], "failed");
        assert_eq!(result["occupier_cleanup"]["succeeded"], false);
        assert_eq!(
            result["occupier_cleanup"]["controller"]["neutralization"],
            "not_delivered"
        );
        assert_eq!(result["occupier_cleanup"]["runtime"]["outcome"], "Closed");
        assert!(!cleanup_contract_succeeded("faults", &result));
    }

    #[test]
    fn complete_run_uses_four_isolated_roles_and_exact_qualification_evidence() {
        let (result, trace) = run_fake(FakeScript::default());
        let roles = [
            FaultRole::Occupier,
            FaultRole::OccupiedProbe,
            FaultRole::Cancel,
            FaultRole::Deadline,
        ];

        assert!(result.get("execution_error").is_none());
        assert!(cleanup_contract_succeeded("faults", &result));
        assert!(result.get("cleanup").is_none());
        assert!(result.get("secondary_cleanup").is_none());
        assert_eq!(trace.attempted, roles);
        assert_closed_once(&trace, &roles);
        assert_eq!(
            trace.closed,
            [
                FaultRole::OccupiedProbe,
                FaultRole::Occupier,
                FaultRole::Cancel,
                FaultRole::Deadline,
            ]
        );
        let (document, failure) = finalize_result("faults", Ok(result));
        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "completed");
        assert_eq!(document["qualification_status"], "passed");
        assert_eq!(document_exit_code(&document), 0);
    }

    #[test]
    fn interrupt_during_cancel_readiness_closes_prefix_and_skips_deadline() {
        let script = FakeScript {
            interrupt_at: Some("cancel_readiness"),
            ..FakeScript::default()
        };
        let (result, trace) = run_fake(script);

        assert_eq!(
            trace.attempted,
            [
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
            ]
        );
        assert_closed_once(
            &trace,
            &[
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
            ],
        );
        assert_eq!(
            trace.closed,
            [
                FaultRole::OccupiedProbe,
                FaultRole::Occupier,
                FaultRole::Cancel,
            ]
        );
        assert_eq!(result["resources"]["deadline"]["created"], false);
        assert_eq!(result["scenarios"]["cancel"]["status"], "cancelled");
        assert_eq!(
            result["scenarios"]["deadline"]["reason"],
            json!({
                "kind": "operator_interrupt",
                "stage": "cancel_readiness",
            })
        );
        assert_eq!(result["operator_terminal_outcome"], "interrupted");
        assert_eq!(result["operator_cancellation"]["stage"], "cancel_readiness");
        assert!(cleanup_contract_succeeded("faults", &result));

        let (document, failure) = finalize_result("faults", Ok(result));
        assert!(failure.is_none());
        assert_eq!(document["execution_status"], "cancelled");
        assert_eq!(document["qualification_status"], "unverified");
        assert_eq!(document_exit_code(&document), 130);
    }

    #[test]
    fn every_fault_stage_closes_the_created_prefix_and_stops_admission() {
        struct Case {
            point: FailPoint,
            created: &'static [FaultRole],
            operation_field: Option<&'static str>,
        }

        const NONE: &[FaultRole] = &[];
        const O: &[FaultRole] = &[FaultRole::Occupier];
        const OP: &[FaultRole] = &[FaultRole::Occupier, FaultRole::OccupiedProbe];
        const OPC: &[FaultRole] = &[
            FaultRole::Occupier,
            FaultRole::OccupiedProbe,
            FaultRole::Cancel,
        ];
        const OPCD: &[FaultRole] = &[
            FaultRole::Occupier,
            FaultRole::OccupiedProbe,
            FaultRole::Cancel,
            FaultRole::Deadline,
        ];

        let cases = [
            Case {
                point: FailPoint::Create(FaultRole::Occupier),
                created: NONE,
                operation_field: None,
            },
            Case {
                point: FailPoint::ConnectAdmit(FaultRole::Occupier),
                created: O,
                operation_field: None,
            },
            Case {
                point: FailPoint::Wait(FaultWaitStage::OccupierConnect),
                created: O,
                operation_field: Some("occupier_connect"),
            },
            Case {
                point: FailPoint::Create(FaultRole::OccupiedProbe),
                created: O,
                operation_field: None,
            },
            Case {
                point: FailPoint::ConnectAdmit(FaultRole::OccupiedProbe),
                created: OP,
                operation_field: None,
            },
            Case {
                point: FailPoint::Wait(FaultWaitStage::OccupiedProbe),
                created: OP,
                operation_field: Some("port_occupied"),
            },
            Case {
                point: FailPoint::Close(FaultRole::OccupiedProbe),
                created: OP,
                operation_field: Some("port_occupied"),
            },
            Case {
                point: FailPoint::Close(FaultRole::Occupier),
                created: OP,
                operation_field: Some("port_occupied"),
            },
            Case {
                point: FailPoint::Create(FaultRole::Cancel),
                created: OP,
                operation_field: Some("port_occupied"),
            },
            Case {
                point: FailPoint::ConnectAdmit(FaultRole::Cancel),
                created: OPC,
                operation_field: None,
            },
            Case {
                point: FailPoint::Wait(FaultWaitStage::CancelConnect),
                created: OPC,
                operation_field: Some("cancel_connect"),
            },
            Case {
                point: FailPoint::SequenceBuild,
                created: OPC,
                operation_field: Some("cancel_connect"),
            },
            Case {
                point: FailPoint::SequenceAdmit,
                created: OPC,
                operation_field: Some("cancel_connect"),
            },
            Case {
                point: FailPoint::CancelReady,
                created: OPC,
                operation_field: Some("cancel"),
            },
            Case {
                point: FailPoint::Wait(FaultWaitStage::CancelTerminal),
                created: OPC,
                operation_field: Some("cancel"),
            },
            Case {
                point: FailPoint::Close(FaultRole::Cancel),
                created: OPC,
                operation_field: Some("cancel"),
            },
            Case {
                point: FailPoint::Create(FaultRole::Deadline),
                created: OPC,
                operation_field: Some("cancel"),
            },
            Case {
                point: FailPoint::ConnectAdmit(FaultRole::Deadline),
                created: OPCD,
                operation_field: None,
            },
            Case {
                point: FailPoint::Wait(FaultWaitStage::Deadline),
                created: OPCD,
                operation_field: Some("deadline"),
            },
            Case {
                point: FailPoint::Close(FaultRole::Deadline),
                created: OPCD,
                operation_field: Some("deadline"),
            },
        ];

        for case in cases {
            let script = FakeScript {
                failure: Some(case.point),
                interrupt_at: None,
                additional_close_failure: None,
                recovery_settle_failure: false,
                final_native_attempt: false,
                nonterminal_wait: None,
            };
            let (result, trace) = run_fake(script);
            assert_eq!(
                result["execution_error"]["stage"],
                case.point.stage(),
                "{:?}",
                case.point
            );
            let failed_scenario = scenario_for_stage(case.point.stage());
            assert_eq!(
                result["scenarios"][failed_scenario]["failure_stage"],
                case.point.stage(),
                "{:?}",
                case.point
            );
            let scenarios = ["port_occupied", "cancel", "deadline"];
            let failed_index = scenarios
                .iter()
                .position(|scenario| *scenario == failed_scenario)
                .expect("known fault scenario");
            for scenario in &scenarios[failed_index + 1..] {
                assert_eq!(
                    result["scenarios"][*scenario]["reason"],
                    json!({
                        "kind": "prior_failure",
                        "stage": case.point.stage(),
                    }),
                    "{:?}",
                    case.point
                );
            }
            assert_closed_once(&trace, case.created);
            let mut attempted = case.created.to_vec();
            if let FailPoint::Create(role) = case.point {
                attempted.push(role);
            }
            assert_eq!(trace.attempted, attempted, "{:?}", case.point);

            for role in [
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
                FaultRole::Deadline,
            ] {
                let created = case.created.contains(&role);
                assert_eq!(
                    result["resources"][role.resource_name()]["created"],
                    created,
                    "{:?}",
                    case.point
                );
                assert_eq!(
                    result.get(role.cleanup_field()).is_some(),
                    created,
                    "{:?}",
                    case.point
                );
            }
            if let Some(field) = case.operation_field {
                assert!(result.get(field).is_some(), "{:?} lost {field}", case.point);
            }
            if case.created.contains(&FaultRole::OccupiedProbe) {
                assert!(
                    result["port_occupied_native_open_attempts"].is_array(),
                    "{:?}",
                    case.point
                );
            }
            for role in case.created {
                assert!(
                    result[role.identity_attempts_field()].is_array(),
                    "{:?} lost {}",
                    case.point,
                    role.identity_attempts_field()
                );
            }

            let cleanup_failed = matches!(case.point, FailPoint::Close(_));
            assert_eq!(
                cleanup_contract_succeeded("faults", &result),
                !cleanup_failed,
                "{:?}",
                case.point
            );
            let (document, failure) = finalize_result("faults", Ok(result));
            assert!(failure.is_some(), "{:?}", case.point);
            assert_eq!(document["status"], "failed", "{:?}", case.point);
            assert_eq!(document["execution_status"], "failed", "{:?}", case.point);
            assert_eq!(
                document["qualification_status"], "failed",
                "{:?}",
                case.point
            );
            assert_eq!(document_exit_code(&document), 1, "{:?}", case.point);
            assert_eq!(
                document["checks"][1],
                qualification_check(
                    "cleanup_evidence",
                    if cleanup_failed {
                        "incomplete_or_failed"
                    } else {
                        "passed"
                    }
                ),
                "{:?}",
                case.point
            );
        }
    }

    #[test]
    fn cancel_wait_error_remains_authoritative_when_recovery_close_also_fails() {
        let script = FakeScript {
            failure: Some(FailPoint::Wait(FaultWaitStage::CancelTerminal)),
            interrupt_at: None,
            additional_close_failure: Some(FaultRole::Cancel),
            recovery_settle_failure: false,
            final_native_attempt: false,
            nonterminal_wait: None,
        };
        let (result, trace) = run_fake(script);

        assert_eq!(result["execution_error"]["stage"], "cancel_terminal_wait");
        assert_eq!(result["cleanup_errors"][0]["stage"], "cancel_close");
        assert_eq!(result["cancel_request_outcome"], "Applied");
        assert_eq!(result["recovery_cancel_outcome"], "Unchanged");
        assert_eq!(
            result["active_operation_at_failure"]["operation"]["state"],
            "Cancelling"
        );
        assert_eq!(result["cancel"]["state"], "Cancelled");
        assert_eq!(result["cancel_cleanup"]["succeeded"], false);
        assert_eq!(
            trace.attempted,
            [
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
            ]
        );
        assert_closed_once(
            &trace,
            &[
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
            ],
        );
        let (document, failure) = finalize_result("faults", Ok(result));
        assert_eq!(
            failure.as_deref(),
            Some("cancel_terminal_wait: injected cancel_terminal_wait")
        );
        assert_eq!(document["status"], "failed");
        assert_eq!(document["execution_status"], "failed");
        assert_eq!(document["qualification_status"], "failed");
        assert_eq!(document_exit_code(&document), 1);
    }

    #[test]
    fn recovery_settle_timeout_retains_operation_until_owner_close() {
        let script = FakeScript {
            failure: Some(FailPoint::Wait(FaultWaitStage::CancelTerminal)),
            interrupt_at: None,
            additional_close_failure: None,
            recovery_settle_failure: true,
            final_native_attempt: false,
            nonterminal_wait: None,
        };
        let (result, trace) = run_fake(script);

        assert_eq!(result["execution_error"]["stage"], "cancel_terminal_wait");
        assert_eq!(result["recovery_settle_error"], "injected recovery_settle");
        assert_eq!(
            result["active_operation_at_failure"]["operation"]["state"],
            "Cancelling"
        );
        assert_eq!(result["cancel"]["state"], "Cancelled");
        assert_eq!(result["cancel_cleanup"]["succeeded"], true);
        assert_closed_once(
            &trace,
            &[
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
            ],
        );
    }

    #[test]
    fn close_refreshes_native_attempts_after_wait_failure() {
        let script = FakeScript {
            failure: Some(FailPoint::Wait(FaultWaitStage::OccupiedProbe)),
            interrupt_at: None,
            additional_close_failure: None,
            recovery_settle_failure: false,
            final_native_attempt: true,
            nonterminal_wait: None,
        };
        let (result, _) = run_fake(script);

        assert_eq!(
            result["execution_error"]["stage"],
            "occupied_probe_connect_wait"
        );
        assert_eq!(
            result["port_occupied_native_open_attempts"]
                .as_array()
                .expect("native attempts")
                .len(),
            3
        );
        assert_eq!(
            result["port_occupied_native_open_attempts"][2]["error"]["os_code"],
            995
        );
    }

    #[test]
    fn nonterminal_cancel_wait_keeps_operation_owned_and_uses_precise_stage() {
        let script = FakeScript {
            failure: None,
            interrupt_at: None,
            additional_close_failure: None,
            recovery_settle_failure: false,
            final_native_attempt: false,
            nonterminal_wait: Some(FaultWaitStage::CancelTerminal),
        };
        let (result, trace) = run_fake(script);

        assert_eq!(
            result["execution_error"]["stage"],
            "cancel_operation_terminal"
        );
        assert_eq!(
            result["active_operation_at_failure"]["operation"]["state"],
            "Cancelling"
        );
        assert_eq!(result["cancel"]["state"], "Cancelled");
        assert_closed_once(
            &trace,
            &[
                FaultRole::Occupier,
                FaultRole::OccupiedProbe,
                FaultRole::Cancel,
            ],
        );
    }
}
