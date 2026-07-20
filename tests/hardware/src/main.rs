#![forbid(unsafe_code)]

use std::env;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use easycon_controller::{
    AckFrame, AckRequest, AmiiboLimits, AmiiboSaveOptions, AmiiboSelectOptions, ConnectOptions,
    ControllerAction, ControllerOptions, ControllerSession, ControllerTransport, HandshakeRequest,
    PreciseSequence, SequenceStep, TransportError, WriteKind, WriteRequest,
};
use easycon_hardware_qualification::{distribution, option_value, required_value, value_or};
use easycon_model::{Button, Hat, StickPosition};
use easycon_runtime::{
    Clock, Operation, OperationState, Runtime, RuntimeCounts, SystemClock, WaitResult, WaitTimeout,
};
use easycon_serial::{
    SerialControllerTransport, SerialPortDescriptor, WindowsByteIoFactory, discover_system_ports,
};
use serde_json::{Value, json};

const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const MINIMUM_REPORT_INTERVAL_NS: u64 = 30_000_000;

#[derive(Clone, Debug)]
struct HandshakeAttempt {
    baud: u32,
    succeeded: bool,
    error: Option<String>,
}

#[derive(Clone, Copy, Debug)]
struct TimingSample {
    sequence: u64,
    command_admitted_ns: Option<u64>,
    lane_wake_ns: Option<u64>,
    dispatch_ns: u64,
    write_entered_ns: u64,
    transport_accepted_ns: u64,
}

#[derive(Default)]
struct Telemetry {
    actual_baud: Option<u32>,
    handshake_attempts: Vec<HandshakeAttempt>,
    timings: Vec<TimingSample>,
}

struct ObservedTransport {
    inner: SerialControllerTransport,
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
        let result = self.inner.write(request);
        let accepted_at = self.clock.now_ns();
        if let Ok(written) = result
            && context.kind == WriteKind::Report
            && context
                .total_len
                .saturating_sub(remaining)
                .saturating_add(written)
                == context.total_len
        {
            let (command_admitted_ns, lane_wake_ns) =
                context.direct_timing.map_or((None, None), |timing| {
                    (Some(timing.command_admitted_ns), Some(timing.lane_wake_ns))
                });
            self.telemetry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .timings
                .push(TimingSample {
                    sequence: context.sequence,
                    command_admitted_ns,
                    lane_wake_ns,
                    dispatch_ns: context.timestamp_ns,
                    write_entered_ns: entered,
                    transport_accepted_ns: accepted_at,
                });
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
    descriptor: SerialPortDescriptor,
}

impl Harness {
    fn new(port_name: &str, options: ControllerOptions) -> Result<Self, String> {
        let descriptor = find_port(port_name)?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
        let telemetry = Arc::new(Mutex::new(Telemetry::default()));
        let serial = SerialControllerTransport::new(
            clock.clone(),
            descriptor.clone(),
            Box::new(WindowsByteIoFactory),
        );
        let observed = ObservedTransport {
            inner: serial,
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
            descriptor,
        })
    }

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

    fn close(&self) -> Result<RuntimeCounts, String> {
        self.controller.close();
        self.runtime
            .close()
            .map_err(|error| format!("Runtime close rejected: {error:?}"))?;
        Ok(self.runtime.counts())
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
}

fn main() {
    if let Err(error) = real_main() {
        eprintln!("hardware qualification failed: {error}");
        std::process::exit(1);
    }
}

fn real_main() -> Result<(), String> {
    if !cfg!(windows) {
        return Err("the hardware qualification CLI requires Windows".to_owned());
    }
    let arguments: Vec<String> = env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };
    if matches!(command, "--help" | "-h" | "help") {
        print_help();
        return Ok(());
    }
    let artifact_dir = artifact_dir(&arguments)?;
    fs::create_dir_all(&artifact_dir).map_err(|error| error.to_string())?;
    let execution = match command {
        "discover" => run_discover(&arguments),
        "handshake" => run_handshake(&arguments),
        "smoke" => run_smoke(&arguments),
        "home-wake" => run_home_wake(&arguments),
        "faults" => run_faults(&arguments),
        "hotplug" => run_hotplug(&arguments),
        "lifecycle" => run_lifecycle(&arguments),
        "sequence" => run_sequence(&arguments, &artifact_dir),
        "amiibo" => run_amiibo(&arguments),
        _ => Err(format!("unknown command: {command}")),
    };
    let (document, failure) = finalize_result(command, execution);
    let output = artifact_dir.join(format!("{command}.json"));
    fs::write(
        &output,
        serde_json::to_vec_pretty(&document).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "artifact": output,
            "document": document,
        }))
        .map_err(|error| error.to_string())?
    );
    failure.map_or(Ok(()), Err)
}

