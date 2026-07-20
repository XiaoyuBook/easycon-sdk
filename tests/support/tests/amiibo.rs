use std::sync::Arc;
use std::time::Duration;

use easycon_controller::{
    AmiiboLimits, AmiiboSaveOptions, AmiiboSelectOptions, ConnectOptions, ControllerLeaseState,
    ControllerOptions, ControllerSession, ControllerState, SwitchReport, WriteKind,
};
use easycon_model::ErrorCode;
use easycon_runtime::{
    Clock, Operation, OperationState, Runtime, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_serial::SerialPortDescriptor;
use easycon_test_support::{
    AckOutcome, Ch32AckBehavior, Ch32ByteSimulator, Ch32HandshakeBehavior, FakeControllerTransport,
    HandshakeOutcome,
};

fn controller_options(slot_count: u16, maximum_data_len: usize) -> ControllerOptions {
    ControllerOptions {
        amiibo_limits: Some(
            AmiiboLimits::new(slot_count, maximum_data_len).expect("explicit test limits"),
        ),
        ..ControllerOptions::default()
    }
}

fn connected_ch32(
    slot_count: u16,
    maximum_data_len: usize,
) -> (
    Arc<VirtualClock>,
    Runtime,
    Ch32ByteSimulator,
    ControllerSession,
) {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock.clone());
    simulator.push_handshake(115_200, Ch32HandshakeBehavior::Success);
    let port = SerialPortDescriptor::new("USB\\VID_1A86&PID_7523\\AMIIBO", "COM99")
        .expect("test descriptor");
    let controller = ControllerSession::new(
        &runtime,
        Box::new(simulator.transport(port)),
        controller_options(slot_count, maximum_data_len),
    )
    .expect("Controller over CH32 simulator");
    let connect = controller
        .connect(ConnectOptions::default())
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

fn close_connected(runtime: &Runtime, controller: &ControllerSession) {
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.amiibo.explicit-limits
#[test]
fn amiibo_admission_requires_explicit_slot_and_length_limits() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock);
    let port =
        SerialPortDescriptor::new("ROOT\\CH32\\NO_LIMITS", "COM99").expect("test descriptor");
    let controller = ControllerSession::new(
        &runtime,
        Box::new(simulator.transport(port)),
        ControllerOptions::default(),
    )
    .expect("Controller without limits");

    assert!(controller.amiibo_limits().is_none());
    assert_eq!(
        controller
            .save_amiibo(0, Arc::<[u8]>::from([1]), AmiiboSaveOptions::default())
            .err()
            .expect("missing save capability")
            .code(),
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        controller
            .select_amiibo(0, AmiiboSelectOptions::default())
            .err()
            .expect("missing select capability")
            .code(),
        ErrorCode::InvalidArgument
    );
    assert_eq!(runtime.counts().active_operations, 0);
    controller.close();
    runtime.close().expect("Runtime close");

    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let simulator = Ch32ByteSimulator::new(clock);
    let port = SerialPortDescriptor::new("ROOT\\CH32\\LIMITS", "COM99").expect("test descriptor");
    let controller = ControllerSession::new(
        &runtime,
        Box::new(simulator.transport(port)),
        controller_options(2, 40),
    )
    .expect("Controller with limits");
    assert_eq!(controller.amiibo_limits().expect("limits").slot_count(), 2);
    assert!(
        controller
            .save_amiibo(2, Arc::<[u8]>::from([1]), AmiiboSaveOptions::default())
            .is_err()
    );
    assert!(
        controller
            .save_amiibo(0, Arc::<[u8]>::from([]), AmiiboSaveOptions::default())
            .is_err()
    );
    assert!(
        controller
            .save_amiibo(
                0,
                Arc::<[u8]>::from(vec![0; 41]),
                AmiiboSaveOptions::default(),
            )
            .is_err()
    );
    assert!(
        controller
            .select_amiibo(2, AmiiboSelectOptions::default())
            .is_err()
    );
    assert!(
        controller
            .save_amiibo(
                0,
                Arc::<[u8]>::from([1]),
                AmiiboSaveOptions {
                    maximum_chunk_retries: 9,
                    ..AmiiboSaveOptions::default()
                },
            )
            .is_err()
    );
    assert_eq!(runtime.counts().active_operations, 0);
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.amiibo.source-exact-chunks
#[test]
fn save_chunks_and_select_are_source_exact_and_wait_for_the_final_ack() {
    let (clock, runtime, simulator, controller) = connected_ch32(4, 45);
    simulator.set_maximum_write_chunk(3);
    for _ in 0..5 {
        simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    }
    simulator.push_ack(Ch32AckBehavior::Delayed {
        byte: 0xff,
        elapsed_ns: 10,
    });
    let data: Vec<u8> = (0..45).collect();
    let save = controller
        .save_amiibo(
            2,
            Arc::<[u8]>::from(data.clone()),
            AmiiboSaveOptions::default(),
        )
        .expect("save operation");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    assert_eq!(save.snapshot().state, OperationState::Running);
    clock.advance_to(clock.now_ns().saturating_add(10));
    wait_terminal(&save);
    assert_eq!(save.snapshot().state, OperationState::Succeeded);

    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    let select = controller
        .select_amiibo(2, AmiiboSelectOptions::default())
        .expect("select operation");
    wait_terminal(&select);
    assert_eq!(select.snapshot().state, OperationState::Succeeded);

    let snapshot = simulator.snapshot();
    assert_eq!(
        snapshot.commands,
        vec![
            vec![0xa5, 0, 0, 20, 0, 2, 0x90],
            data[0..20].to_vec(),
            vec![0xa5, 20, 0, 20, 0, 2, 0x90],
            data[20..40].to_vec(),
            vec![0xa5, 40, 0, 5, 0, 2, 0x90],
            data[40..45].to_vec(),
            vec![0xa5, 2, 0x91],
        ]
    );
    assert_eq!(snapshot.amiibo_chunks.len(), 3);
    assert_eq!(snapshot.amiibo_chunks[0].offset, 0);
    assert_eq!(snapshot.amiibo_chunks[1].offset, 20);
    assert_eq!(snapshot.amiibo_chunks[2].offset, 40);
    assert_eq!(snapshot.amiibo_chunks[2].bytes, data[40..45]);
    assert_eq!(snapshot.amiibo_selections, [2]);

    close_connected(&runtime, &controller);
    assert_eq!(simulator.snapshot().active_streams, 0);
}

// conformance: phase2a.amiibo.bounded-retry
#[test]
fn save_retries_the_same_chunk_after_source_exact_reset() {
    let (_clock, runtime, simulator, controller) = connected_ch32(1, 20);
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::Reply(0x00));
    simulator.push_ack(Ch32AckBehavior::Reply(0x80));
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    let data = vec![1, 2, 3, 4, 5];

    let operation = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from(data.clone()),
            AmiiboSaveOptions {
                maximum_chunk_retries: 1,
                ..AmiiboSaveOptions::default()
            },
        )
        .expect("retrying save");
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    let snapshot = simulator.snapshot();
    assert_eq!(
        snapshot.commands,
        vec![
            vec![0xa5, 0, 0, 5, 0, 0, 0x90],
            data.clone(),
            vec![0xa5, 0x81, 0xa5, 0x81, 0xa5, 0x81],
            vec![0xa5, 0, 0, 5, 0, 0, 0x90],
            data,
        ]
    );
    assert_eq!(snapshot.amiibo_chunks.len(), 2);
    close_connected(&runtime, &controller);
}

