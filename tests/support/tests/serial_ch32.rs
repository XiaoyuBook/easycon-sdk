use std::sync::Arc;
use std::time::Duration;

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerOptions, ControllerSession, ControllerState,
    SwitchReport, WriteKind,
};
use easycon_model::{Button, ErrorCode};
use easycon_runtime::{
    Clock, Operation, OperationState, Runtime, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_serial::{SerialErrorKind, SerialPortDescriptor};
use easycon_test_support::{Ch32AckBehavior, Ch32ByteSimulator, Ch32HandshakeBehavior};

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

fn wait_terminal(operation: &Operation) {
    assert!(matches!(
        operation.wait(WaitTimeout::For(Duration::from_secs(2))),
        WaitResult::Completed(_)
    ));
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
    controller.close();
    wait_terminal(&closing);

    assert_eq!(closing.snapshot().state, OperationState::Cancelled);
    assert_eq!(simulator.snapshot().active_streams, 0);
    runtime.close().expect("Runtime close");
}

#[test]
fn hot_unplug_and_mid_payload_failure_close_the_stream_without_continuation() {
    let (_clock, runtime, simulator, controller) = connected();
    simulator.block_next_write();
    let unplugged = controller
        .direct(ControllerAction::ButtonDown(Button::A))
        .expect("hot-unplug operation");
    assert!(simulator.wait_until_write_blocked(Duration::from_secs(2)));
    simulator.disconnect();
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