fn finalize_result(command: &str, execution: Result<Value, String>) -> (Value, Option<String>) {
    match execution {
        Ok(result) => (
            json!({"command": command, "status": "passed", "result": result}),
            None,
        ),
        Err(error) => (
            json!({"command": command, "status": "failed", "error": error}),
            Some(error),
        ),
    }
}

fn run_discover(arguments: &[String]) -> Result<Value, String> {
    let samples = value_or(arguments, "--samples", 3_usize)?;
    if samples == 0 {
        return Err("--samples must be non-zero".to_owned());
    }
    let mut snapshots = Vec::with_capacity(samples);
    for index in 0..samples {
        let ports = discover_system_ports().map_err(|error| error.to_string())?;
        snapshots.push(Value::Array(ports.iter().map(port_json).collect()));
        if index + 1 != samples {
            thread::sleep(Duration::from_millis(250));
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

fn run_handshake(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    let operation = harness.connect(ConnectOptions::default())?;
    let result = json!({
        "command": "handshake",
        "port": port_json(&harness.descriptor),
        "operation": operation,
        "actual_baud": harness.actual_baud(),
        "attempts": handshake_json(&harness.telemetry),
    });
    let counts = harness.close()?;
    Ok(with_close_counts(result, counts))
}

fn run_smoke(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let full = arguments.iter().any(|argument| argument == "--full");
    let wake_left_stick = arguments
        .iter()
        .any(|argument| argument == "--wake-left-stick");
    let hold_ms = value_or(arguments, "--hold-ms", 0_u64)?;
    validate_hold_ms(hold_ms)?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let mut actions = Vec::new();
    if wake_left_stick {
        for (label, action) in left_stick_wake_actions() {
            exercise_action(&harness, label, action, &mut actions)?;
        }
    }
    exercise_action(
        &harness,
        "button.A.down",
        ControllerAction::ButtonDown(Button::A),
        &mut actions,
    )?;
    if hold_ms != 0 {
        thread::sleep(Duration::from_millis(hold_ms));
    }
    exercise_action(
        &harness,
        "button.A.up",
        ControllerAction::ButtonUp(Button::A),
        &mut actions,
    )?;
    exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;

    if full {
        for button in Button::ALL {
            if button != Button::A {
                exercise_action(
                    &harness,
                    &format!("button.{button:?}.down"),
                    ControllerAction::ButtonDown(button),
                    &mut actions,
                )?;
                exercise_action(
                    &harness,
                    &format!("button.{button:?}.up"),
                    ControllerAction::ButtonUp(button),
                    &mut actions,
                )?;
                exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
            }
        }
        for hat in Hat::ALL.into_iter().filter(|hat| *hat != Hat::Center) {
            exercise_action(
                &harness,
                &format!("hat.{hat:?}"),
                ControllerAction::Hat(hat),
                &mut actions,
            )?;
            exercise_action(
                &harness,
                "hat.Center",
                ControllerAction::Hat(Hat::Center),
                &mut actions,
            )?;
            exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
        }
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
                exercise_action(
                    &harness,
                    &format!("stick.{side}.{},{}", position.x, position.y),
                    constructor(position),
                    &mut actions,
                )?;
                exercise_action(
                    &harness,
                    &format!("stick.{side}.center"),
                    constructor(StickPosition::CENTER),
                    &mut actions,
                )?;
                exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
            }
        }
    }
    let result = json!({
        "command": "smoke",
        "stage": if full { "full" } else { "a_only" },
        "wake_left_stick": wake_left_stick,
        "a_hold_ms": hold_ms,
        "port": port_json(&harness.descriptor),
        "actual_baud": harness.actual_baud(),
        "actions": actions,
        "final_snapshot": snapshot_json(&harness.controller),
        "latency": latency_json(&harness.telemetry, harness.actual_baud()),
        "switch_observation": "requires operator confirmation",
    });
    let counts = harness.close()?;
    Ok(with_close_counts(result, counts))
}

fn run_home_wake(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let attempts = value_or(arguments, "--attempts", 20_usize)?;
    let interval_seconds = value_or(arguments, "--interval-seconds", 3_u64)?;
    validate_home_wake(attempts, interval_seconds)?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let started = Instant::now();
    let mut records = Vec::with_capacity(attempts);
    for attempt in 0..attempts {
        let target = Duration::from_secs(
            u64::try_from(attempt)
                .expect("bounded attempt index fits u64")
                .saturating_mul(interval_seconds),
        );
        if let Some(remaining) = target.checked_sub(started.elapsed()) {
            thread::sleep(remaining);
        }
        let attempt_started_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let mut actions = Vec::with_capacity(3);
        exercise_action(
            &harness,
            "button.Home.down",
            ControllerAction::ButtonDown(Button::Home),
            &mut actions,
        )?;
        println!("{}", home_attempt_marker(attempt + 1));
        std::io::stdout()
            .flush()
            .map_err(|error| error.to_string())?;
        exercise_action(
            &harness,
            "button.Home.up",
            ControllerAction::ButtonUp(Button::Home),
            &mut actions,
        )?;
        exercise_action(&harness, "neutral", ControllerAction::Reset, &mut actions)?;
        records.push(json!({
            "attempt": attempt + 1,
            "started_after_ms": attempt_started_ms,
            "actions": actions,
        }));
    }
    let result = json!({
        "command": "home-wake",
        "attempts": attempts,
        "interval_seconds": interval_seconds,
        "actual_baud": harness.actual_baud(),
        "port": port_json(&harness.descriptor),
        "records": records,
        "final_snapshot": snapshot_json(&harness.controller),
        "latency": latency_json(&harness.telemetry, harness.actual_baud()),
        "switch_observation": "requires operator confirmation",
    });
    let counts = harness.close()?;
    Ok(with_close_counts(result, counts))
}

fn run_faults(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let primary = Harness::new(&port, ControllerOptions::default())?;
    primary.connect(ConnectOptions::default())?;

    let occupied = Harness::new(&port, ControllerOptions::default())?;
    let occupied_operation = occupied
        .controller
        .connect(ConnectOptions {
            operation_deadline_ns: None,
            protocol_timeout_ns: 300_000_000,
        })
        .map_err(|error| error.to_string())?;
    wait_terminal(&occupied_operation, OPERATION_TIMEOUT)?;
    let occupied_result = operation_json(&occupied_operation);
    let occupied_counts = occupied.close()?;

    let sequence = PreciseSequence::new(vec![
        SequenceStep::new(0, ControllerAction::ButtonDown(Button::A)),
        SequenceStep::new(5_000_000_000, ControllerAction::ButtonUp(Button::A)),
    ])
    .map_err(|error| error.to_string())?;
    let cancelled = primary
        .controller
        .precise_sequence(sequence)
        .map_err(|error| error.to_string())?;
    let _ = cancelled.cancel();
    wait_terminal(&cancelled, OPERATION_TIMEOUT)?;

    let deadline = Harness::new(&port, ControllerOptions::default())?;
    let deadline_operation = deadline
        .controller
        .connect(ConnectOptions {
            operation_deadline_ns: Some(deadline.now_ns()),
            protocol_timeout_ns: 300_000_000,
        })
        .map_err(|error| error.to_string())?;
    wait_terminal(&deadline_operation, OPERATION_TIMEOUT)?;
    let deadline_result = operation_json(&deadline_operation);
    let deadline_counts = deadline.close()?;

    let result = json!({
        "command": "faults",
        "port_occupied": occupied_result,
        "port_occupied_expected_failed": occupied_operation.snapshot().state == OperationState::Failed,
        "cancel": operation_json(&cancelled),
        "cancel_expected_cancelled": cancelled.snapshot().state == OperationState::Cancelled,
        "post_cancel_snapshot": snapshot_json(&primary.controller),
        "deadline": deadline_result,
        "deadline_expected_cancelled": deadline_operation.snapshot().state == OperationState::Cancelled,
        "secondary_close_counts": counts_json(occupied_counts),
        "deadline_close_counts": counts_json(deadline_counts),
    });
    let counts = primary.close()?;
    Ok(with_close_counts(result, counts))
}

fn run_hotplug(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let timeout_seconds = value_or(arguments, "--timeout-seconds", 180_u64)?;
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
    let stable_id = harness.descriptor.stable_id().to_owned();
    println!("HOTPLUG_READY: unplug {port} now");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    wait_for_presence(&stable_id, false, Duration::from_secs(timeout_seconds))?;
    let disconnected = harness
        .controller
        .reset()
        .map_err(|error| error.to_string())?;
    wait_terminal(&disconnected, OPERATION_TIMEOUT)?;
    harness.close()?;

    println!("HOTPLUG_DISCONNECTED: reconnect the same device now");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    wait_for_presence(&stable_id, true, Duration::from_secs(timeout_seconds))?;
    let reconnected = Harness::new(&port, ControllerOptions::default())?;
    let reconnect_operation = reconnected.connect(ConnectOptions::default())?;
    let result = json!({
        "command": "hotplug",
        "stable_id": stable_id,
        "disconnect_operation": operation_json(&disconnected),
        "disconnect_detected": disconnected.snapshot().state == OperationState::Failed,
        "reconnect_operation": reconnect_operation,
        "reconnect_baud": reconnected.actual_baud(),
        "reconnected_identity": port_json(&reconnected.descriptor),
    });
    let counts = reconnected.close()?;
    Ok(with_close_counts(result, counts))
}

fn run_lifecycle(arguments: &[String]) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let cycles = value_or(arguments, "--cycles", 100_usize)?;
    if cycles == 0 {
        return Err("--cycles must be non-zero".to_owned());
    }
    let before = process_metrics()?;
    let mut records = Vec::with_capacity(cycles);
    for cycle in 1..=cycles {
        let harness = Harness::new(&port, ControllerOptions::default())?;
        harness.connect(ConnectOptions::default())?;
        let baud = harness.actual_baud();
        let counts = harness.close()?;
        let metrics = process_metrics()?;
        records.push(json!({
            "cycle": cycle,
            "baud": baud,
            "runtime_counts": counts_json(counts),
            "process": metrics,
            "port_present": find_port(&port).is_ok(),
        }));
    }
    let after = process_metrics()?;
    let handle_delta = after["handles"].as_i64().unwrap_or_default()
        - before["handles"].as_i64().unwrap_or_default();
    let thread_delta = after["threads"].as_i64().unwrap_or_default()
        - before["threads"].as_i64().unwrap_or_default();
    Ok(json!({
        "command": "lifecycle",
        "cycles": cycles,
        "before": before,
        "after": after,
        "handle_delta": handle_delta,
        "thread_delta": thread_delta,
        "no_positive_growth": handle_delta <= 0 && thread_delta <= 0,
        "records": records,
    }))
}

