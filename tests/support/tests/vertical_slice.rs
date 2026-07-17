use std::sync::Arc;
use std::time::{Duration, Instant};

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerLeaseState, ControllerOptions, ControllerSession,
    ControllerState, PreciseSequence, SequenceStep, SwitchReport, WriteKind,
};
use easycon_model::{Button, Hat, StickPosition};
use easycon_runtime::{
    Event, EventKind, Operation, OperationState, Runtime, RuntimeCounts, SubscriptionOptions,
    SubscriptionRead, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_test_support::{FakeControllerTransport, HandshakeOutcome};

fn wait_terminal(operation: &Operation) {
    assert!(matches!(
        operation.wait(WaitTimeout::For(Duration::from_secs(2))),
        WaitResult::Completed(_)
    ));
}

fn wait_for_report_acceptance(controller: &ControllerSession, count: u64) {
    let started = Instant::now();
    while controller.snapshot().accepted_report_count < count {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "controller acceptance timed out"
        );
        std::thread::yield_now();
    }
}

#[test]
fn runtime_controller_fake_vertical_slice_has_exact_trace_and_clean_shutdown() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let events = runtime
        .subscribe(SubscriptionOptions {
            capacity: 64,
            ..SubscriptionOptions::default()
        })
        .expect("events");
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    fake.set_maximum_write_chunk(2);
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
    let direct = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    assert!(fake.wait_for_accepted_count(1, Duration::from_secs(2)));
    wait_terminal(&direct);

    clock.advance_to(30_000_000);
    let sequence = PreciseSequence::new(vec![
        SequenceStep::new(0, ControllerAction::ButtonUp(Button::A)),
        SequenceStep::new(0, ControllerAction::Hat(Hat::Right)),
        SequenceStep::new(
            40_000_000,
            ControllerAction::LeftStick(StickPosition::new(0, 255)),
        ),
        SequenceStep::new(80_000_000, ControllerAction::ButtonDown(Button::B)),
    ])
    .expect("sequence");
    let sequence_operation = controller
        .precise_sequence(sequence)
        .expect("sequence submit");
    wait_for_report_acceptance(&controller, 2);
    clock.advance_to(70_000_000);
    wait_for_report_acceptance(&controller, 3);

    clock.advance_to(80_000_000);
    sequence_operation.cancel();
    assert_eq!(
        sequence_operation.snapshot().state,
        OperationState::Cancelling
    );
    assert!(matches!(
        controller.snapshot().lease,
        ControllerLeaseState::Sequence(id) if id == sequence_operation.id()
    ));
    clock.advance_to(100_000_000);
    wait_for_report_acceptance(&controller, 4);
    wait_terminal(&sequence_operation);
    assert_eq!(
        sequence_operation.snapshot().state,
        OperationState::Cancelled
    );
    assert_eq!(controller.snapshot().desired_report, SwitchReport::NEUTRAL);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);

    clock.advance_to(130_000_000);
    runtime.close();
    runtime.close();

    assert_eq!(connect.snapshot().state, OperationState::Succeeded);
    assert_eq!(direct.snapshot().state, OperationState::Succeeded);
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    assert_eq!(controller.snapshot().accepted_report_count, 5);
    assert_eq!(
        controller.snapshot().last_report_timestamp_ns,
        Some(130_000_000)
    );
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 0,
        }
    );

    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), 5);
    assert_eq!(
        writes
            .iter()
            .map(|write| write.context.timestamp_ns)
            .collect::<Vec<_>>(),
        [0, 30_000_000, 70_000_000, 100_000_000, 130_000_000]
    );
    assert_eq!(writes[0].bytes, [0, 1, 1, 8, 4, 2, 1, 128]);
    assert_eq!(writes[1].bytes, [0, 0, 0, 40, 4, 2, 1, 128]);
    assert_eq!(writes[2].bytes, [0, 0, 0, 32, 7, 126, 1, 128]);
    assert_eq!(writes[3].bytes, SwitchReport::NEUTRAL.encode());
    assert_eq!(writes[3].context.kind, WriteKind::Neutralize);
    assert_eq!(writes[4].bytes, SwitchReport::NEUTRAL.encode());
    assert_eq!(writes[4].context.operation_id, None);
    assert!(
        writes
            .iter()
            .all(|write| write.writer_thread == writes[0].writer_thread)
    );

    let mut observed = Vec::new();
    loop {
        match events.read(WaitTimeout::Poll) {
            SubscriptionRead::Event(event) => observed.push(event),
            SubscriptionRead::Closed => break,
            SubscriptionRead::Timeout => panic!("closed subscription must drain then close"),
        }
    }
    assert!(
        observed
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
    assert_eq!(
        observed.last().map(|event| event.code),
        Some("runtime.closed")
    );
    assert_event_order(
        &observed,
        "controller.report.transport_accepted",
        "runtime.operation.succeeded",
        direct.id(),
    );
    assert_event_order(
        &observed,
        "controller.lease.released",
        "runtime.operation.cancelled",
        sequence_operation.id(),
    );
    assert!(observed.iter().any(|event| {
        event.code == "controller.report.transport_accepted"
            && event.operation_id == Some(direct.id())
            && event
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("hardware_execution=false"))
    }));
    assert!(
        !observed
            .iter()
            .any(|event| matches!(event.kind, EventKind::Gap(_)))
    );
}

fn assert_event_order(
    events: &[Event],
    earlier_code: &'static str,
    terminal_code: &'static str,
    operation_id: easycon_model::OperationId,
) {
    let earlier = events
        .iter()
        .position(|event| {
            event.code == earlier_code
                && (earlier_code == "controller.lease.released"
                    || event.operation_id == Some(operation_id))
        })
        .expect("earlier event");
    let terminal = events
        .iter()
        .position(|event| event.code == terminal_code && event.operation_id == Some(operation_id))
        .expect("terminal event");
    assert!(earlier < terminal);
}
