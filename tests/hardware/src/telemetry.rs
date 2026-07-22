use easycon_controller::{TransportError, WriteContext};
use easycon_hardware_qualification::distribution;
use easycon_serial::SerialError;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
struct PendingCall {
    remaining: usize,
    entered_ns: u64,
    native_error: Option<SerialError>,
}

#[derive(Clone, Debug)]
enum LogicalReportOutcome {
    Pending,
    Accepted,
    Failed(TransportError),
    Contradiction(TelemetryContradiction),
}

#[derive(Clone, Debug)]
struct TelemetryContradiction {
    code: &'static str,
    message: String,
}

#[derive(Clone, Debug)]
pub(crate) struct LogicalReportAttempt {
    context: WriteContext,
    first_write_entered_ns: u64,
    transport_accepted_ns: Option<u64>,
    last_returned_ns: Option<u64>,
    partial_count: usize,
    accepted_bytes: usize,
    native_error: Option<SerialError>,
    pending_call: Option<PendingCall>,
    outcome: LogicalReportOutcome,
}

#[derive(Clone, Debug)]
struct GlobalTelemetryError {
    code: &'static str,
    message: String,
    write_sequence: Option<u64>,
}

#[derive(Default)]
pub(crate) struct LogicalReportTelemetry {
    attempts: Vec<LogicalReportAttempt>,
    global_errors: Vec<GlobalTelemetryError>,
}

impl LogicalReportAttempt {
    fn contradict(&mut self, code: &'static str, message: impl Into<String>) {
        if !matches!(self.outcome, LogicalReportOutcome::Contradiction(_)) {
            self.outcome = LogicalReportOutcome::Contradiction(TelemetryContradiction {
                code,
                message: message.into(),
            });
        }
    }

    fn outcome_name(&self) -> &'static str {
        match self.outcome {
            LogicalReportOutcome::Pending => "pending",
            LogicalReportOutcome::Accepted => "accepted",
            LogicalReportOutcome::Failed(_) => "failed",
            LogicalReportOutcome::Contradiction(_) => "contradiction",
        }
    }

    fn transport_error(&self) -> Option<&TransportError> {
        match &self.outcome {
            LogicalReportOutcome::Failed(error) => Some(error),
            LogicalReportOutcome::Pending
            | LogicalReportOutcome::Accepted
            | LogicalReportOutcome::Contradiction(_) => None,
        }
    }

    fn contradiction(&self) -> Option<&TelemetryContradiction> {
        match &self.outcome {
            LogicalReportOutcome::Contradiction(contradiction) => Some(contradiction),
            LogicalReportOutcome::Pending
            | LogicalReportOutcome::Accepted
            | LogicalReportOutcome::Failed(_) => None,
        }
    }

    fn json(&self) -> Value {
        let transport_error = self.transport_error().map(|error| {
            json!({
                "kind": format!("{:?}", error.kind()),
                "message": error.message(),
            })
        });
        let native_error = self.native_error.as_ref().map(serial_error_json);
        let contradiction = self.contradiction().map(|contradiction| {
            json!({
                "code": contradiction.code,
                "message": contradiction.message,
            })
        });
        json!({
            "resource_id": self.context.resource_id.get(),
            "operation_id": self.context.operation_id.map(|id| id.get()),
            "write_sequence": self.context.sequence,
            "command_admitted_ns": self.context.direct_timing.map(|timing| timing.command_admitted_ns),
            "lane_wake_ns": self.context.direct_timing.map(|timing| timing.lane_wake_ns),
            "dispatch_ns": self.context.timestamp_ns,
            "first_write_entered_ns": self.first_write_entered_ns,
            "transport_accepted_ns": self.transport_accepted_ns,
            "last_returned_ns": self.last_returned_ns,
            "partial_count": self.partial_count,
            "total_bytes": self.context.total_len,
            "accepted_bytes": self.accepted_bytes,
            "outcome": self.outcome_name(),
            "transport_error": transport_error,
            "native_error": native_error,
            "contradiction": contradiction,
        })
    }
}