fn run_sequence(arguments: &[String], artifact_dir: &Path) -> Result<Value, String> {
    let port: String = required_value(arguments, "--port")?;
    let steps = value_or(arguments, "--steps", 10_000_usize)?;
    if !(1..=10_000).contains(&steps) {
        return Err("--steps must be in 1..=10000".to_owned());
    }
    let harness = Harness::new(&port, ControllerOptions::default())?;
    harness.connect(ConnectOptions::default())?;
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
    let sequence = PreciseSequence::new(sequence_steps).map_err(|error| error.to_string())?;
    let started = harness.now_ns();
    let operation = harness
        .controller
        .precise_sequence(sequence)
        .map_err(|error| error.to_string())?;
    let expected_seconds = u64::try_from(steps)
        .expect("step count fits u64")
        .saturating_mul(30)
        .div_ceil(1_000)
        .saturating_add(60);
    wait_terminal(&operation, Duration::from_secs(expected_seconds))?;
    let ended = harness.now_ns();
    let reset = harness
        .controller
        .reset()
        .map_err(|error| error.to_string())?;
    wait_succeeded(&reset, OPERATION_TIMEOUT)?;
    fs::create_dir_all(artifact_dir).map_err(|error| error.to_string())?;
    let csv = artifact_dir.join("sequence-timings.csv");
    write_timing_csv(&csv, &harness.telemetry)?;
    let timing_count = harness
        .telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .timings
        .len();
    let result = json!({
        "command": "sequence",
        "requested_steps": steps,
        "operation": operation_json(&operation),
        "functional_success": operation.snapshot().state == OperationState::Succeeded,
        "accepted_report_count_before_final_reset": harness.controller.snapshot().accepted_report_count.saturating_sub(1),
        "recorded_complete_writes": timing_count,
        "elapsed_ns": ended.saturating_sub(started),
        "actual_baud": harness.actual_baud(),
        "latency": latency_json(&harness.telemetry, harness.actual_baud()),
        "timing_csv": csv,
        "physical_order_evidence": "open: no logic analyzer or firmware trace",
    });
    let counts = harness.close()?;
    Ok(with_close_counts(result, counts))
}

