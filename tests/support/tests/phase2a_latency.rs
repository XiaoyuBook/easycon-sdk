use std::sync::Arc;
use std::time::Duration;

use easycon_controller::{
    ConnectOptions, ControllerAction, ControllerOptions, ControllerSession, SwitchReport,
};
use easycon_model::StickPosition;
use easycon_runtime::{Operation, OperationState, Runtime, SystemClock, WaitResult, WaitTimeout};
use easycon_test_support::DirectLatencyTransport;

fn wait_terminal(operation: &Operation) {
    assert!(matches!(
        operation.wait(WaitTimeout::For(Duration::from_secs(2))),
        WaitResult::Completed(_)
    ));
}

#[test]
fn direct_report_records_monotonic_software_path_stages_without_filtering() {
    let clock = Arc::new(SystemClock::default());
    let runtime = Runtime::new(clock.clone());
    let (transport, recorder) = DirectLatencyTransport::new(clock);
    let controller = ControllerSession::new(
        &runtime,
        Box::new(transport),
        ControllerOptions {
            minimum_report_interval_ns: 1,
            ..ControllerOptions::default()
        },
    )
    .expect("Controller over latency transport");
    let connect = controller
        .connect(ConnectOptions::default())
        .expect("connect operation");
    wait_terminal(&connect);

    let first = controller
        .direct(ControllerAction::LeftStick(StickPosition::new(1, 2)))
        .expect("first direct report");
    wait_terminal(&first);
    let second = controller
        .direct(ControllerAction::LeftStick(StickPosition::new(3, 4)))
        .expect("second direct report");
    wait_terminal(&second);
    assert_eq!(first.snapshot().state, OperationState::Succeeded);
    assert_eq!(second.snapshot().state, OperationState::Succeeded);
    assert!(recorder.wait_for_samples(2, Duration::from_secs(2)));

    let samples = recorder.samples();
    assert_eq!(samples.len(), 2);
    assert_eq!(samples[0].operation_id, first.id());
    assert_eq!(samples[1].operation_id, second.id());
    assert_eq!(samples[0].write_sequence + 1, samples[1].write_sequence);
    assert!(samples.iter().all(|sample| sample.is_monotonic()));
    assert!(samples[1].command_admitted_ns >= samples[0].transport_accepted_ns.saturating_add(1));

    controller.close();
    assert_eq!(controller.snapshot().desired_report, SwitchReport::NEUTRAL);
    assert!(recorder.is_closed());
    runtime.close().expect("Runtime close");
}