impl LogicalReportTelemetry {
    pub(crate) fn begin(&mut self, context: WriteContext, remaining: usize, entered_ns: u64) {
        if let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.context.sequence == context.sequence)
        {
            if attempt.context != context {
                attempt.contradict(
                    "context_changed",
                    "logical report context changed across partial writes",
                );
            } else if !matches!(attempt.outcome, LogicalReportOutcome::Pending) {
                attempt.contradict(
                    "resumed_after_terminal",
                    "logical report resumed after a terminal observation",
                );
            } else if attempt.pending_call.is_some() {
                attempt.contradict(
                    "overlapping_calls",
                    "logical report entered a second transport call before the first returned",
                );
            } else if context.total_len.checked_sub(remaining) != Some(attempt.accepted_bytes) {
                attempt.contradict(
                    "noncontiguous_prefix",
                    "logical report partial-write prefix was not contiguous",
                );
            } else if attempt
                .last_returned_ns
                .is_some_and(|returned_ns| entered_ns < returned_ns)
            {
                attempt.contradict(
                    "call_time_reversed",
                    "logical report call entered before the prior call returned",
                );
            }
            attempt.pending_call = Some(PendingCall {
                remaining,
                entered_ns,
                native_error: None,
            });
            return;
        }

        let mut attempt = LogicalReportAttempt {
            context,
            first_write_entered_ns: entered_ns,
            transport_accepted_ns: None,
            last_returned_ns: None,
            partial_count: 0,
            accepted_bytes: 0,
            native_error: None,
            pending_call: Some(PendingCall {
                remaining,
                entered_ns,
                native_error: None,
            }),
            outcome: LogicalReportOutcome::Pending,
        };
        if context.sequence == 0 {
            attempt.contradict("zero_sequence", "logical report write sequence was zero");
        } else if context.operation_id.is_none() {
            attempt.contradict(
                "missing_operation_id",
                "logical report did not carry an operation ID",
            );
        } else if context.total_len == 0 || remaining == 0 {
            attempt.contradict(
                "empty_payload",
                "logical report payload and remainder must be non-zero",
            );
        } else if remaining != context.total_len {
            attempt.contradict(
                "missing_initial_prefix",
                "logical report began without the complete payload",
            );
        } else if let Some(previous) = self.attempts.last() {
            if previous.context.sequence >= context.sequence {
                attempt.contradict(
                    "sequence_not_increasing",
                    "logical report write sequence was not strictly increasing",
                );
            } else if !matches!(
                previous.outcome,
                LogicalReportOutcome::Accepted | LogicalReportOutcome::Failed(_)
            ) || previous.pending_call.is_some()
            {
                attempt.contradict(
                    "prior_report_not_terminal",
                    "a new logical report began before the prior report was terminal",
                );
            } else if context.timestamp_ns < previous.context.timestamp_ns {
                attempt.contradict(
                    "dispatch_sequence_reversed",
                    "logical report dispatch timestamps reversed across write sequences",
                );
            } else if previous
                .last_returned_ns
                .is_some_and(|returned_ns| entered_ns < returned_ns)
            {
                attempt.contradict(
                    "report_time_reversed",
                    "logical report entered before the prior report returned",
                );
            }
        }
        if let Some(timing) = context.direct_timing
            && timing.command_admitted_ns > timing.lane_wake_ns
        {
            attempt.contradict(
                "admission_after_lane_wake",
                "command admission occurred after lane wake",
            );
        }
        if let Some(timing) = context.direct_timing
            && timing.lane_wake_ns > context.timestamp_ns
        {
            attempt.contradict(
                "lane_wake_after_dispatch",
                "lane wake occurred after dispatch",
            );
        }
        if context.timestamp_ns > entered_ns {
            attempt.contradict(
                "dispatch_after_write_entry",
                "logical report dispatch occurred after transport entry",
            );
        }
        self.attempts.push(attempt);
    }

    pub(crate) fn record_native_error(&mut self, context: WriteContext, error: &SerialError) {
        let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.context.sequence == context.sequence)
        else {
            self.global_errors.push(GlobalTelemetryError {
                code: "orphan_native_error",
                message: "native Controller write error had no matching logical report".to_owned(),
                write_sequence: Some(context.sequence),
            });
            return;
        };
        if attempt.context != context {
            attempt.contradict(
                "native_error_context_mismatch",
                "native Controller write error did not match the logical report context",
            );
            return;
        }
        let Some(call) = attempt.pending_call.as_mut() else {
            attempt.contradict(
                "native_error_without_call",
                "native Controller write error arrived outside an active transport call",
            );
            return;
        };
        if call.native_error.is_some() || attempt.native_error.is_some() {
            attempt.contradict(
                "duplicate_native_error",
                "logical report observed more than one native terminal error",
            );
            return;
        }
        call.native_error = Some(error.clone());
    }

    pub(crate) fn finish(
        &mut self,
        context: WriteContext,
        remaining: usize,
        returned_ns: u64,
        result: &Result<usize, TransportError>,
    ) {
        let Some(attempt) = self
            .attempts
            .iter_mut()
            .find(|attempt| attempt.context.sequence == context.sequence)
        else {
            self.global_errors.push(GlobalTelemetryError {
                code: "orphan_transport_result",
                message: "logical report result had no matching transport entry".to_owned(),
                write_sequence: Some(context.sequence),
            });
            return;
        };
        let Some(call) = attempt.pending_call.take() else {
            attempt.contradict(
                "result_without_call",
                "logical report result had no active transport call",
            );
            return;
        };
        if result.is_err() {
            attempt.native_error.clone_from(&call.native_error);
        }
        attempt.last_returned_ns = Some(returned_ns);
        if attempt.context != context || call.remaining != remaining {
            attempt.contradict(
                "result_context_mismatch",
                "logical report result did not match its transport entry",
            );
            return;
        }
        if returned_ns < call.entered_ns {
            attempt.contradict(
                "return_before_entry",
                "logical report transport returned before its entry timestamp",
            );
            return;
        }
        if matches!(attempt.outcome, LogicalReportOutcome::Contradiction(_)) {
            return;
        }
        let Some(offset) = context.total_len.checked_sub(remaining) else {
            attempt.contradict(
                "remainder_exceeds_total",
                "logical report remainder exceeded its total length",
            );
            return;
        };
        if attempt.accepted_bytes != offset {
            attempt.contradict(
                "result_prefix_mismatch",
                "logical report result did not match its accepted prefix",
            );
            return;
        }

        match result {
            Ok(written) => {
                if call.native_error.is_some() {
                    attempt.contradict(
                        "native_error_with_success",
                        "native error was observed for a successful transport call",
                    );
                    return;
                }
                if *written == 0 || *written > remaining {
                    attempt.contradict(
                        "invalid_progress",
                        "logical report transport returned invalid write progress",
                    );
                    return;
                }
                let Some(partial_count) = attempt.partial_count.checked_add(1) else {
                    attempt.contradict(
                        "partial_count_overflow",
                        "logical report partial count overflowed",
                    );
                    return;
                };
                let Some(accepted_bytes) = offset.checked_add(*written) else {
                    attempt.contradict(
                        "accepted_count_overflow",
                        "logical report accepted-byte count overflowed",
                    );
                    return;
                };
                if accepted_bytes > context.total_len {
                    attempt.contradict(
                        "accepted_beyond_total",
                        "logical report accepted beyond its total length",
                    );
                    return;
                }
                attempt.partial_count = partial_count;
                attempt.accepted_bytes = accepted_bytes;
                if accepted_bytes == context.total_len {
                    attempt.transport_accepted_ns = Some(returned_ns);
                    attempt.outcome = LogicalReportOutcome::Accepted;
                }
            }
            Err(error) => {
                attempt.outcome = LogicalReportOutcome::Failed(error.clone());
            }
        }
    }

    pub(crate) fn accepted_count(&self) -> usize {
        self.attempts
            .iter()
            .filter(|attempt| matches!(attempt.outcome, LogicalReportOutcome::Accepted))
            .count()
    }

    pub(crate) fn attempt_count(&self) -> usize {
        self.attempts.len()
    }

    pub(crate) fn integrity_passed(&self) -> bool {
        self.global_errors.is_empty()
            && self.attempts.iter().all(|attempt| {
                !matches!(
                    attempt.outcome,
                    LogicalReportOutcome::Pending | LogicalReportOutcome::Contradiction(_)
                ) && attempt.pending_call.is_none()
            })
    }

    pub(crate) fn projection_json(&self, baud: Option<u32>, csv_path: Option<&str>) -> Value {
        let accepted = self
            .attempts
            .iter()
            .filter(|attempt| matches!(attempt.outcome, LogicalReportOutcome::Accepted))
            .collect::<Vec<_>>();
        let direct = accepted
            .iter()
            .filter(|attempt| attempt.context.direct_timing.is_some())
            .copied()
            .collect::<Vec<_>>();
        let admission_to_write = direct
            .iter()
            .filter_map(|attempt| {
                attempt.first_write_entered_ns.checked_sub(
                    attempt
                        .context
                        .direct_timing
                        .expect("filtered direct timing")
                        .command_admitted_ns,
                )
            })
            .collect::<Vec<_>>();
        let dispatch_to_write = accepted
            .iter()
            .filter_map(|attempt| {
                attempt
                    .first_write_entered_ns
                    .checked_sub(attempt.context.timestamp_ns)
            })
            .collect::<Vec<_>>();
        let write_call = accepted
            .iter()
            .filter_map(|attempt| {
                attempt
                    .transport_accepted_ns
                    .and_then(|accepted_ns| accepted_ns.checked_sub(attempt.first_write_entered_ns))
            })
            .collect::<Vec<_>>();
        let errors = self.integrity_errors_json();
        let detail = csv_path.map_or_else(
            || {
                json!({
                    "kind": "inline",
                    "rows": self.attempts.iter().map(LogicalReportAttempt::json).collect::<Vec<_>>(),
                })
            },
            |relative_path| {
                json!({
                    "kind": "csv",
                    "relative_path": relative_path,
                })
            },
        );
        json!({
            "integrity": {
                "status": if self.integrity_passed() { "passed" } else { "failed" },
                "errors": errors,
            },
            "logical_reports": {
                "attempt_count": self.attempts.len(),
                "accepted_count": accepted.len(),
                "failed_count": self.attempts.iter().filter(|attempt| matches!(attempt.outcome, LogicalReportOutcome::Failed(_))).count(),
                "pending_count": self.attempts.iter().filter(|attempt| matches!(attempt.outcome, LogicalReportOutcome::Pending)).count(),
                "contradiction_count": self.attempts.iter().filter(|attempt| matches!(attempt.outcome, LogicalReportOutcome::Contradiction(_))).count(),
                "detail": detail,
            },
            "sample_count": accepted.len(),
            "direct_sample_count": direct.len(),
            "command_admitted_to_write_entered_ns": distribution_json(&admission_to_write),
            "dispatch_to_write_entered_ns": distribution_json(&dispatch_to_write),
            "write_entered_to_os_acceptance_ns": distribution_json(&write_call),
            "uart_complete_frame": baud.map(|value| json!({
                "classification": "theoretical",
                "baud": value,
                "bytes": 8,
                "format": "8N1",
                "theoretical_ns": 80_000_000_000_u64 / u64::from(value),
                "measured": false,
            })),
            "usb_hid": {
                "qualification_status": "unverified",
                "measured": false,
                "reason": "USB analyzer or auditable firmware trace unavailable",
            },
            "switch_physical_order": {
                "qualification_status": "unverified",
                "measured": false,
                "reason": "analyzer or firmware trace correlated with physical observation unavailable",
            },
        })
    }

    fn integrity_errors_json(&self) -> Vec<Value> {
        let mut errors = self
            .global_errors
            .iter()
            .map(|error| {
                json!({
                    "code": error.code,
                    "message": error.message,
                    "write_sequence": error.write_sequence,
                })
            })
            .collect::<Vec<_>>();
        errors.extend(self.attempts.iter().filter_map(|attempt| {
            let contradiction = match &attempt.outcome {
                LogicalReportOutcome::Contradiction(contradiction) => contradiction,
                LogicalReportOutcome::Pending => {
                    return Some(json!({
                        "code": "pending_logical_report",
                        "message": "logical report did not reach a terminal transport observation",
                        "write_sequence": attempt.context.sequence,
                    }));
                }
                LogicalReportOutcome::Accepted | LogicalReportOutcome::Failed(_) => return None,
            };
            Some(json!({
                "code": contradiction.code,
                "message": contradiction.message,
                "write_sequence": attempt.context.sequence,
            }))
        }));
        errors
    }

    pub(crate) fn csv_bytes(&self) -> Result<Vec<u8>, String> {
        let mut writer = csv::WriterBuilder::new()
            .has_headers(false)
            .terminator(csv::Terminator::Any(b'\n'))
            .from_writer(Vec::new());
        writer
            .write_record(CSV_HEADER)
            .map_err(|error| error.to_string())?;
        for attempt in &self.attempts {
            let transport_error = attempt.transport_error();
            let native_error = attempt.native_error.as_ref();
            let contradiction = attempt.contradiction();
            writer
                .write_record([
                    attempt.context.sequence.to_string(),
                    optional_u64(attempt.context.operation_id.map(|id| id.get())),
                    attempt.context.resource_id.get().to_string(),
                    optional_u64(
                        attempt
                            .context
                            .direct_timing
                            .map(|timing| timing.command_admitted_ns),
                    ),
                    optional_u64(
                        attempt
                            .context
                            .direct_timing
                            .map(|timing| timing.lane_wake_ns),
                    ),
                    attempt.context.timestamp_ns.to_string(),
                    attempt.first_write_entered_ns.to_string(),
                    optional_u64(attempt.transport_accepted_ns),
                    optional_u64(attempt.last_returned_ns),
                    attempt.partial_count.to_string(),
                    attempt.context.total_len.to_string(),
                    attempt.accepted_bytes.to_string(),
                    attempt.outcome_name().to_owned(),
                    transport_error
                        .map(|error| format!("{:?}", error.kind()))
                        .unwrap_or_default(),
                    transport_error
                        .map(|error| error.message().to_owned())
                        .unwrap_or_default(),
                    native_error
                        .map(|error| format!("{:?}", error.kind()))
                        .unwrap_or_default(),
                    optional_u64(native_error.and_then(SerialError::os_code).map(u64::from)),
                    native_error
                        .map(|error| error.message().to_owned())
                        .unwrap_or_default(),
                    contradiction
                        .map(|value| value.code.to_owned())
                        .unwrap_or_default(),
                    contradiction
                        .map(|value| value.message.clone())
                        .unwrap_or_default(),
                ])
                .map_err(|error| error.to_string())?;
        }
        writer.into_inner().map_err(|error| error.to_string())
    }

    #[cfg(test)]
    pub(crate) fn attempts(&self) -> &[LogicalReportAttempt] {
        &self.attempts
    }
}

