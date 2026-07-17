use std::sync::Arc;
use std::sync::Barrier;
use std::time::{Duration, Instant};

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerOptions, ControllerSession, ControllerState,
    SwitchReport, WriteKind,
};
use easycon_model::{Button, ErrorCode, Hat, StickPosition};
use easycon_runtime::{
    Clock, OperationState, Runtime, RuntimeCounts, SubscriptionOptions, SubscriptionRead,
    TransitionOutcome, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_test_support::{FakeControllerTransport, HandshakeOutcome};

fn wait_terminal(operation: &easycon_runtime::Operation) {
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
fn fallback_connect_partial_direct_and_close_are_exact() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let events = runtime
        .subscribe(SubscriptionOptions::default())
        .expect("events");
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Timeout { elapsed_ns: 10 });
    fake.push_handshake(9_600, HandshakeOutcome::Success { elapsed_ns: 5 });
    fake.set_maximum_write_chunk(2);
    let controller = ControllerSession::new(
        runtime.clone(),
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("controller");

    let connect = controller
        .connect(ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 100,
        })
        .expect("connect");
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Succeeded);
    assert_eq!(
        fake.handshake_attempts()
            .iter()
            .map(|attempt| attempt.baud_rate)
            .collect::<Vec<_>>(),
        [115_200, 9_600]
    );
    assert_eq!(controller.snapshot().state, ControllerState::Connected);

    let down = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    wait_terminal(&down);
    assert_eq!(down.snapshot().state, OperationState::Succeeded);
    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].bytes,
        SwitchReport::new(
            4,
            Default::default(),
            Default::default(),
            Default::default()
        )
        .encode()
    );
    assert_eq!(writes[0].context.timestamp_ns, 15);
    assert_eq!(writes[0].partial_calls, 4);
    let observed: Vec<_> = std::iter::from_fn(|| match events.read(WaitTimeout::Poll) {
        SubscriptionRead::Event(event) => Some(event),
        SubscriptionRead::Timeout | SubscriptionRead::Closed => None,
    })
    .collect();
    let accepted = observed
        .iter()
        .position(|event| {
            event.code == "controller.report.transport_accepted"
                && event.operation_id == Some(down.id())
        })
        .expect("report acceptance event");
    let terminal = observed
        .iter()
        .position(|event| {
            event.code == "runtime.operation.succeeded" && event.operation_id == Some(down.id())
        })
        .expect("operation terminal event");
    assert!(accepted < terminal);
    assert!(
        observed[accepted]
            .detail
            .as_deref()
            .expect("acceptance detail")
            .contains("hardware_execution=false")
    );

    let up = controller
        .direct(ControllerAction::ButtonUp(Button::A))
        .expect("direct");
    assert_eq!(up.wait(WaitTimeout::Poll), WaitResult::Timeout);
    clock.advance_to(30_000_015);
    wait_terminal(&up);
    assert_eq!(
        fake.accepted_writes()[1].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    assert_eq!(fake.accepted_writes()[1].context.timestamp_ns, 30_000_015);

    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 1,
            active_tasks: 2,
        }
    );
    controller.close();
    assert!(fake.is_closed());
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    let writes = fake.accepted_writes();
    assert_eq!(
        writes.last().expect("close report").context.kind,
        WriteKind::Neutralize
    );
    assert_eq!(
        writes.last().expect("close report").bytes,
        SwitchReport::NEUTRAL.encode()
    );
    assert_eq!(runtime.counts().active_resources, 0);
    assert_eq!(runtime.counts().active_tasks, 1);
    runtime.close();
}

#[test]
fn report_spacing_and_snapshot_use_transport_acceptance_time() {
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

    fake.delay_next_write_by(20_000_000);
    let first = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("first report");
    wait_terminal(&first);
    assert_eq!(clock.now_ns(), 20_000_000);
    assert_eq!(controller.snapshot().accepted_report_count, 1);
    assert_eq!(
        controller.snapshot().last_report_timestamp_ns,
        Some(20_000_000)
    );

    let second = controller
        .direct(ControllerAction::ButtonDown(Button::B))
        .expect("second report");
    clock.advance_to(49_999_999);
    assert_eq!(second.wait(WaitTimeout::Poll), WaitResult::Timeout);
    assert_eq!(fake.accepted_writes().len(), 1);
    clock.advance_to(50_000_000);
    wait_for_report_acceptance(&controller, 2);
    wait_terminal(&second);
    assert_eq!(
        fake.accepted_writes()[1].bytes,
        SwitchReport::new(
            Button::A.mask() | Button::B.mask(),
            Default::default(),
            Default::default(),
            Default::default(),
        )
        .encode()
    );

    controller.close();
    assert_eq!(controller.snapshot().accepted_report_count, 3);
    assert_eq!(
        controller.snapshot().last_report_timestamp_ns,
        Some(50_000_000)
    );
    runtime.close();
}

#[test]
fn hat_sticks_and_reset_all_flow_through_the_report_lane() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 1 });
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

    let actions = [
        ControllerAction::Hat(Hat::Right),
        ControllerAction::LeftStick(StickPosition::new(0, 255)),
        ControllerAction::RightStick(StickPosition::new(255, 0)),
        ControllerAction::Reset,
    ];
    for (index, action) in actions.into_iter().enumerate() {
        let operation = controller.direct(action).expect("direct action");
        if index != 0 {
            clock.advance_by(Duration::from_millis(30));
        }
        wait_for_report_acceptance(&controller, u64::try_from(index + 1).expect("report count"));
        wait_terminal(&operation);
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    }

    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), 4);
    assert_eq!(
        writes[0].bytes,
        SwitchReport::new(0, Hat::Right, Default::default(), Default::default()).encode()
    );
    assert_eq!(
        writes[1].bytes,
        SwitchReport::new(
            0,
            Hat::Right,
            StickPosition::new(0, 255),
            Default::default()
        )
        .encode()
    );
    assert_eq!(
        writes[2].bytes,
        SwitchReport::new(
            0,
            Hat::Right,
            StickPosition::new(0, 255),
            StickPosition::new(255, 0)
        )
        .encode()
    );
    assert_eq!(writes[3].bytes, SwitchReport::NEUTRAL.encode());

    controller.close();
    runtime.close();
}

