use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerLeaseState, ControllerOptions, ControllerSession,
    ControllerState, PreciseSequence, SequenceStep, SwitchReport, TransportError,
    TransportErrorKind, WriteKind,
};
use easycon_model::{Button, ErrorCode, Hat, StickPosition};
use easycon_runtime::{
    Event, Operation, OperationState, Runtime, RuntimeCounts, SubscriptionOptions,
    SubscriptionRead, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_test_support::{AckOutcome, FakeControllerTransport, HandshakeOutcome};
use serde_json::Value;

struct SequenceTrace {
    lane_start_ns: u64,
    minimum_report_interval_ns: u64,
    cancel_at_ns: Option<u64>,
    steps: Vec<SequenceStep>,
    expected_reports: Vec<ExpectedReport>,
    operation_states: Vec<String>,
    terminal_preconditions: Vec<String>,
}

struct ExpectedReport {
    timestamp_ns: u64,
    bytes: [u8; 8],
    purpose: Option<String>,
}

fn connected() -> (
    Arc<VirtualClock>,
    Runtime,
    FakeControllerTransport,
    ControllerSession,
) {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let controller = ControllerSession::new(
        runtime.clone(),
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("controller");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect");
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Succeeded);
    (clock, runtime, fake, controller)
}

fn wait_terminal(operation: &Operation) {
    assert!(matches!(
        operation.wait(WaitTimeout::For(Duration::from_secs(2))),
        WaitResult::Completed(_)
    ));
}

fn wait_until(mut predicate: impl FnMut() -> bool) {
    let started = Instant::now();
    while !predicate() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "condition timed out"
        );
        std::thread::yield_now();
    }
}

fn load_sequence_trace(id: &str) -> SequenceTrace {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../spec/fixtures/controller/sequence-traces-v1.json"
    );
    let document: Value =
        serde_json::from_str(&fs::read_to_string(path).expect("trace read")).expect("trace JSON");
    let trace = document["traces"]
        .as_array()
        .expect("trace array")
        .iter()
        .find(|trace| trace["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("missing sequence trace {id}"));
    SequenceTrace {
        lane_start_ns: json_u64(&trace["lane_start_ns"]),
        minimum_report_interval_ns: json_u64(&trace["minimum_report_interval_ns"]),
        cancel_at_ns: trace.get("cancel_at_ns").map(json_u64),
        steps: trace["steps"]
            .as_array()
            .expect("steps")
            .iter()
            .map(|step| SequenceStep::new(json_u64(&step["offset_ns"]), fixture_action(step)))
            .collect(),
        expected_reports: trace["expected_reports"]
            .as_array()
            .expect("expected reports")
            .iter()
            .map(|report| ExpectedReport {
                timestamp_ns: json_u64(&report["timestamp_ns"]),
                bytes: report["bytes"]
                    .as_array()
                    .expect("report bytes")
                    .iter()
                    .map(json_u8)
                    .collect::<Vec<_>>()
                    .try_into()
                    .expect("eight report bytes"),
                purpose: report
                    .get("purpose")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
            .collect(),
        operation_states: trace["operation_states"]
            .as_array()
            .expect("operation states")
            .iter()
            .map(|state| state.as_str().expect("operation state").to_owned())
            .collect(),
        terminal_preconditions: trace
            .get("terminal_preconditions")
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |items| {
                items
                    .iter()
                    .map(|item| item.as_str().expect("terminal precondition").to_owned())
                    .collect()
            }),
    }
}

fn fixture_action(step: &Value) -> ControllerAction {
    match step["action"].as_str().expect("action") {
        "button_down" => ControllerAction::ButtonDown(fixture_button(&step["button"])),
        "button_up" => ControllerAction::ButtonUp(fixture_button(&step["button"])),
        "hat" => ControllerAction::Hat(fixture_hat(&step["hat"])),
        "left_stick" => ControllerAction::LeftStick(StickPosition::new(
            json_u8(&step["x"]),
            json_u8(&step["y"]),
        )),
        "right_stick" => ControllerAction::RightStick(StickPosition::new(
            json_u8(&step["x"]),
            json_u8(&step["y"]),
        )),
        "reset" => ControllerAction::Reset,
        action => panic!("unknown fixture action {action}"),
    }
}

fn fixture_button(value: &Value) -> Button {
    match value.as_str().expect("button") {
        "Y" => Button::Y,
        "B" => Button::B,
        "A" => Button::A,
        "X" => Button::X,
        "L" => Button::L,
        "R" => Button::R,
        "ZL" => Button::ZL,
        "ZR" => Button::ZR,
        "MINUS" => Button::Minus,
        "PLUS" => Button::Plus,
        "LCLICK" => Button::LClick,
        "RCLICK" => Button::RClick,
        "HOME" => Button::Home,
        "CAPTURE" => Button::Capture,
        button => panic!("unknown fixture button {button}"),
    }
}

fn fixture_hat(value: &Value) -> Hat {
    match value.as_str().expect("HAT") {
        "TOP" => Hat::Top,
        "TOP_RIGHT" => Hat::TopRight,
        "RIGHT" => Hat::Right,
        "BOTTOM_RIGHT" => Hat::BottomRight,
        "BOTTOM" => Hat::Bottom,
        "BOTTOM_LEFT" => Hat::BottomLeft,
        "LEFT" => Hat::Left,
        "TOP_LEFT" => Hat::TopLeft,
        "CENTER" => Hat::Center,
        hat => panic!("unknown fixture HAT {hat}"),
    }
}

fn json_u64(value: &Value) -> u64 {
    value.as_u64().expect("fixture u64")
}

fn json_u8(value: &Value) -> u8 {
    u8::try_from(json_u64(value)).expect("fixture byte")
}

fn drain_events(subscription: &easycon_runtime::EventSubscription) -> Vec<Event> {
    std::iter::from_fn(|| match subscription.read(WaitTimeout::Poll) {
        SubscriptionRead::Event(event) => Some(event),
        SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
    })
    .collect()
}

#[test]
fn precise_sequence_matches_absolute_offset_fixture_without_drift() {
    let (clock, runtime, fake, controller) = connected();
    let trace = load_sequence_trace("absolute-offsets-and-same-offset-merge");
    assert_eq!(
        trace.minimum_report_interval_ns,
        ControllerOptions::default().minimum_report_interval_ns
    );
    clock.advance_to(trace.lane_start_ns);
    let sequence = PreciseSequence::new(trace.steps).expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");

    assert!(fake.wait_for_accepted_count(1, Duration::from_secs(2)));
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);
    for (index, report) in trace.expected_reports.iter().enumerate().skip(1) {
        clock.advance_to(report.timestamp_ns);
        assert!(fake.wait_for_accepted_count(index + 1, Duration::from_secs(2)));
    }
    wait_terminal(&operation);

    let writes = fake.accepted_writes();
    assert_eq!(
        writes
            .iter()
            .map(|write| write.context.timestamp_ns)
            .collect::<Vec<_>>(),
        trace
            .expected_reports
            .iter()
            .map(|report| report.timestamp_ns)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        writes
            .iter()
            .map(|write| write.bytes.as_slice())
            .collect::<Vec<_>>(),
        trace
            .expected_reports
            .iter()
            .map(|report| report.bytes.as_slice())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        trace.operation_states.last().map(String::as_str),
        Some("Succeeded")
    );
    assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);

    controller.close();
    runtime.close();
}