// conformance: phase2a.amiibo.generation
#[test]
fn amiibo_ack_uses_the_existing_generation_matcher() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let fake = FakeControllerTransport::new(clock);
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let controller =
        ControllerSession::new(&runtime, Box::new(fake.clone()), controller_options(1, 20))
            .expect("Controller over transport fake");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    wait_terminal(&connect);
    fake.push_ack(AckOutcome::Frame {
        generation: 1,
        byte: 0xff,
        elapsed_ns: 0,
    });
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

    let save = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from([7, 8, 9]),
            AmiiboSaveOptions {
                maximum_chunk_retries: 0,
                ..AmiiboSaveOptions::default()
            },
        )
        .expect("generation-aware save");
    wait_terminal(&save);

    assert_eq!(save.snapshot().state, OperationState::Succeeded);
    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), 2);
    assert_eq!(writes[0].bytes, [0xa5, 0, 0, 3, 0, 0, 0x90]);
    assert_eq!(writes[1].bytes, [7, 8, 9]);
    close_connected(&runtime, &controller);
}

// conformance: phase2a.amiibo.partial-failure
#[test]
fn save_partial_failure_reports_completed_chunks_and_releases_the_lane() {
    let (clock, runtime, simulator, controller) = connected_ch32(1, 25);
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::Reply(0x00));
    simulator.push_ack(Ch32AckBehavior::Reply(0x80));
    let data: Vec<u8> = (0..25).collect();

    let save = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from(data),
            AmiiboSaveOptions {
                maximum_chunk_retries: 0,
                ..AmiiboSaveOptions::default()
            },
        )
        .expect("partially failing save");
    wait_terminal(&save);

    let error = save.snapshot().error.expect("partial failure");
    assert_eq!(error.code(), ErrorCode::ProtocolError);
    assert!(error.message().contains("after 1 complete chunks"));
    assert_eq!(controller.snapshot().state, ControllerState::Connected);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.commands.len(), 4);
    assert_eq!(snapshot.commands[3], [0xa5, 0x81, 0xa5, 0x81, 0xa5, 0x81]);
    assert_eq!(snapshot.amiibo_chunks.len(), 1);
    let lease = controller
        .acquire_automation_lease()
        .expect("Automation lease");
    let busy = controller
        .select_amiibo(0, AmiiboSelectOptions::default())
        .expect("busy select operation");
    wait_terminal(&busy);
    assert_eq!(
        busy.snapshot().error.expect("lease busy").code(),
        ErrorCode::ResourceBusy
    );
    lease.release();

    simulator.push_ack(Ch32AckBehavior::Reply(0x00));
    simulator.push_ack(Ch32AckBehavior::NoReply);
    simulator.block_next_close();
    let cleanup_failure = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from([9]),
            AmiiboSaveOptions {
                reset_timeout_ns: 100,
                maximum_chunk_retries: 0,
                ..AmiiboSaveOptions::default()
            },
        )
        .expect("save with failing final cleanup");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    clock.advance_to(clock.now_ns().saturating_add(100));
    assert!(simulator.wait_until_close_blocked(Duration::from_secs(2)));
    assert_eq!(cleanup_failure.wait(WaitTimeout::Poll), WaitResult::Timeout);
    simulator.release_close();
    wait_terminal(&cleanup_failure);

    assert_eq!(
        cleanup_failure
            .snapshot()
            .error
            .expect("original partial failure")
            .code(),
        ErrorCode::ProtocolError
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    assert_eq!(simulator.snapshot().active_streams, 0);
    controller.close();
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.amiibo.cancel-deadline-cleanup
#[test]
fn save_cancel_and_operation_deadline_run_bounded_reset_cleanup() {
    let (clock, runtime, simulator, controller) = connected_ch32(1, 20);
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::NoReply);
    simulator.push_ack(Ch32AckBehavior::Reply(0x80));
    let cancelled = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from([1, 2, 3]),
            AmiiboSaveOptions::default(),
        )
        .expect("cancelled save");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    cancelled.cancel();
    wait_terminal(&cancelled);

    assert_eq!(cancelled.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Connected);
    assert_eq!(
        simulator.snapshot().commands[2],
        [0xa5, 0x81, 0xa5, 0x81, 0xa5, 0x81]
    );

    simulator.push_ack(Ch32AckBehavior::NoReply);
    simulator.push_ack(Ch32AckBehavior::Reply(0x80));
    let deadline_ns = clock.now_ns().saturating_add(100);
    let deadline = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from([4, 5, 6]),
            AmiiboSaveOptions {
                operation_deadline_ns: Some(deadline_ns),
                ack_timeout_ns: 1_000,
                ..AmiiboSaveOptions::default()
            },
        )
        .expect("deadline save");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    clock.advance_to(deadline_ns);
    wait_terminal(&deadline);

    assert_eq!(deadline.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Connected);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    close_connected(&runtime, &controller);
}