fn run_amiibo(arguments: &[String]) -> Result<Value, String> {
    if !arguments
        .iter()
        .any(|argument| argument == "--authorize-write")
    {
        return Ok(json!({
            "command": "amiibo",
            "capability": "unknown",
            "write_performed": false,
            "reason": "the v1 protocol has no safe capacity query; explicit limits and write authorization are required",
        }));
    }
    if !arguments
        .iter()
        .any(|argument| argument == "--confirm-disposable")
    {
        return Err("Amiibo write also requires --confirm-disposable".to_owned());
    }
    let port: String = required_value(arguments, "--port")?;
    let slot: u8 = required_value(arguments, "--slot")?;
    let slot_count: u16 = required_value(arguments, "--slot-count")?;
    let maximum_data_len: usize = required_value(arguments, "--maximum-data-len")?;
    let data_path = PathBuf::from(
        option_value(arguments, "--data")?
            .ok_or_else(|| "missing required option --data".to_owned())?,
    );
    let data = fs::read(&data_path).map_err(|error| error.to_string())?;
    let limits =
        AmiiboLimits::new(slot_count, maximum_data_len).map_err(|error| error.to_string())?;
    let harness = Harness::new(
        &port,
        ControllerOptions {
            amiibo_limits: Some(limits),
            ..ControllerOptions::default()
        },
    )?;
    harness.connect(ConnectOptions::default())?;
    let save = harness
        .controller
        .save_amiibo(slot, data, AmiiboSaveOptions::default())
        .map_err(|error| error.to_string())?;
    wait_succeeded(&save, Duration::from_secs(60))?;
    let select = harness
        .controller
        .select_amiibo(slot, AmiiboSelectOptions::default())
        .map_err(|error| error.to_string())?;
    wait_succeeded(&select, OPERATION_TIMEOUT)?;
    let result = json!({
        "command": "amiibo",
        "write_performed": true,
        "slot": slot,
        "slot_count": slot_count,
        "maximum_data_len": maximum_data_len,
        "data_path": data_path,
        "save": operation_json(&save),
        "select": operation_json(&select),
    });
    let counts = harness.close()?;
    Ok(with_close_counts(result, counts))
}