#[test]
fn sequence_cancel_neutralizes_and_releases_before_terminal() {
    let (clock, runtime, fake, controller) = connected();
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let trace = load_sequence_trace("cancel-before-future-step-neutralizes");
    clock.advance_to(trace.lane_start_ns);
    let sequence = PreciseSequence::new(trace.steps).expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    assert!(fake.wait_for_accepted_count(1, Duration::from_secs(2)));

    let busy = controller
        .direct(ControllerAction::ButtonDown(Button::X))
        .expect("busy operation");
    wait_terminal(&busy);
    assert_eq!(busy.snapshot().state, OperationState::Failed);
    assert_eq!(
        busy.snapshot().error.expect("busy error").code(),
        ErrorCode::ResourceBusy
    );

    clock.advance_to(trace.cancel_at_ns.expect("cancel timestamp"));
    operation.cancel();
    assert_eq!(operation.snapshot().state, OperationState::Cancelling);
    assert!(matches!(
        controller.snapshot().lease,
        ControllerLeaseState::Sequence(id) if id == operation.id()
    ));
    assert_eq!(operation.wait(WaitTimeout::Poll), WaitResult::Timeout);

    clock.advance_to(
        trace
            .expected_reports
            .last()
            .expect("neutral report")
            .timestamp_ns,
    );
    assert!(fake.wait_for_accepted_count(2, Duration::from_secs(2)));
    wait_terminal(&operation);
    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        fake.accepted_writes()[0].bytes,
        trace.expected_reports[0].bytes
    );
    assert_eq!(
        fake.accepted_writes()[1].bytes,
        trace.expected_reports[1].bytes
    );
    assert_eq!(
        trace.expected_reports[1].purpose.as_deref(),
        Some("neutralize")
    );
    assert!(
        trace
            .terminal_preconditions
            .iter()
            .any(|condition| condition == "sequence lease released")
    );
    assert_eq!(
        fake.accepted_writes()[1].context.kind,
        WriteKind::Neutralize
    );
    assert_eq!(controller.snapshot().desired_report, SwitchReport::NEUTRAL);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);

    let observed = drain_events(&events);
    let released = observed
        .iter()
        .position(|event| event.code == "controller.lease.released")
        .expect("lease release event");
    let terminal = observed
        .iter()
        .position(|event| {
            event.code == "runtime.operation.cancelled"
                && event.operation_id == Some(operation.id())
        })
        .expect("cancel terminal event");
    assert!(released < terminal);

    controller.close();
    runtime.close();
}

