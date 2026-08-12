use std::sync::Arc;
use std::time::{Duration, Instant};

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerOptions, ControllerSession, ControllerState,
    SwitchReport, TransportError, TransportErrorKind, WriteKind,
};
use easycon_model::{Button, ErrorCode};
use easycon_runtime::{
    Clock, Operation, OperationState, Runtime, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_serial::{SerialErrorKind, SerialPortDescriptor};
use easycon_test_support::{
    Ch32AckBehavior, Ch32ByteSimulator, Ch32HandshakeBehavior, FakeControllerTransport,
    HandshakeOutcome,
};

fn create_controller(
    _clock: &Arc<VirtualClock>,
    runtime: &Runtime,
    simulator: &Ch32ByteSimulator,
) -> ControllerSession {
    let port = SerialPortDescriptor::new("USB\\VID_1A86&PID_7523\\TEST", "COM99")
        .expect("test descriptor");
    ControllerSession::new(
        runtime,
        Box::new(simulator.transport(port)),
        ControllerOptions::default(),
    )
    .expect("Controller over serial adapter")
}

fn connected() -> (
    Arc<VirtualClock>,
    Runtime,
    Ch32ByteSimulator,
    ControllerSession,
) {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock.clone());
    simulator.push_handshake(115_200, Ch32HandshakeBehavior::Success);
    let controller = create_controller(&clock, &runtime, &simulator);
    let connect = controller
        .connect(ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 100,
        })
        .expect("connect operation");
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Succeeded);
    (clock, runtime, simulator, controller)
}

fn connected_fake() -> (
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
        &runtime,
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("Controller over fake transport");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("fake connect operation");
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

fn wait_running(operation: &Operation) {
    let started = Instant::now();
    while operation.snapshot().state == OperationState::Pending {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "operation did not start"
        );
        std::thread::yield_now();
    }
    assert_eq!(operation.snapshot().state, OperationState::Running);
}

struct ReleaseCh32BarriersOnPanic(Ch32ByteSimulator);

impl Drop for ReleaseCh32BarriersOnPanic {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.release_close();
            self.0.disconnect();
        }
    }
}

fn close_controller(clock: &VirtualClock, runtime: &Runtime, controller: &ControllerSession) {
    if controller.snapshot().state == ControllerState::Connected
        && let Some(last) = controller.snapshot().last_report_timestamp_ns
    {
        let target = last.saturating_add(ControllerOptions::default().minimum_report_interval_ns);
        if clock.now_ns() < target {
            clock.advance_to(target);
        }
    }
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.serial.end-to-end
// conformance: phase2a.serial.partial-io
#[test]
fn serial_adapter_fallback_partial_report_and_close_are_byte_exact() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock.clone());
    simulator.set_maximum_write_chunk(2);
    simulator.push_handshake(115_200, Ch32HandshakeBehavior::Timeout);
    simulator.push_handshake(9_600, Ch32HandshakeBehavior::Success);
    let controller = create_controller(&clock, &runtime, &simulator);

    let connect = controller
        .connect(ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 100,
        })
        .expect("connect operation");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    clock.advance_to(100);
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Succeeded);

    let direct = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("direct operation");
    wait_terminal(&direct);
    assert_eq!(direct.snapshot().state, OperationState::Succeeded);
    let first = simulator.snapshot();
    assert_eq!(first.baud_attempts, [115_200, 9_600]);
    assert_eq!(first.reports.len(), 1);
    assert_eq!(first.reports[0].bytes, {
        let mut report = SwitchReport::NEUTRAL;
        report.press(Button::A);
        report.encode()
    });
    assert_eq!(first.reports[0].context.kind, WriteKind::Report);
    assert!(
        first.byte_write_calls > 4,
        "partial byte writes were exercised"
    );

    close_controller(&clock, &runtime, &controller);
    let final_snapshot = simulator.snapshot();
    assert_eq!(final_snapshot.reports.len(), 2);
    assert_eq!(
        final_snapshot.reports[1].bytes,
        SwitchReport::NEUTRAL.encode()
    );
    assert_eq!(
        final_snapshot.reports[1].context.kind,
        WriteKind::Neutralize
    );
    assert_eq!(final_snapshot.active_streams, 0);
    assert_eq!(final_snapshot.closed_streams, 2);
}