fn exercise_action(
    harness: &Harness,
    label: &str,
    action: ControllerAction,
    output: &mut Vec<Value>,
) -> Result<(), String> {
    let operation = harness
        .controller
        .direct(action)
        .map_err(|error| error.to_string())?;
    wait_succeeded(&operation, OPERATION_TIMEOUT)?;
    output.push(json!({"label": label, "operation": operation_json(&operation)}));
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

fn validate_home_wake(attempts: usize, interval_seconds: u64) -> Result<(), String> {
    if !(1..=100).contains(&attempts) {
        return Err("--attempts must be in 1..=100".to_owned());
    }
    if !(1..=60).contains(&interval_seconds) {
        return Err("--interval-seconds must be in 1..=60".to_owned());
    }
    Ok(())
}

fn home_attempt_marker(attempt: usize) -> String {
    attempt.to_string()
}

fn find_port(port_name: &str) -> Result<SerialPortDescriptor, String> {
    discover_system_ports()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|port| port.port_name().eq_ignore_ascii_case(port_name))
        .ok_or_else(|| format!("system discovery did not report {port_name}"))
}

fn wait_for_presence(stable_id: &str, present: bool, timeout: Duration) -> Result<(), String> {
    let started = std::time::Instant::now();
    while started.elapsed() < timeout {
        let found = discover_system_ports()
            .map_err(|error| error.to_string())?
            .iter()
            .any(|port| port.stable_id() == stable_id);
        if found == present {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(250));
    }
    Err(format!(
        "timed out waiting for stable identity to become {}",
        if present { "present" } else { "absent" }
    ))
}

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
        "error": snapshot.error.map(|error| error.to_string()),
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
    let snapshot = controller.snapshot();
    json!({
        "state": format!("{:?}", snapshot.state),
        "desired_report_neutral": snapshot.desired_report.is_neutral(),
        "accepted_report_count": snapshot.accepted_report_count,
        "last_report_timestamp_ns": snapshot.last_report_timestamp_ns,
        "lease": format!("{:?}", snapshot.lease),
    })
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

