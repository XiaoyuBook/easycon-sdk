use std::env;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use easycon_controller::{ConnectOptions, ControllerAction, ControllerOptions, ControllerSession};
use easycon_model::StickPosition;
use easycon_runtime::{
    Clock, Operation, OperationState, Runtime, SystemClock, WaitResult, WaitTimeout,
};
use easycon_test_support::{DirectLatencySample, DirectLatencyTransport};

const DEFAULT_SAMPLES: usize = 10_000;
const DEFAULT_WARMUP: usize = 1_000;
const OPERATION_TIMEOUT: Duration = Duration::from_secs(10);

struct Config {
    samples: usize,
    warmup: usize,
    minimum_report_interval_ns: u64,
    machine: String,
    windows_build: String,
    power_plan: String,
    output: Option<PathBuf>,
}

#[derive(Clone, Copy)]
struct Distribution {
    p50_ns: u64,
    p95_ns: u64,
    p99_ns: u64,
    max_ns: u64,
}

fn main() {
    match parse_config().and_then(|config| config.map_or(Ok(()), run)) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("controller latency harness failed: {error}");
            std::process::exit(1);
        }
    }
}

fn parse_config() -> Result<Option<Config>, String> {
    let mut config = Config {
        samples: DEFAULT_SAMPLES,
        warmup: DEFAULT_WARMUP,
        minimum_report_interval_ns: 1,
        machine: env::var("COMPUTERNAME").unwrap_or_else(|_| "unknown".to_owned()),
        windows_build: "unspecified".to_owned(),
        power_plan: "unspecified".to_owned(),
        output: None,
    };
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--samples" => {
                config.samples = parse_value(&mut arguments, "--samples")?;
            }
            "--warmup" => {
                config.warmup = parse_value(&mut arguments, "--warmup")?;
            }
            "--minimum-report-interval-ns" => {
                config.minimum_report_interval_ns =
                    parse_value(&mut arguments, "--minimum-report-interval-ns")?;
            }
            "--machine" => config.machine = next_value(&mut arguments, "--machine")?,
            "--windows-build" => {
                config.windows_build = next_value(&mut arguments, "--windows-build")?;
            }
            "--power-plan" => config.power_plan = next_value(&mut arguments, "--power-plan")?,
            "--output" => {
                config.output = Some(PathBuf::from(next_value(&mut arguments, "--output")?));
            }
            "--help" | "-h" => {
                print_help();
                return Ok(None);
            }
            _ => return Err(format!("unknown argument: {argument}")),
        }
    }
    if config.samples == 0 || config.minimum_report_interval_ns == 0 {
        return Err("samples and minimum report interval must be non-zero".to_owned());
    }
    Ok(Some(config))
}