// conformance: phase2a.amiibo.disconnect-close
#[test]
fn save_disconnect_and_controller_close_have_unique_terminal_cleanup() {
    let (_clock, runtime, simulator, controller) = connected_ch32(1, 20);
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::Disconnect);
    let disconnected = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from([1, 2, 3]),
            AmiiboSaveOptions::default(),
        )
        .expect("disconnecting save");
    wait_terminal(&disconnected);

    assert_eq!(
        disconnected.snapshot().error.expect("disconnect").code(),
        ErrorCode::DeviceDisconnected
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    assert_eq!(simulator.snapshot().active_streams, 0);
    controller.close();
    runtime.close().expect("Runtime close");

    let (_clock, runtime, simulator, controller) = connected_ch32(1, 20);
    simulator.push_ack(Ch32AckBehavior::Reply(0xff));
    simulator.push_ack(Ch32AckBehavior::NoReply);
    let closing = controller
        .save_amiibo(
            0,
            Arc::<[u8]>::from([4, 5, 6]),
            AmiiboSaveOptions::default(),
        )
        .expect("close-cancelled save");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    controller.close();
    wait_terminal(&closing);

    assert_eq!(closing.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Closed);
    let snapshot = simulator.snapshot();
    assert_eq!(snapshot.active_streams, 0);
    assert_eq!(snapshot.reports.len(), 1);
    assert_eq!(snapshot.reports[0].bytes, SwitchReport::NEUTRAL.encode());
    assert_eq!(snapshot.reports[0].context.kind, WriteKind::Neutralize);
    runtime.close().expect("Runtime close");
}