// conformance: phase2a.serial.open-errors
#[test]
fn access_denied_and_port_busy_open_failures_are_bounded_and_leak_free() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock.clone());
    simulator.push_handshake(
        115_200,
        Ch32HandshakeBehavior::OpenError(SerialErrorKind::AccessDenied),
    );
    simulator.push_handshake(
        9_600,
        Ch32HandshakeBehavior::OpenError(SerialErrorKind::PortBusy),
    );
    let controller = create_controller(&clock, &runtime, &simulator);

    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    wait_terminal(&connect);

    assert_eq!(connect.snapshot().state, OperationState::Failed);
    assert_eq!(
        connect.snapshot().error.expect("open failure").code(),
        ErrorCode::Transport
    );
    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.baud_attempts, [115_200, 9_600]);
    assert_eq!(snapshot.active_streams, 0);
    assert_eq!(snapshot.closed_streams, 0);
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.serial.ack-faults
#[test]
fn byte_ack_delay_duplicate_wrong_reply_and_zero_progress_are_isolated() {
    let (clock, runtime, simulator, controller) = connected();

    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.zero_next_read();
    let zero = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x10]), 0xff, 100)
        .expect("zero-read command");
    wait_terminal(&zero);
    assert_eq!(
        zero.snapshot().error.expect("zero progress").code(),
        ErrorCode::Transport
    );

    simulator.push_ack(Ch32AckBehavior::Reply(0x00));
    let wrong = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x11]), 0xff, 100)
        .expect("wrong ACK command");
    wait_terminal(&wrong);
    assert_eq!(
        wrong.snapshot().error.expect("wrong ACK").code(),
        ErrorCode::ProtocolError
    );

    simulator.push_ack(Ch32AckBehavior::Duplicate(0xff));
    let duplicate = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x12]), 0xff, 100)
        .expect("duplicate ACK command");
    wait_terminal(&duplicate);
    assert_eq!(duplicate.snapshot().state, OperationState::Succeeded);

    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    let after_duplicate = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x13]), 0xff, 100)
        .expect("post-duplicate command");
    wait_terminal(&after_duplicate);
    assert_eq!(after_duplicate.snapshot().state, OperationState::Succeeded);

    simulator.push_ack(Ch32AckBehavior::Delayed {
        byte: 0xff,
        elapsed_ns: 50,
    });
    let delayed = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x14]), 0xff, 100)
        .expect("delayed ACK command");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    clock.advance_to(clock.now_ns().saturating_add(50));
    wait_terminal(&delayed);
    assert_eq!(delayed.snapshot().state, OperationState::Succeeded);

    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.commands.len(), 5);
    assert_eq!(snapshot.discarded_input_bytes, 2);
    close_controller(&clock, &runtime, &controller);
    assert_eq!(simulator.snapshot().active_streams, 0);
}

#[test]
fn blocked_byte_write_cancellation_neutralizes_before_terminal() {
    let (clock, runtime, simulator, controller) = connected();
    simulator.block_next_write();
    let operation = controller
        .direct(ControllerAction::ButtonDown(Button::B))
        .expect("blocked direct operation");
    assert!(simulator.wait_until_write_blocked(Duration::from_secs(2)));

    operation.cancel();
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.reports.len(), 1);
    assert_eq!(snapshot.reports[0].bytes, SwitchReport::NEUTRAL.encode());
    assert_eq!(snapshot.reports[0].context.kind, WriteKind::Neutralize);
    close_controller(&clock, &runtime, &controller);
    assert_eq!(simulator.snapshot().active_streams, 0);
}