fn parse_value<T: std::str::FromStr>(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<T, String> {
    next_value(arguments, option)?
        .parse()
        .map_err(|_| format!("invalid value for {option}"))
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    option: &'static str,
) -> Result<String, String> {
    arguments
        .next()
        .ok_or_else(|| format!("missing value for {option}"))
}

fn print_help() {
    println!(
        "Usage: controller_latency [--samples N] [--warmup N] \
         [--minimum-report-interval-ns N] [--machine NAME] \
         [--windows-build BUILD] [--power-plan NAME] [--output CSV]"
    );
}

fn run(config: Config) -> Result<(), String> {
    let clock: Arc<dyn Clock> = Arc::new(SystemClock::default());
    let runtime = Runtime::new(clock.clone());
    let (transport, recorder) = DirectLatencyTransport::new(clock);
    let controller = ControllerSession::new(
        &runtime,
        Box::new(transport),
        ControllerOptions {
            minimum_report_interval_ns: config.minimum_report_interval_ns,
            ..ControllerOptions::default()
        },
    )
    .map_err(|error| error.to_string())?;
    let connect = controller
        .connect(ConnectOptions::default())
        .map_err(|error| error.to_string())?;
    wait_succeeded(&connect)?;

    for index in 0..config.warmup {
        submit_direct(&controller, index)?;
    }
    let warmup_last_acceptance_ns = recorder
        .samples()
        .last()
        .map(|sample| sample.transport_accepted_ns);
    recorder.clear();

    let measured_at = Instant::now();
    for index in 0..config.samples {
        submit_direct(&controller, config.warmup.saturating_add(index))?;
    }
    if !recorder.wait_for_samples(config.samples, OPERATION_TIMEOUT) {
        return Err("timed out waiting for complete latency samples".to_owned());
    }
    let elapsed = measured_at.elapsed();
    let samples = recorder.samples();
    if samples.len() != config.samples {
        return Err(format!(
            "expected {} samples but recorded {}",
            config.samples,
            samples.len()
        ));
    }
    validate_samples(
        &samples,
        warmup_last_acceptance_ns,
        config.minimum_report_interval_ns,
    )?;

    controller.close();
    if !recorder.is_closed() {
        return Err("latency transport did not close".to_owned());
    }
    runtime
        .close()
        .map_err(|error| format!("Runtime close rejected: {error:?}"))?;

    print_report(&config, elapsed, &samples);
    if let Some(path) = &config.output {
        write_csv(path, &samples)?;
    }
    Ok(())
}

fn submit_direct(controller: &ControllerSession, index: usize) -> Result<(), String> {
    let value = u16::try_from(index % usize::from(u16::MAX)).expect("bounded sample index");
    let [x, y] = value.to_le_bytes();
    let operation = controller
        .direct(ControllerAction::LeftStick(StickPosition::new(x, y)))
        .map_err(|error| error.to_string())?;
    wait_succeeded(&operation)
}

fn wait_succeeded(operation: &Operation) -> Result<(), String> {
    if !matches!(
        operation.wait(WaitTimeout::For(OPERATION_TIMEOUT)),
        WaitResult::Completed(_)
    ) {
        return Err(format!("operation {} timed out", operation.id().get()));
    }
    let snapshot = operation.snapshot();
    if snapshot.state != OperationState::Succeeded {
        return Err(format!(
            "operation {} ended in {:?}: {:?}",
            operation.id().get(),
            snapshot.state,
            snapshot.error
        ));
    }
    Ok(())
}

fn validate_samples(
    samples: &[DirectLatencySample],
    previous_acceptance_ns: Option<u64>,
    minimum_report_interval_ns: u64,
) -> Result<(), String> {
    let mut previous_acceptance_ns = previous_acceptance_ns;
    let mut previous_sequence = None;
    for (index, sample) in samples.iter().copied().enumerate() {
        if !sample.is_monotonic() {
            return Err(format!(
                "sample {index} has non-monotonic stages: {sample:?}"
            ));
        }
        if let Some(previous) = previous_acceptance_ns
            && sample.command_admitted_ns < previous.saturating_add(minimum_report_interval_ns)
        {
            return Err(format!(
                "sample {index} was not eligible: admission={} previous_acceptance={} interval={minimum_report_interval_ns}",
                sample.command_admitted_ns, previous
            ));
        }
        if let Some(previous) = previous_sequence
            && sample.write_sequence != previous + 1
        {
            return Err(format!("sample {index} is out of write order"));
        }
        previous_acceptance_ns = Some(sample.transport_accepted_ns);
        previous_sequence = Some(sample.write_sequence);
    }
    Ok(())
}

fn print_report(config: &Config, elapsed: Duration, samples: &[DirectLatencySample]) {
    println!("format=easycon-phase2a-controller-latency-v1");
    println!("hardware_unverified=true");
    println!("transport=non-blocking-memory");
    println!("machine={}", config.machine);
    println!("windows_build={}", config.windows_build);
    println!("power_plan={}", config.power_plan);
    println!("warmup_samples={}", config.warmup);
    println!("eligible_samples={}", samples.len());
    println!(
        "minimum_report_interval_ns={}",
        config.minimum_report_interval_ns
    );
    println!("measurement_elapsed_ms={}", elapsed.as_millis());
    print_distribution(
        "command_admitted_to_lane_wake_ns",
        samples.iter().map(|sample| {
            sample
                .lane_wake_ns
                .saturating_sub(sample.command_admitted_ns)
        }),
    );
    print_distribution(
        "lane_wake_to_dispatch_ns",
        samples
            .iter()
            .map(|sample| sample.lane_dispatch_ns.saturating_sub(sample.lane_wake_ns)),
    );
    print_distribution(
        "dispatch_to_transport_write_entered_ns",
        samples.iter().map(|sample| {
            sample
                .transport_write_entered_ns
                .saturating_sub(sample.lane_dispatch_ns)
        }),
    );
    print_distribution(
        "transport_write_entered_to_acceptance_ns",
        samples.iter().map(|sample| {
            sample
                .transport_accepted_ns
                .saturating_sub(sample.transport_write_entered_ns)
        }),
    );
    print_distribution(
        "command_admitted_to_transport_write_entered_ns",
        samples
            .iter()
            .copied()
            .map(DirectLatencySample::admitted_to_write_entered_ns),
    );
    print_distribution(
        "command_admitted_to_transport_acceptance_ns",
        samples.iter().map(|sample| {
            sample
                .transport_accepted_ns
                .saturating_sub(sample.command_admitted_ns)
        }),
    );
}

fn print_distribution(metric: &str, values: impl Iterator<Item = u64>) {
    let distribution = distribution(values);
    println!(
        "metric={metric} p50_ns={} p95_ns={} p99_ns={} max_ns={}",
        distribution.p50_ns, distribution.p95_ns, distribution.p99_ns, distribution.max_ns
    );
}

fn distribution(values: impl Iterator<Item = u64>) -> Distribution {
    let mut values: Vec<_> = values.collect();
    assert!(
        !values.is_empty(),
        "validated sample population is non-empty"
    );
    values.sort_unstable();
    Distribution {
        p50_ns: percentile(&values, 50),
        p95_ns: percentile(&values, 95),
        p99_ns: percentile(&values, 99),
        max_ns: *values.last().expect("non-empty distribution"),
    }
}

fn percentile(sorted: &[u64], percent: usize) -> u64 {
    let rank = sorted.len().saturating_mul(percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn write_csv(path: &PathBuf, samples: &[DirectLatencySample]) -> Result<(), String> {
    let file = File::create(path)
        .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
    let mut output = BufWriter::new(file);
    writeln!(
        output,
        "operation_id,write_sequence,command_admitted_ns,lane_wake_ns,lane_dispatch_ns,transport_write_entered_ns,transport_accepted_ns,admitted_to_write_entered_ns"
    )
    .map_err(|error| error.to_string())?;
    for sample in samples {
        writeln!(
            output,
            "{},{},{},{},{},{},{},{}",
            sample.operation_id.get(),
            sample.write_sequence,
            sample.command_admitted_ns,
            sample.lane_wake_ns,
            sample.lane_dispatch_ns,
            sample.transport_write_entered_ns,
            sample.transport_accepted_ns,
            sample.admitted_to_write_entered_ns(),
        )
        .map_err(|error| error.to_string())?;
    }
    output.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_rank_distribution_keeps_the_maximum() {
        let result = distribution(1_u64..=100);
        assert_eq!(result.p50_ns, 50);
        assert_eq!(result.p95_ns, 95);
        assert_eq!(result.p99_ns, 99);
        assert_eq!(result.max_ns, 100);
    }
}