// conformance: phase2a.amiibo.select-cleanup
#[test]
fn select_cancel_deadline_and_disconnect_use_the_same_cleanup_contract() {
    let (clock, runtime, simulator, controller) = connected_ch32(2, 20);
    simulator.push_ack(Ch32AckBehavior::NoReply);
    simulator.push_ack(Ch32AckBehavior::Reply(0x80));
    let cancelled = controller
        .select_amiibo(1, AmiiboSelectOptions::default())
        .expect("cancelled select");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    cancelled.cancel();
    wait_terminal(&cancelled);
    assert_eq!(cancelled.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Connected);

    simulator.push_ack(Ch32AckBehavior::NoReply);
    simulator.push_ack(Ch32AckBehavior::Reply(0x80));
    let deadline_ns = clock.now_ns().saturating_add(100);
    let deadline = controller
        .select_amiibo(
            1,
            AmiiboSelectOptions {
                operation_deadline_ns: Some(deadline_ns),
                ack_timeout_ns: 1_000,
            },
        )
        .expect("deadline select");
    assert!(simulator.wait_until_read_blocked(Duration::from_secs(2)));
    clock.advance_to(deadline_ns);
    wait_terminal(&deadline);
    assert_eq!(deadline.snapshot().state, OperationState::Cancelled);
    assert_eq!(controller.snapshot().state, ControllerState::Connected);

    simulator.push_ack(Ch32AckBehavior::Disconnect);
    let disconnected = controller
        .select_amiibo(1, AmiiboSelectOptions::default())
        .expect("disconnecting select");
    wait_terminal(&disconnected);
    assert_eq!(
        disconnected.snapshot().error.expect("disconnect").code(),
        ErrorCode::DeviceDisconnected
    );
    assert_eq!(controller.snapshot().state, ControllerState::Disconnected);
    assert_eq!(simulator.snapshot().active_streams, 0);
    controller.close();
    runtime.close().expect("Runtime close");
}