pub(crate) const CSV_HEADER: [&str; 20] = [
    "write_sequence",
    "operation_id",
    "resource_id",
    "command_admitted_ns",
    "lane_wake_ns",
    "dispatch_ns",
    "first_write_entered_ns",
    "transport_accepted_ns",
    "last_returned_ns",
    "partial_count",
    "total_bytes",
    "accepted_bytes",
    "outcome",
    "transport_error_kind",
    "transport_error_message",
    "native_error_kind",
    "native_os_code",
    "native_error_message",
    "contradiction_code",
    "contradiction_message",
];

fn optional_u64(value: Option<u64>) -> String {
    value.map_or_else(String::new, |value| value.to_string())
}

fn distribution_json(values: &[u64]) -> Value {
    distribution(values).map_or(
        Value::Null,
        |value| json!({"p50": value.p50, "p95": value.p95, "p99": value.p99, "max": value.max}),
    )
}

fn serial_error_json(error: &SerialError) -> Value {
    json!({
        "kind": format!("{:?}", error.kind()),
        "os_code": error.os_code(),
        "message": error.message(),
    })
}

#[cfg(test)]
mod tests {
    use easycon_controller::{DirectWriteTiming, TransportErrorKind, WriteKind};
    use easycon_model::{OperationId, ResourceId};
    use easycon_serial::SerialErrorKind;

