use std::sync::Arc;
use std::time::{Duration, Instant};

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerLeaseState, ControllerOptions, ControllerSession,
    PreciseSequence, SequenceStep, SwitchReport, WriteKind,
};
use easycon_model::{Hat, StickPosition};
use easycon_runtime::{
    EventKind, Operation, OperationState, Runtime, RuntimeCounts, SubscriptionOptions,
    SubscriptionRead, VirtualClock, WaitResult, WaitTimeout,
};
use easycon_test_support::{FakeControllerTransport, HandshakeOutcome};

const INPUT_STEP_COUNT: usize = 10_000;
const REPORT_COUNT: usize = INPUT_STEP_COUNT / 2;
const LANE_START_NS: u64 = 1_000_000_000;
const INTERVAL_NS: u64 = 30_000_000;
const EXTRA_GAP_NS: u64 = 1_000_000;

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

fn wait_terminal(operation: &Operation) {
    assert!(matches!(
        operation.wait(WaitTimeout::For(Duration::from_secs(2))),
        WaitResult::Completed(_)
    ));
}

fn group_offset_ns(group: usize) -> u64 {
    let group = u64::try_from(group).expect("group index");
    group
        .checked_mul(INTERVAL_NS)
        .and_then(|offset| offset.checked_add((group / 17).saturating_mul(EXTRA_GAP_NS)))
        .expect("10,000-step offset remains bounded")
}

fn group_positions(group: usize) -> (StickPosition, StickPosition) {
    let [low, high] = u16::try_from(group).expect("group fits u16").to_le_bytes();
    (
        StickPosition::new(low, high),
        StickPosition::new(!low, !high),
    )
}

#[test]
fn ten_thousand_step_sequence_is_exact_and_all_registries_converge() {
    let clock = Arc::new(VirtualClock::default());
    let runtime = Runtime::new(clock.clone());
    let events = runtime
        .subscribe(SubscriptionOptions {
            capacity: REPORT_COUNT + 32,
            ..SubscriptionOptions::default()
        })
        .expect("large deterministic trace subscription");
    let fake = FakeControllerTransport::new(clock.clone());
    fake.push_handshake(115_200, HandshakeOutcome::Success { elapsed_ns: 0 });
    let controller = ControllerSession::new(
        &runtime,
        Box::new(fake.clone()),
        ControllerOptions::default(),
    )
    .expect("Controller over memory transport");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    wait_terminal(&connect);
    assert_eq!(connect.snapshot().state, OperationState::Succeeded);

    clock.advance_to(LANE_START_NS);
    let mut steps = Vec::with_capacity(INPUT_STEP_COUNT);
    let mut expected_targets = Vec::with_capacity(REPORT_COUNT);
    let mut expected_reports = Vec::with_capacity(REPORT_COUNT);
    for group in 0..REPORT_COUNT {
        let offset_ns = group_offset_ns(group);
        let (first, second) = group_positions(group);
        steps.push(SequenceStep::new(
            offset_ns,
            ControllerAction::LeftStick(first),
        ));
        steps.push(SequenceStep::new(
            offset_ns,
            ControllerAction::LeftStick(second),
        ));
        expected_targets.push(LANE_START_NS.saturating_add(offset_ns));
        expected_reports
            .push(SwitchReport::new(0, Hat::Center, second, StickPosition::CENTER).encode());
    }
    assert_eq!(steps.len(), INPUT_STEP_COUNT);

    let operation = controller
        .precise_sequence(PreciseSequence::new(steps).expect("maximum-size sequence"))
        .expect("sequence operation");
    for (index, target_ns) in expected_targets.iter().copied().enumerate() {
        if index != 0 {
            assert_eq!(
                controller.snapshot().accepted_report_count,
                u64::try_from(index).expect("accepted count"),
                "report {index} was emitted before its absolute target"
            );
            clock.advance_to(target_ns);
        }
        assert!(fake.wait_for_accepted_count(index + 1, Duration::from_secs(2)));
        wait_until(|| {
            controller.snapshot().accepted_report_count
                >= u64::try_from(index + 1).expect("accepted count")
        });
        assert_eq!(
            controller.snapshot().accepted_report_count,
            u64::try_from(index + 1).expect("accepted count"),
            "more than one same-offset report was emitted for group {index}"
        );
    }
    wait_terminal(&operation);

    assert_eq!(operation.snapshot().state, OperationState::Succeeded);
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    assert_eq!(
        controller.snapshot().desired_report.encode(),
        *expected_reports.last().expect("final expected report")
    );
    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), REPORT_COUNT);
    for (index, ((write, expected_target), expected_report)) in writes
        .iter()
        .zip(&expected_targets)
        .zip(&expected_reports)
        .enumerate()
    {
        assert_eq!(write.context.sequence, u64::try_from(index + 1).unwrap());
        assert_eq!(write.context.kind, WriteKind::Report);
        assert_eq!(write.context.timestamp_ns, *expected_target);
        assert_eq!(write.bytes, *expected_report);
    }

    let deadlines = clock.deadline_trace();
    assert_eq!(deadlines.len(), REPORT_COUNT);
    assert_eq!(clock.wake_order().len(), REPORT_COUNT);
    for (deadline, expected_target) in deadlines.iter().zip(&expected_targets) {
        assert_eq!(deadline.target_ns, *expected_target);
        assert_eq!(deadline.dispatched_at_ns, Some(*expected_target));
        assert!(deadline.woken);
    }

    let mut observed = Vec::new();
    while let SubscriptionRead::Event(event) = events.read(WaitTimeout::Poll).expect("event read") {
        observed.push(event);
    }
    assert!(
        observed
            .iter()
            .all(|event| !matches!(event.kind, EventKind::Gap(_)))
    );
    let terminal_codes: Vec<_> = observed
        .iter()
        .filter(|event| {
            event.kind == EventKind::Terminal && event.operation_id == Some(operation.id())
        })
        .map(|event| event.code)
        .collect();
    assert_eq!(terminal_codes, ["runtime.operation.succeeded"]);
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 1,
            active_tasks: 2,
        }
    );

    clock.advance_to(
        expected_targets
            .last()
            .copied()
            .expect("final target")
            .saturating_add(INTERVAL_NS),
    );
    controller.close();
    assert_eq!(controller.snapshot().lease, ControllerLeaseState::Available);
    assert_eq!(controller.snapshot().desired_report, SwitchReport::NEUTRAL);
    assert!(fake.is_closed());
    let writes = fake.accepted_writes();
    assert_eq!(writes.len(), REPORT_COUNT + 1);
    assert_eq!(
        writes.last().expect("close report").context.kind,
        WriteKind::Neutralize
    );
    assert_eq!(
        writes.last().expect("close report").bytes,
        SwitchReport::NEUTRAL.encode()
    );
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 1,
        }
    );

    runtime.close().expect("Runtime close");
    assert_eq!(
        runtime.counts(),
        RuntimeCounts {
            active_operations: 0,
            active_resources: 0,
            active_tasks: 0,
        }
    );
}