fn latency_json(telemetry: &Arc<Mutex<Telemetry>>, baud: Option<u32>) -> Value {
    let telemetry = telemetry.lock().unwrap_or_else(|error| error.into_inner());
    let direct: Vec<_> = telemetry
        .timings
        .iter()
        .filter(|sample| sample.command_admitted_ns.is_some())
        .copied()
        .collect();
    let admission_to_write: Vec<_> = direct
        .iter()
        .map(|sample| {
            sample
                .write_entered_ns
                .saturating_sub(sample.command_admitted_ns.expect("filtered direct timing"))
        })
        .collect();
    let write_call: Vec<_> = telemetry
        .timings
        .iter()
        .map(|sample| {
            sample
                .transport_accepted_ns
                .saturating_sub(sample.write_entered_ns)
        })
        .collect();
    let dispatch_to_write: Vec<_> = telemetry
        .timings
        .iter()
        .map(|sample| sample.write_entered_ns.saturating_sub(sample.dispatch_ns))
        .collect();
    json!({
        "sample_count": telemetry.timings.len(),
        "direct_sample_count": direct.len(),
        "command_admitted_to_write_entered_ns": distribution_json(&admission_to_write),
        "dispatch_to_write_entered_ns": distribution_json(&dispatch_to_write),
        "write_entered_to_os_acceptance_ns": distribution_json(&write_call),
        "uart_complete_frame": baud.map(|value| json!({
            "baud": value,
            "bytes": 8,
            "format": "8N1",
            "theoretical_ns": 80_000_000_000_u64 / u64::from(value),
            "measured": false,
        })),
        "usb_hid": {"measured": false, "reason": "USB analyzer or auditable firmware trace unavailable"},
    })
}