    use super::*;

    fn context(sequence: u64) -> WriteContext {
        WriteContext {
            resource_id: ResourceId::new(7),
            operation_id: Some(OperationId::new(11)),
            sequence,
            timestamp_ns: 90,
            direct_timing: Some(DirectWriteTiming {
                command_admitted_ns: 70,
                lane_wake_ns: 80,
            }),
            total_len: 8,
            kind: WriteKind::Report,
        }
    }

    #[test]
    fn partial_segments_keep_first_entry_and_final_acceptance() {
        let mut telemetry = LogicalReportTelemetry::default();
        let context = context(13);

        telemetry.begin(context, 8, 100);
        telemetry.finish(context, 8, 110, &Ok(3));
        telemetry.begin(context, 5, 120);
        telemetry.finish(context, 5, 130, &Ok(5));

        let attempt = &telemetry.attempts()[0];
        assert_eq!(attempt.first_write_entered_ns, 100);
        assert_eq!(attempt.transport_accepted_ns, Some(130));
        assert_eq!(attempt.partial_count, 2);
        assert_eq!(attempt.accepted_bytes, 8);
        assert_eq!(attempt.outcome_name(), "accepted");
        assert!(telemetry.integrity_passed());
    }

    #[test]
    fn native_failure_keeps_prefix_kind_and_os_code() {
        let mut telemetry = LogicalReportTelemetry::default();
        let context = context(13);

        telemetry.begin(context, 8, 100);
        telemetry.finish(context, 8, 110, &Ok(3));
        telemetry.begin(context, 5, 120);
        telemetry.record_native_error(
            context,
            &SerialError::with_os_code(SerialErrorKind::Io, "injected write failure", 995),
        );
        telemetry.finish(
            context,
            5,
            130,
            &Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "mapped transport failure",
            )),
        );

        let row = telemetry.attempts()[0].json();
        assert_eq!(row["accepted_bytes"], 3);
        assert_eq!(row["partial_count"], 1);
        assert_eq!(row["outcome"], "failed");
        assert_eq!(row["transport_error"]["kind"], "Disconnected");
        assert_eq!(row["native_error"]["kind"], "Io");
        assert_eq!(row["native_error"]["os_code"], 995);
        assert!(telemetry.integrity_passed());
    }

    #[test]
    fn backwards_time_is_a_stable_contradiction() {
        let mut telemetry = LogicalReportTelemetry::default();
        let context = context(13);

        telemetry.begin(context, 8, 89);
        telemetry.finish(context, 8, 88, &Ok(8));

        let projection = telemetry.projection_json(Some(115_200), None);
        assert_eq!(projection["integrity"]["status"], "failed");
        assert_eq!(
            projection["integrity"]["errors"][0]["code"],
            "dispatch_after_write_entry"
        );
        assert_eq!(projection["logical_reports"]["accepted_count"], 0);
    }

    #[test]
    fn csv_keeps_failed_native_structure_and_escapes_diagnostics() {
        let mut telemetry = LogicalReportTelemetry::default();
        let context = context(13);
        telemetry.begin(context, 8, 100);
        telemetry.finish(context, 8, 110, &Ok(3));
        telemetry.begin(context, 5, 120);
        telemetry.record_native_error(
            context,
            &SerialError::with_os_code(SerialErrorKind::Io, "native, \"quoted\"\nerror", 995),
        );
        telemetry.finish(
            context,
            5,
            130,
            &Err(TransportError::new(
                TransportErrorKind::Disconnected,
                "mapped, \"quoted\"\nerror",
            )),
        );

        let bytes = telemetry.csv_bytes().expect("CSV bytes");
        let mut reader = csv::ReaderBuilder::new().from_reader(bytes.as_slice());
        assert_eq!(reader.headers().expect("header"), CSV_HEADER.as_slice());
        let records = reader
            .records()
            .collect::<Result<Vec<_>, _>>()
            .expect("CSV records");
        assert_eq!(records.len(), 1);
        let record = &records[0];
        let field = |name: &str| {
            let index = CSV_HEADER
                .iter()
                .position(|field| *field == name)
                .expect("known CSV field");
            record.get(index).expect("CSV value")
        };
        assert_eq!(field("partial_count"), "1");
        assert_eq!(field("accepted_bytes"), "3");
        assert_eq!(field("outcome"), "failed");
        assert_eq!(field("transport_error_kind"), "Disconnected");
        assert_eq!(
            field("transport_error_message"),
            "mapped, \"quoted\"\nerror"
        );
        assert_eq!(field("native_error_kind"), "Io");
        assert_eq!(field("native_os_code"), "995");
        assert_eq!(field("native_error_message"), "native, \"quoted\"\nerror");
    }

    #[test]
    fn timestamps_cannot_reverse_between_report_sequences() {
        let mut telemetry = LogicalReportTelemetry::default();
        let first = context(13);
        telemetry.begin(first, 8, 100);
        telemetry.finish(first, 8, 110, &Ok(8));
        let mut second = context(14);
        second.timestamp_ns = 85;
        telemetry.begin(second, 8, 90);
        telemetry.finish(second, 8, 95, &Ok(8));

        let projection = telemetry.projection_json(None, None);
        assert_eq!(projection["integrity"]["status"], "failed");
        assert_eq!(
            projection["integrity"]["errors"][0]["code"],
            "dispatch_sequence_reversed"
        );
        assert_eq!(projection["logical_reports"]["accepted_count"], 1);
        assert_eq!(projection["logical_reports"]["contradiction_count"], 1);
    }
}