#[test]
fn handshake_protocol_timeout_is_failed_not_cancelled() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Timeout { elapsed_ns: 10 });
    fake.push_handshake(9_600, HandshakeOutcome::Timeout { elapsed_ns: 10 });
    let controller = ControllerSession::new(
        runtime.clone(),
        Box::new(fake),
        ControllerOptions::default(),
    )
    .expect("controller");

    let connect = controller
        .connect(ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 100,
        })
        .expect("connect");
    wait_terminal(&connect);
    let snapshot = connect.snapshot();
    assert_eq!(snapshot.state, OperationState::Failed);
    assert_eq!(
        snapshot.error.expect("timeout error").code(),
        ErrorCode::ProtocolTimeout
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    controller.close();
    runtime.close();
}

#[test]
fn operation_deadline_cancels_connect_before_second_baud_attempt() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(115_200, HandshakeOutcome::Timeout { elapsed_ns: 100 });
    let controller = ControllerSession::new(
        runtime.clone(),
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("controller");

    let connect = controller
        .connect(ConnectOptions {
            operation_deadline_ns: Some(50),
            protocol_timeout_ns: 1_000,
        })
        .expect("connect");
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Cancelled);
    assert_eq!(
        connect.snapshot().error.expect("deadline").code(),
        ErrorCode::DeadlineExceeded
    );
    assert_eq!(fake.handshake_attempts().len(), 1);
    controller.close();
    runtime.close();
}

#[test]
fn handshake_protocol_errors_use_bounded_fallback_then_fail() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(
        115_200,
        HandshakeOutcome::Error {
            kind: easycon_controller::TransportErrorKind::Protocol,
            message: "wrong hello".into(),
        },
    );
    fake.push_handshake(
        9_600,
        HandshakeOutcome::Error {
            kind: easycon_controller::TransportErrorKind::Protocol,
            message: "wrong hello".into(),
        },
    );
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
    assert_eq!(connect.snapshot().state, OperationState::Failed);
    assert_eq!(
        connect.snapshot().error.expect("protocol").code(),
        ErrorCode::ProtocolError
    );
    assert_eq!(fake.handshake_attempts().len(), 2);
    controller.close();
    runtime.close();
}

#[test]
fn direct_while_disconnected_fails_without_transport_write() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    let controller = ControllerSession::new(
        runtime.clone(),
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("controller");

    let action = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct");
    wait_terminal(&action);
    assert_eq!(action.snapshot().state, OperationState::Failed);
    assert!(fake.accepted_writes().is_empty());
    controller.close();
    runtime.close();
}

#[test]
fn concurrent_callers_still_use_one_writer_thread() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 1 });
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

    let callers: Vec<_> = Button::ALL[..4]
        .iter()
        .copied()
        .map(|button| {
            let controller = controller.clone();
            std::thread::spawn(move || {
                controller
                    .direct(ControllerAction::ButtonDown(button))
                    .expect("direct")
            })
        })
        .collect();
    let operations: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("caller"))
        .collect();
    for accepted in 1..=4 {
        clock.advance_by(Duration::from_millis(30));
        wait_for_report_acceptance(&controller, accepted);
    }
    for operation in &operations {
        wait_terminal(operation);
        assert_eq!(operation.snapshot().state, OperationState::Succeeded);
        assert_eq!(operation.cancel(), TransitionOutcome::AlreadyTerminal);
    }

    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), 4);
    assert!(
        writes
            .iter()
            .all(|write| write.writer_thread == writes[0].writer_thread)
    );
    assert!(
        writes
            .windows(2)
            .all(|pair| pair[1].context.timestamp_ns - pair[0].context.timestamp_ns >= 30_000_000)
    );

    controller.close();
    runtime.close();
}

#[test]
fn controller_construction_and_runtime_close_have_no_half_admitted_state() {
    for _ in 0..32 {
        let clock = Arc::new(VirtualClock::default());
        let runtime = Runtime::new(clock.clone());
        let fake = FakeControllerTransport::new(clock);
        let barrier = Arc::new(Barrier::new(3));

        let creator_runtime = runtime.clone();
        let creator_barrier = barrier.clone();
        let creator = std::thread::spawn(move || {
            creator_barrier.wait();
            ControllerSession::new(
                creator_runtime,
                Box::new(fake),
                ControllerOptions::default(),
            )
        });
        let closer_runtime = runtime.clone();
        let closer_barrier = barrier.clone();
        let closer = std::thread::spawn(move || {
            closer_barrier.wait();
            closer_runtime.close();
        });
        barrier.wait();

        let controller = creator.join().expect("creator thread");
        closer.join().expect("closer thread");
        if let Ok(controller) = controller {
            controller.close();
            assert_eq!(controller.snapshot().state, ControllerState::Closed);
        }
        assert_eq!(runtime.state(), easycon_runtime::RuntimeState::Closed);
        assert_eq!(
            runtime.counts(),
            RuntimeCounts {
                active_operations: 0,
                active_resources: 0,
                active_tasks: 0,
            }
        );
    }
}