#[test]
fn minimum_interval_delays_steps_without_dropping_transitions() {
    let (clock, runtime, fake, controller) = connected();
    let sequence = PreciseSequence::new(vec![
        SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
        SequenceStep::new(10_000_000, ControllerAction::ButtonDown(Button::B)),
        SequenceStep::new(20_000_000, ControllerAction::ButtonUp(Button::A)),
    ])
    .expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    assert!(fake.wait_for_accepted_count(1, Duration::from_secs(2)));
    clock.advance_to(30_000_000);
    assert!(fake.wait_for_accepted_count(2, Duration::from_secs(2)));
    clock.advance_to(60_000_000);
    assert!(fake.wait_for_accepted_count(3, Duration::from_secs(2)));
    wait_terminal(&operation);

    assert_eq!(
        fake.accepted_writes()
            .iter()
            .map(|write| write.context.timestamp_ns)
            .collect::<Vec<_>>(),
        [0, 30_000_000, 60_000_000]
    );
    assert_eq!(fake.accepted_writes()[2].bytes, [0, 0, 65, 8, 4, 2, 1, 128]);
    controller.close();
    runtime.close();
}

#[test]
fn sequence_future_state_is_not_applied_before_its_offset() {
    let (clock, runtime, fake, controller) = connected();
    let sequence = PreciseSequence::new(vec![SequenceStep::new(
        30_000_000,
        ControllerAction::ButtonDown(Button::A),
    )])
    .expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    wait_until(|| {
        matches!(
            controller.snapshot().lease,
            ControllerLeaseState::Sequence(_)
        )
    });

    assert_eq!(controller.snapshot().desired_report, SwitchReport::NEUTRAL);
    assert!(fake.accepted_writes().is_empty());

    clock.advance_to(30_000_000);
    wait_terminal(&operation);
    assert_eq!(
        controller.snapshot().desired_report,
        SwitchReport::new(
            Button::A.mask(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
    );
    controller.close();
    runtime.close();
}

#[test]
fn cancel_after_final_sequence_acceptance_still_neutralizes() {
    let (clock, runtime, fake, controller) = connected();
    fake.block_next_write_after_acceptance();
    let sequence = PreciseSequence::new(vec![SequenceStep::new(
        0,
        ControllerAction::ButtonDown(Button::A),
    )])
    .expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    assert!(fake.wait_until_write_blocked(Duration::from_secs(2)));
    assert_eq!(fake.accepted_writes().len(), 1);

    operation.cancel();
    clock.advance_to(30_000_000);
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    assert_eq!(fake.accepted_writes().len(), 2);
    assert_eq!(
        fake.accepted_writes()[1].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    controller.close();
    runtime.close();
}

#[test]
fn cancel_after_direct_acceptance_still_reaches_cancelled() {
    let (clock, runtime, fake, controller) = connected();
    fake.block_next_write_after_acceptance();
    let operation = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    assert!(fake.wait_until_write_blocked(Duration::from_secs(2)));

    operation.cancel();
    clock.advance_to(30_000_000);
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(fake.accepted_writes().len(), 2);
    assert_eq!(
        fake.accepted_writes()[1].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    controller.close();
    runtime.close();
}

#[test]
fn direct_cancel_rebuilds_later_reports_from_neutral() {
    let (clock, runtime, fake, controller) = connected();
    let first = controller
        .direct(ControllerAction::ButtonDown(Button::X))
        .expect("first direct");
    wait_terminal(&first);
    let cancelled = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("cancelled direct");
    let later = controller
        .direct(ControllerAction::ButtonDown(Button::B))
        .expect("later direct");
    wait_until(|| {
        controller.snapshot().desired_report.buttons()
            == Button::X.mask() | Button::A.mask() | Button::B.mask()
    });

    cancelled.cancel();
    clock.advance_to(30_000_000);
    wait_terminal(&cancelled);
    assert_eq!(cancelled.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        fake.accepted_writes()[1].bytes,
        SwitchReport::NEUTRAL.encode()
    );

    clock.advance_to(60_000_000);
    wait_terminal(&later);
    assert_eq!(later.snapshot().state, OperationState::Succeeded);
    assert_eq!(
        fake.accepted_writes()[2].bytes,
        SwitchReport::new(
            Button::B.mask(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .encode()
    );
    controller.close();
    runtime.close();
}

#[test]
fn ack_waits_for_an_earlier_report_in_the_fifo_lane() {
    let (clock, runtime, fake, controller) = connected();
    let first = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("first direct");
    wait_terminal(&first);
    let pending = controller
        .direct(ControllerAction::ButtonDown(Button::B))
        .expect("pending direct");
    fake.push_ack(AckOutcome::Frame {
        generation: 1,
        byte: 0xff,
        elapsed_ns: 0,
    });
    let ack = controller
        .command_with_ack(Arc::<[u8]>::from([0xA5, 0x91]), 0xff, 100)
        .expect("ACK command");
    assert_eq!(ack.wait(WaitTimeout::Poll), WaitResult::Timeout);

    clock.advance_to(30_000_000);
    wait_terminal(&pending);
    wait_terminal(&ack);

    assert_eq!(ack.snapshot().state, OperationState::Succeeded);
    assert_eq!(
        fake.accepted_writes()
            .iter()
            .map(|write| write.context.kind)
            .collect::<Vec<_>>(),
        [WriteKind::Report, WriteKind::Report, WriteKind::Command]
    );
    controller.close();
    runtime.close();
}

#[test]
fn close_interrupts_a_blocked_report_write_and_joins() {
    let (_clock, runtime, fake, controller) = connected();
    fake.block_next_write_before_acceptance();
    let operation = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    assert!(fake.wait_until_write_blocked(Duration::from_secs(2)));

    controller.close();
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    assert!(
        fake.accepted_writes()
            .iter()
            .all(|write| write.bytes == SwitchReport::NEUTRAL.encode())
    );
    runtime.close();
}

#[test]
fn blocked_report_write_obeys_its_io_deadline() {
    let (clock, runtime, fake, controller) = connected();
    fake.block_next_write_before_acceptance();
    let operation = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    assert!(fake.wait_until_write_blocked(Duration::from_secs(2)));

    clock.advance_to(ControllerOptions::default().write_timeout_ns);
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Failed);
    assert_eq!(
        operation.snapshot().error.expect("write timeout").code(),
        ErrorCode::Transport
    );
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    controller.close();
    runtime.close();
}

#[test]
fn runtime_close_does_not_execute_direct_queued_behind_ack() {
    let (_clock, runtime, fake, controller) = connected();
    fake.push_ack(AckOutcome::BlockUntilCancelled);
    let ack = controller
        .command_with_ack(Arc::<[u8]>::from([0xA5, 0x91]), 0xff, 100)
        .expect("ACK command");
    assert!(fake.wait_until_ack_blocked(Duration::from_secs(2)));
    let direct = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("queued direct");

    runtime.close();

    wait_terminal(&ack);
    wait_terminal(&direct);
    assert_eq!(ack.snapshot().state, OperationState::Cancelled);
    assert_eq!(direct.snapshot().state, OperationState::Cancelled);
    assert!(
        fake.accepted_writes()
            .iter()
            .all(|write| write.context.kind != WriteKind::Report)
    );
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 0,
        }
    );
}

#[test]
fn automation_lease_is_a_low_level_exclusive_primitive() {
    let (_clock, runtime, fake, controller) = connected();
    let lease = controller.acquire_automation_lease().expect("lease");
    assert_eq!(
        controller.snapshot().lease,
        ControllerLeaseState::Automation(lease.lease_id())
    );

    let direct = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    wait_terminal(&direct);
    assert_eq!(
        direct.snapshot().error.expect("busy").code(),
        ErrorCode::ResourceBusy
    );

    let authorized = controller
        .direct_with_lease(&lease, ControllerAction::ButtonDown(Button::B))
        .expect("authorized direct");
    wait_terminal(&authorized);
    assert_eq!(authorized.snapshot().state, OperationState::Succeeded);
    assert_eq!(fake.accepted_writes().len(), 1);

    let sequence = PreciseSequence::new(vec![SequenceStep::new(0, ControllerAction::Reset)])
        .expect("sequence");
    let busy_sequence = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    wait_terminal(&busy_sequence);
    assert_eq!(
        busy_sequence.snapshot().error.expect("busy").code(),
        ErrorCode::ResourceBusy
    );

    drop(lease);
    let second = controller
        .acquire_automation_lease()
        .expect("released then reacquired");
    drop(second);
    controller.close();
    runtime.close();
}

#[test]
fn ack_matcher_ignores_late_generation_and_rejects_wrong_reply() {
    let (_clock, runtime, fake, controller) = connected();
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    fake.push_ack(AckOutcome::Timeout { elapsed_ns: 10 });
    let first = controller
        .command_with_ack(Arc::<[u8]>::from([0xA5, 0x90]), 0xff, 100)
        .expect("first ACK command");
    wait_terminal(&first);
    assert_eq!(
        first.snapshot().error.expect("timeout").code(),
        ErrorCode::ProtocolTimeout
    );

    fake.push_ack(AckOutcome::Frame {
        generation: 1,
        byte: 0xff,
        elapsed_ns: 0,
    });
    fake.push_ack(AckOutcome::Frame {
        generation: 2,
        byte: 0xff,
        elapsed_ns: 0,
    });
    let second = controller
        .command_with_ack(Arc::<[u8]>::from([0xA5, 0x91]), 0xff, 100)
        .expect("second ACK command");
    wait_terminal(&second);
    assert_eq!(second.snapshot().state, OperationState::Succeeded);

    fake.push_ack(AckOutcome::Frame {
        generation: 3,
        byte: 0,
        elapsed_ns: 0,
    });
    let wrong = controller
        .command_with_ack(Arc::<[u8]>::from([0xA5, 0x92]), 0xff, 100)
        .expect("wrong ACK command");
    wait_terminal(&wrong);
    assert_eq!(
        wrong.snapshot().error.expect("protocol").code(),
        ErrorCode::ProtocolError
    );
    assert!(drain_events(&events).iter().any(|event| {
        event.code == "controller.ack.late_ignored" && event.operation_id == Some(second.id())
    }));
    assert!(
        fake.accepted_writes()
            .iter()
            .all(|write| write.context.kind == WriteKind::Command)
    );

    controller.close();
    runtime.close();
}

#[test]
fn close_cancels_blocked_ack_and_joins_lane() {
    let (_clock, runtime, fake, controller) = connected();
    fake.push_ack(AckOutcome::BlockUntilCancelled);
    let operation = controller
        .command_with_ack(Arc::<[u8]>::from([0xA5, 0x91]), 0xff, 100)
        .expect("ACK command");
    assert!(fake.wait_until_ack_blocked(Duration::from_secs(2)));

    controller.close();
    wait_terminal(&operation);
    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 1,
        }
    );
    runtime.close();
}

#[test]
fn sequence_disconnect_releases_lease_and_exposes_neutral_warning() {
    let (clock, runtime, fake, controller) = connected();
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let sequence = PreciseSequence::new(vec![
        SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
        SequenceStep::new(30_000_000, ControllerAction::ButtonDown(Button::B)),
    ])
    .expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    assert!(fake.wait_for_accepted_count(1, Duration::from_secs(2)));
    fake.disconnect();
    clock.advance_to(30_000_000);
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Failed);
    assert_eq!(
        operation.snapshot().error.expect("disconnect").code(),
        ErrorCode::DeviceDisconnected
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    assert!(
        drain_events(&events)
            .iter()
            .any(|event| event.code == "controller.neutralization.not_delivered")
    );
    controller.close();
    runtime.close();
}

#[test]
fn recoverable_sequence_write_failure_accepts_neutral_before_failed() {
    let (_clock, runtime, fake, controller) = connected();
    fake.fail_write_call(
        1,
        TransportError::new(TransportErrorKind::Io, "scripted report failure"),
    );
    let sequence = PreciseSequence::new(vec![SequenceStep::new(
        0,
        ControllerAction::ButtonDown(Button::A),
    )])
    .expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Failed);
    assert_eq!(
        operation.snapshot().error.expect("I/O").code(),
        ErrorCode::Transport
    );
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    assert_eq!(
        fake.accepted_writes()[0].context.kind,
        WriteKind::Neutralize
    );
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    controller.close();
    runtime.close();
}

#[test]
fn sequence_target_overflow_uses_the_same_neutral_failure_guard() {
    let (clock, runtime, fake, controller) = connected();
    clock.advance_to(u64::MAX - 1);
    let sequence = PreciseSequence::new(vec![SequenceStep::new(
        2,
        ControllerAction::ButtonDown(Button::A),
    )])
    .expect("sequence");
    let operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Failed);
    assert_eq!(
        operation.snapshot().error.expect("overflow").code(),
        ErrorCode::InvalidArgument
    );
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    controller.close();
    runtime.close();
}