// conformance: phase2a.serial.deadline-close-wake
#[test]
fn blocked_ack_obeys_deadline_and_controller_close_wakes_a_second_wait() {
    let (clock, runtime, simulator, controller) = connected();
    simulator.push_ack(Ch32AckBehavior::NoReply);
    let timeout = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x20]), 0xff, 100)
        .expect("timeout command");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    clock.advance_to(100);
    wait_terminal(&timeout);
    assert_eq!(
        timeout.snapshot().error.expect("protocol timeout").code(),
        ErrorCode::ProtocolTimeout
    );

    simulator.push_ack(Ch32AckBehavior::NoReply);
    let closing = controller
        .command_with_ack(Arc::<[u8]>::from([0xa5, 0x21]), 0xff, 100)
        .expect("close-wake command");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    simulator.block_next_close();
    let closing_controller = controller.clone();
    let close = std::thread::spawn(move || closing_controller.close());
    assert!(simulator.wait_until_close_blocked(Duration::from_secs(2)));
    assert_eq!(closing.wait(WaitTimeout::Poll), WaitResult::Timeout);
    simulator.release_close();
    close.join().expect("Controller close thread");
    wait_terminal(&closing);

    assert_eq!(closing.snapshot().state, OperationState::Cancelled);
    assert_eq!(simulator.snapshot().active_streams, 0);
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.serial.disconnect-close-order
#[test]
fn hot_unplug_and_mid_payload_failure_close_the_stream_without_continuation() {
    let (_clock, runtime, simulator, controller) = connected();
    simulator.block_next_write();
    simulator.block_next_close();
    let unplugged = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("hot-unplug operation");
    assert!(simulator.wait_until_write_blocked(Duration::from_secs(2)));
    simulator.disconnect();
    assert!(simulator.wait_until_close_blocked(Duration::from_secs(2)));
    let state_before_close = unplugged.wait(WaitTimeout::Poll);
    simulator.release_close();
    assert_eq!(state_before_close, WaitResult::Timeout);
    wait_terminal(&unplugged);

    assert_eq!(
        unplugged.snapshot().error.expect("disconnect").code(),
        ErrorCode::DeviceDisconnected
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    assert_eq!(simulator.snapshot().active_streams, 0);
    controller.close();
    runtime.close().expect("Runtime close");

    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock.clone());
    simulator.push_handshake(115_200, Ch32HandshakeBehavior::Success);
    let controller = create_controller(&clock, &runtime, &simulator);
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    wait_terminal(&connect);
    simulator.set_maximum_write_chunk(2);
    simulator.fail_controller_write_after_bytes(2, SerialErrorKind::Io);
    let partial = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("partial-failure operation");
    wait_terminal(&partial);

    assert_eq!(
        partial.snapshot().error.expect("partial failure").code(),
        ErrorCode::DeviceDisconnected
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    let snapshot = simulator.snapshot();
    assert!(snapshot.reports.is_empty());
    assert_eq!(snapshot.active_streams, 0);
    assert_eq!(snapshot.closed_streams, 1);
    controller.close();
    runtime.close().expect("Runtime close");
}

#[test]
fn cancellation_between_partial_calls_closes_the_corrupted_stream() {
    let (_clock, runtime, simulator, controller) = connected();
    simulator.set_maximum_write_chunk(2);
    simulator.block_controller_write_after_bytes(2);
    let operation = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("partial-cancel operation");
    assert!(simulator.wait_until_write_blocked(Duration::from_secs(2)));

    operation.cancel();
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    let snapshot = simulator.snapshot();
    assert!(snapshot.reports.is_empty());
    assert_eq!(snapshot.active_streams, 0);
    assert_eq!(snapshot.closed_streams, 1);
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.serial.neutral-failure-close-order
#[test]
fn failed_cancel_neutral_closes_the_serial_stream_before_terminal() {
    let (clock, runtime, simulator, controller) = connected();
    let accepted = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("accepted direct operation");
    wait_terminal(&accepted);
    assert_eq!(accepted.snapshot().state, OperationState::Succeeded);

    let _barrier_cleanup = ReleaseCh32BarriersOnPanic(simulator.clone());
    simulator.block_next_write();
    let cancelled = controller
        .direct(ControllerAction::ButtonDown(Button::B))
        .expect("cancelled direct operation");
    wait_running(&cancelled);
    clock.advance_by(Duration::from_nanos(
        ControllerOptions::default().minimum_report_interval_ns,
    ));
    let write_blocked = simulator.wait_until_write_blocked(Duration::from_secs(2));
    let reports_at_write_barrier = simulator.snapshot().reports.len();

    simulator.fail_next_write(SerialErrorKind::Io);
    simulator.block_next_close();
    cancelled.cancel();

    let close_blocked = simulator.wait_until_close_blocked(Duration::from_secs(2));
    let state_before_close = cancelled.wait(WaitTimeout::Poll);
    simulator.release_close();
    if !write_blocked || !close_blocked {
        simulator.disconnect();
    }
    wait_terminal(&cancelled);

    assert!(
        write_blocked,
        "cancelled B report did not enter the byte-write barrier"
    );
    assert_eq!(reports_at_write_barrier, 1);
    assert!(
        close_blocked,
        "failed neutralization did not enter the stream-close barrier"
    );
    assert_eq!(state_before_close, WaitResult::Timeout);
    assert_eq!(cancelled.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.active_streams, 0);
    assert_eq!(snapshot.closed_streams, 1);
    assert_eq!(snapshot.reports.len(), 1);
    controller.close();
    runtime.close().expect("Runtime close");
}

#[test]
fn zero_progress_write_fails_only_after_connected_neutral_cleanup() {
    let (clock, runtime, simulator, controller) = connected();
    simulator.zero_next_write();
    let operation = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("zero-progress operation");
    wait_terminal(&operation);

    assert_eq!(
        operation
            .snapshot()
            .error
            .expect("transport failure")
            .code(),
        ErrorCode::Transport
    );
    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.reports.len(), 1);
    assert_eq!(snapshot.reports[0].bytes, SwitchReport::NEUTRAL.encode());
    assert_eq!(controller.snapshot().state, ControllerState::Connected);
    close_controller(&clock, &runtime, &controller);
}

// conformance: controller.lease.fake-serial-parity
#[test]
fn fake_and_serial_adapter_match_reusable_and_partial_write_settlement() {
    let (fake_clock, fake_runtime, fake, fake_controller) = connected_fake();
    fake.fail_write_call(
        1,
        TransportError::reusable_stream(TransportErrorKind::Io, "fake backend proved zero effect"),
    );
    let fake_zero_effect = fake_controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("fake zero-effect direct operation");
    wait_terminal(&fake_zero_effect);
    assert_eq!(fake_zero_effect.snapshot().state, OperationState::Failed);
    assert_eq!(fake_controller.snapshot().state, ControllerState::Connected);
    assert_eq!(fake.accepted_writes().len(), 1);
    assert_eq!(
        fake.accepted_writes()[0].context.kind,
        WriteKind::Neutralize
    );

    let (serial_clock, serial_runtime, simulator, serial_controller) = connected();
    simulator.zero_next_write();
    let serial_zero_effect = serial_controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("serial zero-effect direct operation");
    wait_terminal(&serial_zero_effect);
    assert_eq!(serial_zero_effect.snapshot().state, OperationState::Failed);
    assert_eq!(
        serial_zero_effect
            .snapshot()
            .error
            .expect("serial zero-effect error")
            .code(),
        fake_zero_effect
            .snapshot()
            .error
            .expect("fake zero-effect error")
            .code()
    );
    assert_eq!(
        serial_controller.snapshot().state,
        ControllerState::Connected
    );
    assert_eq!(simulator.snapshot().reports.len(), 1);
    assert_eq!(
        simulator.snapshot().reports[0].context.kind,
        WriteKind::Neutralize
    );

    close_controller(&fake_clock, &fake_runtime, &fake_controller);
    close_controller(&serial_clock, &serial_runtime, &serial_controller);

    let (_fake_clock, fake_runtime, fake, fake_controller) = connected_fake();
    fake.set_maximum_write_chunk(2);
    fake.fail_write_call(
        2,
        TransportError::new(TransportErrorKind::Io, "fake partial write failure"),
    );
    let fake_partial = fake_controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("fake partial direct operation");
    wait_terminal(&fake_partial);
    assert_eq!(fake_partial.snapshot().state, OperationState::Failed);
    assert_eq!(
        fake_controller.snapshot().state,
        ControllerState::Disconnected
    );
    assert!(fake.is_closed(), "fake partial prefix closes the stream");
    assert!(fake.accepted_writes().is_empty());

    let (_serial_clock, serial_runtime, simulator, serial_controller) = connected();
    simulator.set_maximum_write_chunk(2);
    simulator.fail_controller_write_after_bytes(2, SerialErrorKind::Io);
    let serial_partial = serial_controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("serial partial direct operation");
    wait_terminal(&serial_partial);
    assert_eq!(
        serial_partial.snapshot().state,
        fake_partial.snapshot().state
    );
    assert_eq!(
        serial_controller.snapshot().state,
        ControllerState::Disconnected
    );
    let snapshot = simulator.snapshot();
    assert!(snapshot.reports.is_empty());
    assert_eq!(snapshot.active_streams, 0);
    assert_eq!(snapshot.closed_streams, 1);

    fake_controller.close();
    serial_controller.close();
    fake_runtime.close().expect("fake Runtime close");
    serial_runtime.close().expect("serial Runtime close");
}