fn distribution_json(values: &[u64]) -> Value {
    distribution(values).map_or(
        Value::Null,
        |value| json!({"p50": value.p50, "p95": value.p95, "p99": value.p99, "max": value.max}),
    )
}

fn counts_json(counts: RuntimeCounts) -> Value {
    json!({
        "active_operations": counts.active_operations,
        "active_resources": counts.active_resources,
        "active_tasks": counts.active_tasks,
    })
}

fn with_close_counts(mut value: Value, counts: RuntimeCounts) -> Value {
    value["runtime_counts_after_close"] = counts_json(counts);
    value
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

fn write_timing_csv(path: &Path, telemetry: &Arc<Mutex<Telemetry>>) -> Result<(), String> {
    let file = File::create(path).map_err(|error| error.to_string())?;
    let mut writer = BufWriter::new(file);
    writeln!(
        writer,
        "sequence,command_admitted_ns,lane_wake_ns,dispatch_ns,write_entered_ns,transport_accepted_ns"
    )
    .map_err(|error| error.to_string())?;
    for sample in &telemetry
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .timings
    {
        writeln!(
            writer,
            "{},{},{},{},{},{}",
            sample.sequence,
            sample
                .command_admitted_ns
                .map_or_else(String::new, |value| value.to_string()),
            sample
                .lane_wake_ns
                .map_or_else(String::new, |value| value.to_string()),
            sample.dispatch_ns,
            sample.write_entered_ns,
            sample.transport_accepted_ns,
        )
        .map_err(|error| error.to_string())?;
    }
    writer.flush().map_err(|error| error.to_string())
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
         easycon-hardware-qualification handshake --port COMx\n  \
         easycon-hardware-qualification smoke --port COMx [--full] [--wake-left-stick] [--hold-ms N]\n  \
         easycon-hardware-qualification home-wake --port COMx [--attempts 20] [--interval-seconds 3]\n  \
         easycon-hardware-qualification faults --port COMx\n  \
         easycon-hardware-qualification hotplug --port COMx [--timeout-seconds N]\n  \
         easycon-hardware-qualification lifecycle --port COMx [--cycles 100]\n  \
         easycon-hardware-qualification sequence --port COMx [--steps 10000]\n  \
         easycon-hardware-qualification amiibo [--port COMx --slot N --slot-count N \
         --maximum-data-len N --data FILE --authorize-write --confirm-disposable]\n\n  \
         Every command accepts --output-dir PATH."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_command_still_produces_an_artifact_document() {
        let (document, error) = finalize_result("handshake", Err("protocol timeout".to_owned()));
        assert_eq!(document["status"], "failed");
        assert_eq!(document["command"], "handshake");
        assert_eq!(document["error"], "protocol timeout");
        assert_eq!(error.as_deref(), Some("protocol timeout"));
    }

    #[test]
    fn diagnostic_hold_is_bounded() {
        assert!(validate_hold_ms(0).is_ok());
        assert!(validate_hold_ms(5_000).is_ok());
        assert!(validate_hold_ms(5_001).is_err());
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
    fn home_wake_loop_is_bounded() {
        assert!(validate_home_wake(20, 3).is_ok());
        assert!(validate_home_wake(0, 3).is_err());
        assert!(validate_home_wake(101, 3).is_err());
        assert!(validate_home_wake(20, 0).is_err());
        assert!(validate_home_wake(20, 61).is_err());
    }

    #[test]
    fn home_attempt_marker_is_the_one_based_number_only() {
        assert_eq!(home_attempt_marker(1), "1");
        assert_eq!(home_attempt_marker(20), "20");
    }
}
