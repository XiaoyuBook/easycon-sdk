use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use easycon_controller::{
    AckFrame, AckRequest, ControllerTransport, HandshakeRequest, TransportError,
    TransportErrorKind, WriteContext, WriteKind, WriteRequest,
};
#[cfg(test)]
use easycon_controller::WriteSettlement;
use easycon_model::OperationId;
use serde_json::{Value, json};

use crate::journal::{JournalEventKind, JournalWriter};

const READY: u8 = 0xa5;
const SAVE: u8 = 0x90;
const SELECT: u8 = 0x91;
const MAX_CHUNK_LEN: usize = 20;
const RESET: [u8; 6] = [0xa5, 0x81, 0xa5, 0x81, 0xa5, 0x81];

pub(crate) struct AmiiboEvidenceBinding {
    pub(crate) lease_id: String,
    pub(crate) expected_stable_id: String,
    pub(crate) observed_stable_id: String,
    pub(crate) slot: u8,
    pub(crate) payload_len: usize,
    pub(crate) payload_sha256: String,
}

pub(crate) struct AmiiboEvidenceConfig {
    pub(crate) journal: JournalWriter,
    pub(crate) binding: AmiiboEvidenceBinding,
    pub(crate) recorder: AmiiboEvidenceRecorder,
}

#[derive(Clone, Default)]
pub(crate) struct AmiiboEvidenceRecorder {
    trace: Arc<Mutex<AmiiboTrace>>,
}

#[derive(Default)]
struct AmiiboTrace {
    chunks: Vec<Value>,
    selects: Vec<Value>,
    cleanup_resets: Vec<Value>,
    evidence_errors: Vec<String>,
    transport_closed_in_state: Option<String>,
}

impl AmiiboEvidenceRecorder {
    pub(crate) fn projection(&self) -> Value {
        match self.trace.lock() {
            Ok(trace) => json!({
                "chunks": trace.chunks,
                "selects": trace.selects,
                "cleanup_resets": trace.cleanup_resets,
                "evidence_errors": trace.evidence_errors,
                "transport_closed_in_state": trace.transport_closed_in_state,
            }),
            Err(_) => json!({
                "chunks": [],
                "selects": [],
                "cleanup_resets": [],
                "evidence_errors": ["Amiibo evidence trace lock is poisoned"],
                "transport_closed_in_state": "unknown",
            }),
        }
    }

    fn begin_chunk(&self, payload: Value) -> Result<usize, TransportError> {
        let mut trace = lock_trace(&self.trace)?;
        trace.chunks.push(payload);
        Ok(trace.chunks.len() - 1)
    }

    fn begin_select(&self, payload: Value) -> Result<usize, TransportError> {
        let mut trace = lock_trace(&self.trace)?;
        trace.selects.push(payload);
        Ok(trace.selects.len() - 1)
    }

    fn begin_cleanup(&self, payload: Value) -> Result<usize, TransportError> {
        let mut trace = lock_trace(&self.trace)?;
        trace.cleanup_resets.push(payload);
        Ok(trace.cleanup_resets.len() - 1)
    }

    fn update(&self, exchange: &Exchange, field: &str, value: Value) -> Result<(), TransportError> {
        let mut trace = lock_trace(&self.trace)?;
        let record = match exchange {
            Exchange::Chunk { trace_index, .. } => trace.chunks.get_mut(*trace_index),
            Exchange::Select { trace_index, .. } => trace.selects.get_mut(*trace_index),
            Exchange::Cleanup { trace_index, .. } => trace.cleanup_resets.get_mut(*trace_index),
        }
        .ok_or_else(|| protocol_error("Amiibo evidence trace index is invalid"))?;
        record[field] = value;
        Ok(())
    }

    fn evidence_error(&self, error: impl Into<String>) {
        if let Ok(mut trace) = self.trace.lock() {
            trace.evidence_errors.push(error.into());
        }
    }

    fn record_close(&self, state: &EvidenceState) {
        if let Ok(mut trace) = self.trace.lock() {
            trace.transport_closed_in_state = Some(state.as_str().to_owned());
        }
    }
}

fn lock_trace(trace: &Mutex<AmiiboTrace>) -> Result<MutexGuard<'_, AmiiboTrace>, TransportError> {
    trace
        .lock()
        .map_err(|_| io_error("Amiibo evidence trace lock is poisoned"))
}

pub(crate) struct AmiiboEvidenceTransport {
    inner: Box<dyn ControllerTransport>,
    journal: JournalWriter,
    binding: AmiiboEvidenceBinding,
    recorder: AmiiboEvidenceRecorder,
    state: EvidenceState,
    next_offset: usize,
    attempts: BTreeMap<usize, u32>,
    save_operation_id: Option<OperationId>,
}

#[derive(Clone)]
enum EvidenceState {
    Ready,
    Writing(ActiveWrite),
    AwaitingAck(Exchange),
    HeaderAcked(ChunkKey),
    Recovering(ResumeState, OperationId),
    SaveComplete,
    Complete,
    Failed,
}

impl EvidenceState {
    const fn as_str(&self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Writing(_) => "writing",
            Self::AwaitingAck(_) => "awaiting_ack",
            Self::HeaderAcked(_) => "header_acked",
            Self::Recovering(_, _) => "recovering",
            Self::SaveComplete => "save_complete",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone)]
struct ActiveWrite {
    context: WriteContext,
    accepted: usize,
    exchange: Exchange,
    phase: WritePhase,
}

#[derive(Clone)]
enum Exchange {
    Chunk {
        trace_index: usize,
        key: ChunkKey,
    },
    Select {
        trace_index: usize,
        operation_id: OperationId,
    },
    Cleanup {
        trace_index: usize,
        operation_id: OperationId,
        resume: ResumeState,
    },
}

impl Exchange {
    const fn operation_id(&self) -> OperationId {
        match self {
            Self::Chunk { key, .. } => key.operation_id,
            Self::Select { operation_id, .. } | Self::Cleanup { operation_id, .. } => *operation_id,
        }
    }
}

#[derive(Clone, Copy)]
struct ChunkKey {
    operation_id: OperationId,
    offset: usize,
    length: usize,
    attempt: u32,
}

#[derive(Clone, Copy)]
enum WritePhase {
    Header,
    Payload,
    Select,
    CleanupReset,
}

impl WritePhase {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Header => "header",
            Self::Payload => "payload",
            Self::Select => "select",
            Self::CleanupReset => "cleanup_reset",
        }
    }
}

#[derive(Clone, Copy)]
enum ResumeState {
    Save,
    Select,
}

impl AmiiboEvidenceTransport {
    pub(crate) fn new(
        inner: Box<dyn ControllerTransport>,
        journal: JournalWriter,
        binding: AmiiboEvidenceBinding,
        recorder: AmiiboEvidenceRecorder,
    ) -> Self {
        Self {
            inner,
            journal,
            binding,
            recorder,
            state: EvidenceState::Ready,
            next_offset: 0,
            attempts: BTreeMap::new(),
            save_operation_id: None,
        }
    }

    fn begin_write(&mut self, request: &WriteRequest<'_>) -> Result<ActiveWrite, TransportError> {
        if request.context.kind != WriteKind::Command {
            return Err(protocol_error(
                "Amiibo evidence only instruments command writes",
            ));
        }
        let (exchange, phase) = match self.state.clone() {
            EvidenceState::HeaderAcked(key) => {
                let operation_id = request
                    .context
                    .operation_id
                    .ok_or_else(|| protocol_error("Amiibo payload has no operation ID"))?;
                if operation_id != key.operation_id
                    || request.context.total_len != key.length
                    || request.bytes.len() != key.length
                {
                    return Err(protocol_error(
                        "Amiibo payload does not match the acknowledged chunk header",
                    ));
                }
                let trace_index = self.chunk_trace_index(key)?;
                (Exchange::Chunk { trace_index, key }, WritePhase::Payload)
            }
            EvidenceState::Ready => {
                let operation_id = request
                    .context
                    .operation_id
                    .ok_or_else(|| protocol_error("Amiibo save header has no operation ID"))?;
                let (slot, offset, length) = parse_save_header(request.bytes)?;
                if slot != self.binding.slot || offset != self.next_offset {
                    return Err(protocol_error(
                        "Amiibo save header is not bound to the authorized slot and offset",
                    ));
                }
                let remaining = self
                    .binding
                    .payload_len
                    .checked_sub(offset)
                    .ok_or_else(|| protocol_error("Amiibo chunk offset exceeds payload"))?;
                if length != remaining.min(MAX_CHUNK_LEN) {
                    return Err(protocol_error(
                        "Amiibo chunk length does not match the authorized payload",
                    ));
                }
                match self.save_operation_id {
                    Some(expected) if expected != operation_id => {
                        return Err(protocol_error(
                            "Amiibo save operation changed before completion",
                        ));
                    }
                    None => self.save_operation_id = Some(operation_id),
                    Some(_) => {}
                }
                let attempt = self
                    .attempts
                    .entry(offset)
                    .and_modify(|attempt| *attempt = attempt.saturating_add(1))
                    .or_insert(1);
                let key = ChunkKey {
                    operation_id,
                    offset,
                    length,
                    attempt: *attempt,
                };
                let intent = self.chunk_event_payload(key, request.context.sequence, "intent");
                self.append_required(JournalEventKind::AmiiboChunkIntent, intent.clone())?;
                let trace_index = self.recorder.begin_chunk(json!({
                    "operation_id": operation_id.get(),
                    "offset": offset,
                    "length": length,
                    "attempt": key.attempt,
                    "header_write_sequence": request.context.sequence,
                    "header_accepted_bytes": 0,
                    "header_acknowledged": false,
                    "payload_write_sequence": null,
                    "payload_accepted_bytes": 0,
                    "payload_acknowledged": false,
                    "status": "intent_durable",
                    "error": null,
                }))?;
                (Exchange::Chunk { trace_index, key }, WritePhase::Header)
            }
            EvidenceState::SaveComplete => {
                let operation_id = request
                    .context
                    .operation_id
                    .ok_or_else(|| protocol_error("Amiibo select has no operation ID"))?;
                if request.bytes != [READY, self.binding.slot, SELECT] {
                    return Err(protocol_error(
                        "only the authorized Amiibo select command may follow save",
                    ));
                }
                let intent = self.base_payload(json!({
                    "layer": "transport",
                    "operation_id": operation_id.get(),
                    "slot": self.binding.slot,
                    "write_sequence": request.context.sequence,
                }));
                self.append_required(JournalEventKind::AmiiboSelectIntent, intent.clone())?;
                let trace_index = self.recorder.begin_select(json!({
                    "operation_id": operation_id.get(),
                    "slot": self.binding.slot,
                    "write_sequence": request.context.sequence,
                    "accepted_bytes": 0,
                    "acknowledged": false,
                    "status": "intent_durable",
                    "error": null,
                }))?;
                (
                    Exchange::Select {
                        trace_index,
                        operation_id,
                    },
                    WritePhase::Select,
                )
            }
            EvidenceState::Recovering(resume, operation_id) => {
                if request.context.operation_id.is_some() || request.bytes != RESET {
                    return Err(protocol_error(
                        "Amiibo recovery expected an unbound source-exact reset write",
                    ));
                }
                let intent = self.base_payload(json!({
                    "operation_id": operation_id.get(),
                    "write_sequence": request.context.sequence,
                    "resume": resume.as_str(),
                }));
                self.append_cleanup(JournalEventKind::AmiiboCleanupIntent, intent.clone());
                let trace_index = self.recorder.begin_cleanup(json!({
                    "operation_id": operation_id.get(),
                    "write_sequence": request.context.sequence,
                    "accepted_bytes": 0,
                    "acknowledged": false,
                    "resume": resume.as_str(),
                    "status": "intent_attempted",
                    "error": null,
                }))?;
                (
                    Exchange::Cleanup {
                        trace_index,
                        operation_id,
                        resume,
                    },
                    WritePhase::CleanupReset,
                )
            }
            _ => {
                return Err(protocol_error(
                    "Amiibo command write arrived in an invalid evidence state",
                ));
            }
        };
        Ok(ActiveWrite {
            context: request.context,
            accepted: 0,
            exchange,
            phase,
        })
    }

    fn chunk_trace_index(&self, key: ChunkKey) -> Result<usize, TransportError> {
        let trace = lock_trace(&self.recorder.trace)?;
        trace
            .chunks
            .iter()
            .position(|chunk| {
                chunk["operation_id"].as_u64() == Some(key.operation_id.get())
                    && chunk["offset"].as_u64() == u64::try_from(key.offset).ok()
                    && chunk["attempt"].as_u64() == Some(u64::from(key.attempt))
            })
            .ok_or_else(|| protocol_error("Amiibo chunk trace was not found"))
    }

    fn append_required(
        &self,
        kind: JournalEventKind,
        payload: Value,
    ) -> Result<(), TransportError> {
        self.journal
            .append(kind, payload)
            .map_err(|error| io_error(format!("cannot persist Amiibo evidence: {error}")))
    }

    fn append_cleanup(&self, kind: JournalEventKind, payload: Value) {
        if let Err(error) = self.journal.append(kind, payload) {
            self.recorder
                .evidence_error(format!("cannot persist Amiibo cleanup evidence: {error}"));
        }
    }

    fn base_payload(&self, detail: Value) -> Value {
        json!({
            "lease_id": self.binding.lease_id,
            "expected_stable_id": self.binding.expected_stable_id,
            "observed_stable_id": self.binding.observed_stable_id,
            "authorized_slot": self.binding.slot,
            "payload_length": self.binding.payload_len,
            "payload_sha256": self.binding.payload_sha256,
            "detail": detail,
        })
    }

    fn chunk_event_payload(&self, key: ChunkKey, write_sequence: u64, status: &str) -> Value {
        self.base_payload(json!({
            "operation_id": key.operation_id.get(),
            "offset": key.offset,
            "length": key.length,
            "attempt": key.attempt,
            "write_sequence": write_sequence,
            "status": status,
        }))
    }

    fn progress_kind(phase: WritePhase) -> JournalEventKind {
        match phase {
            WritePhase::Header | WritePhase::Payload => JournalEventKind::AmiiboChunkProgress,
            WritePhase::Select => JournalEventKind::AmiiboSelectProgress,
            WritePhase::CleanupReset => JournalEventKind::AmiiboCleanupProgress,
        }
    }

    fn terminal_kind(exchange: &Exchange) -> JournalEventKind {
        match exchange {
            Exchange::Chunk { .. } => JournalEventKind::AmiiboChunkTerminal,
            Exchange::Select { .. } => JournalEventKind::AmiiboSelectTerminal,
            Exchange::Cleanup { .. } => JournalEventKind::AmiiboCleanupTerminal,
        }
    }

    fn record_write_progress(
        &self,
        active: &ActiveWrite,
        accepted_now: usize,
        accepted_total: usize,
    ) -> Result<(), TransportError> {
        let field = match active.phase {
            WritePhase::Header => "header_accepted_bytes",
            WritePhase::Payload => "payload_accepted_bytes",
            WritePhase::Select | WritePhase::CleanupReset => "accepted_bytes",
        };
        self.recorder
            .update(&active.exchange, field, json!(accepted_total))?;
        if matches!(active.phase, WritePhase::Payload) {
            self.recorder.update(
                &active.exchange,
                "payload_write_sequence",
                json!(active.context.sequence),
            )?;
        }
        let payload = self.base_payload(json!({
            "operation_id": active.exchange.operation_id().get(),
            "phase": active.phase.as_str(),
            "write_sequence": active.context.sequence,
            "accepted_this_call": accepted_now,
            "accepted_total": accepted_total,
            "logical_length": active.context.total_len,
        }));
        match active.phase {
            WritePhase::CleanupReset => {
                self.append_cleanup(Self::progress_kind(active.phase), payload);
                Ok(())
            }
            _ => self.append_required(Self::progress_kind(active.phase), payload),
        }
    }

    fn record_terminal(
        &self,
        exchange: &Exchange,
        status: &str,
        error: Option<&TransportError>,
    ) -> Result<(), TransportError> {
        self.recorder.update(exchange, "status", json!(status))?;
        self.recorder.update(
            exchange,
            "error",
            error.map_or(Value::Null, transport_error_json),
        )?;
        let payload = self.base_payload(json!({
            "operation_id": exchange.operation_id().get(),
            "status": status,
            "error": error.map(transport_error_json),
        }));
        if matches!(exchange, Exchange::Cleanup { .. }) {
            self.append_cleanup(Self::terminal_kind(exchange), payload);
            Ok(())
        } else {
            self.append_required(Self::terminal_kind(exchange), payload)
        }
    }

    fn record_late_ack(
        &self,
        exchange: &Exchange,
        observed_generation: u64,
        expected_generation: u64,
    ) -> Result<(), TransportError> {
        let payload = self.base_payload(json!({
            "operation_id": exchange.operation_id().get(),
            "phase": "ack_late_ignored",
            "observed_generation": observed_generation,
            "expected_generation": expected_generation,
        }));
        match exchange {
            Exchange::Chunk { .. } => {
                self.append_required(JournalEventKind::AmiiboChunkProgress, payload)
            }
            Exchange::Select { .. } => {
                self.append_required(JournalEventKind::AmiiboSelectProgress, payload)
            }
            Exchange::Cleanup { .. } => {
                self.append_cleanup(JournalEventKind::AmiiboCleanupProgress, payload);
                Ok(())
            }
        }
    }

    fn fail_exchange(
        &mut self,
        exchange: &Exchange,
        error: &TransportError,
    ) -> Result<(), TransportError> {
        let journal_result = self.record_terminal(exchange, "failed", Some(error));
        self.state = match exchange {
            Exchange::Chunk { .. } => {
                EvidenceState::Recovering(ResumeState::Save, exchange.operation_id())
            }
            Exchange::Select { .. } => {
                EvidenceState::Recovering(ResumeState::Select, exchange.operation_id())
            }
            Exchange::Cleanup { .. } => EvidenceState::Failed,
        };
        journal_result
    }

    fn complete_ack(&mut self, exchange: Exchange) -> Result<(), TransportError> {
        match exchange {
            Exchange::Chunk { trace_index, key } => {
                let payload_accepted = {
                    let trace = lock_trace(&self.recorder.trace)?;
                    trace.chunks[trace_index]["payload_accepted_bytes"]
                        .as_u64()
                        .unwrap_or_default()
                };
                if payload_accepted == 0 {
                    self.recorder.update(
                        &Exchange::Chunk { trace_index, key },
                        "header_acknowledged",
                        json!(true),
                    )?;
                    self.append_required(
                        JournalEventKind::AmiiboChunkProgress,
                        self.base_payload(json!({
                            "operation_id": key.operation_id.get(),
                            "offset": key.offset,
                            "length": key.length,
                            "attempt": key.attempt,
                            "phase": "header_ack",
                            "status": "acked",
                        })),
                    )?;
                    self.state = EvidenceState::HeaderAcked(key);
                } else {
                    self.recorder.update(
                        &Exchange::Chunk { trace_index, key },
                        "payload_acknowledged",
                        json!(true),
                    )?;
                    self.record_terminal(&Exchange::Chunk { trace_index, key }, "acked", None)?;
                    self.next_offset = self
                        .next_offset
                        .checked_add(key.length)
                        .ok_or_else(|| protocol_error("Amiibo chunk offset overflowed"))?;
                    self.state = if self.next_offset == self.binding.payload_len {
                        EvidenceState::SaveComplete
                    } else {
                        EvidenceState::Ready
                    };
                }
            }
            Exchange::Select {
                trace_index,
                operation_id,
            } => {
                let exchange = Exchange::Select {
                    trace_index,
                    operation_id,
                };
                self.recorder
                    .update(&exchange, "acknowledged", json!(true))?;
                self.record_terminal(&exchange, "acked", None)?;
                self.state = EvidenceState::Complete;
            }
            Exchange::Cleanup {
                trace_index,
                operation_id,
                resume,
            } => {
                let exchange = Exchange::Cleanup {
                    trace_index,
                    operation_id,
                    resume,
                };
                self.recorder
                    .update(&exchange, "acknowledged", json!(true))?;
                self.record_terminal(&exchange, "acked", None)?;
                self.state = match resume {
                    ResumeState::Save => EvidenceState::Ready,
                    ResumeState::Select => EvidenceState::SaveComplete,
                };
            }
        }
        Ok(())
    }
}

impl ControllerTransport for AmiiboEvidenceTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        self.inner.handshake(request)
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        if request.context.kind != WriteKind::Command {
            return self.inner.write(request);
        }
        let active = match self.state.clone() {
            EvidenceState::Writing(active) => {
                let offset = active
                    .context
                    .total_len
                    .checked_sub(request.bytes.len())
                    .ok_or_else(|| protocol_error("Amiibo write remainder exceeds total length"))?;
                if request.context != active.context || offset != active.accepted {
                    return Err(protocol_error("Amiibo partial-write continuity was lost"));
                }
                active
            }
            _ => {
                let offset = request
                    .context
                    .total_len
                    .checked_sub(request.bytes.len())
                    .ok_or_else(|| protocol_error("Amiibo write remainder exceeds total length"))?;
                if offset != 0 {
                    return Err(protocol_error("Amiibo write resumed without an intent"));
                }
                self.begin_write(&request)?
            }
        };

        let request_len = request.bytes.len();
        let settlement = request.settlement.clone();
        let result = self.inner.write(request);
        match result {
            Ok(accepted) if accepted != 0 && accepted <= request_len => {
                let accepted_total = active
                    .accepted
                    .checked_add(accepted)
                    .ok_or_else(|| protocol_error("Amiibo accepted-byte count overflowed"))?;
                if accepted_total == active.context.total_len && !settlement.is_full_accepted() {
                    let error = io_error(
                        "Amiibo evidence transport returned a full payload without backend settlement",
                    );
                    self.fail_exchange(&active.exchange, &error)?;
                    return Err(error);
                }
                if let Err(error) = self.record_write_progress(&active, accepted, accepted_total) {
                    self.state = match active.exchange {
                        Exchange::Chunk { .. } => EvidenceState::Recovering(
                            ResumeState::Save,
                            active.exchange.operation_id(),
                        ),
                        Exchange::Select { .. } => EvidenceState::Recovering(
                            ResumeState::Select,
                            active.exchange.operation_id(),
                        ),
                        Exchange::Cleanup { .. } => EvidenceState::Failed,
                    };
                    return Err(error);
                }
                self.state = if accepted_total == active.context.total_len {
                    EvidenceState::AwaitingAck(active.exchange)
                } else {
                    EvidenceState::Writing(ActiveWrite {
                        accepted: accepted_total,
                        ..active
                    })
                };
                Ok(accepted)
            }
            Ok(_) => {
                let error = protocol_error("Amiibo transport returned invalid write progress");
                self.fail_exchange(&active.exchange, &error)?;
                Err(error)
            }
            Err(error) => {
                self.fail_exchange(&active.exchange, &error)?;
                Err(error)
            }
        }
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        let EvidenceState::AwaitingAck(exchange) = self.state.clone() else {
            return Err(protocol_error(
                "Amiibo ACK arrived without a complete write",
            ));
        };
        if request.operation_id != exchange.operation_id() {
            return Err(protocol_error("Amiibo ACK operation binding changed"));
        }
        let generation = request.generation;
        let expected_reply = request.expected_reply;
        let result = self.inner.wait_for_ack(request);
        match result {
            Ok(frame) if frame.generation == generation && frame.byte == expected_reply => {
                if let Err(error) = self.complete_ack(exchange.clone()) {
                    self.state = match exchange {
                        Exchange::Chunk { .. } => {
                            EvidenceState::Recovering(ResumeState::Save, exchange.operation_id())
                        }
                        Exchange::Select { .. } => {
                            EvidenceState::Recovering(ResumeState::Select, exchange.operation_id())
                        }
                        Exchange::Cleanup { .. } => EvidenceState::Failed,
                    };
                    return Err(error);
                }
                Ok(frame)
            }
            Ok(frame) if frame.generation < generation => {
                self.record_late_ack(&exchange, frame.generation, generation)?;
                Ok(frame)
            }
            Ok(frame) => {
                let error = protocol_error("Amiibo ACK generation or byte did not match");
                self.fail_exchange(&exchange, &error)?;
                Ok(frame)
            }
            Err(error) => {
                self.fail_exchange(&exchange, &error)?;
                Err(error)
            }
        }
    }

    fn close(&mut self) {
        self.recorder.record_close(&self.state);
        self.inner.close();
    }

    fn close_checked(&mut self) -> Result<(), TransportError> {
        self.recorder.record_close(&self.state);
        self.inner.close_checked()
    }
}

impl ResumeState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Save => "save",
            Self::Select => "select",
        }
    }
}

fn parse_save_header(bytes: &[u8]) -> Result<(u8, usize, usize), TransportError> {
    if bytes.len() != 7
        || bytes[0] != READY
        || bytes[6] != SAVE
        || bytes[1..=4].iter().any(|byte| byte & 0x80 != 0)
    {
        return Err(protocol_error(
            "command is not a source-exact Amiibo save header",
        ));
    }
    let offset = usize::from(bytes[1]) | (usize::from(bytes[2]) << 7);
    let length = usize::from(bytes[3]) | (usize::from(bytes[4]) << 7);
    if !(1..=MAX_CHUNK_LEN).contains(&length) {
        return Err(protocol_error("Amiibo save header length is invalid"));
    }
    Ok((bytes[5], offset, length))
}

fn transport_error_json(error: &TransportError) -> Value {
    json!({
        "kind": format!("{:?}", error.kind()),
        "message": error.message(),
    })
}

fn protocol_error(message: impl Into<Arc<str>>) -> TransportError {
    TransportError::new(TransportErrorKind::Protocol, message)
}

fn io_error(message: impl Into<Arc<str>>) -> TransportError {
    TransportError::new(TransportErrorKind::Io, message)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use easycon_model::{OperationId, ResourceId};
    use easycon_runtime::CancellationToken;

    use super::*;
    use crate::journal::{EvidenceJournal, JOURNAL_FILE_NAME, JournalStart, parse_journal};

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "easycon-amiibo-evidence-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct ScriptedTransport {
        writes: VecDeque<Result<usize, TransportErrorKind>>,
        acks: VecDeque<Result<(), TransportErrorKind>>,
    }

    /// Returns a complete write without publishing its backend acceptance gate.
    ///
    /// This models a faulty decorator/adapter boundary: a full byte count alone must not become
    /// Amiibo evidence before the shared `WriteSettlement` has accepted the physical final byte.
    struct FullReturnWithoutSettlementTransport;

    fn accept_scripted_write(
        request: &WriteRequest<'_>,
        accepted: usize,
    ) -> Result<usize, TransportError> {
        let completes_payload = request
            .context
            .total_len
            .checked_sub(request.bytes.len())
            .and_then(|offset| offset.checked_add(accepted))
            == Some(request.context.total_len);
        if completes_payload
            && !request
                .settlement
                .full_accepted_at(request.context.timestamp_ns)
        {
            return Err(TransportError::new(
                TransportErrorKind::Io,
                "scripted Amiibo transport rejected final-byte settlement",
            ));
        }
        Ok(accepted)
    }

    impl ControllerTransport for ScriptedTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            match self.writes.pop_front().expect("scripted write") {
                Ok(usize::MAX) => accept_scripted_write(&request, request.bytes.len()),
                Ok(accepted) => accept_scripted_write(&request, accepted),
                Err(kind) => Err(TransportError::new(kind, "injected write failure")),
            }
        }

        fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
            match self.acks.pop_front().expect("scripted ACK") {
                Ok(()) => Ok(AckFrame {
                    generation: request.generation,
                    byte: request.expected_reply,
                }),
                Err(kind) => Err(TransportError::new(kind, "injected ACK failure")),
            }
        }

        fn close(&mut self) {}
    }

    impl ControllerTransport for FullReturnWithoutSettlementTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            Ok(request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(io_error("unsettled full-return transport cannot acknowledge a command"))
        }

        fn close(&mut self) {}
    }

    struct CheckedCloseFailureTransport {
        close_calls: Arc<AtomicUsize>,
    }

    impl ControllerTransport for CheckedCloseFailureTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            accept_scripted_write(&request, request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Err(TransportError::new(
                TransportErrorKind::Protocol,
                "no ACK in checked-close delegation test",
            ))
        }

        fn close(&mut self) {
            self.close_calls.fetch_add(1, Ordering::AcqRel);
        }

        fn close_checked(&mut self) -> Result<(), TransportError> {
            self.close();
            Err(TransportError::new(
                TransportErrorKind::Io,
                "injected Amiibo typed checked-close failure",
            ))
        }
    }

    struct LateAckTransport {
        frames: VecDeque<AckFrame>,
    }

    impl ControllerTransport for LateAckTransport {
        fn handshake(&mut self, _request: HandshakeRequest) -> Result<(), TransportError> {
            Ok(())
        }

        fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
            accept_scripted_write(&request, request.bytes.len())
        }

        fn wait_for_ack(&mut self, _request: AckRequest) -> Result<AckFrame, TransportError> {
            Ok(self.frames.pop_front().expect("scripted frame"))
        }

        fn close(&mut self) {}
    }

    fn journal(label: &str, directory: &TestDirectory) -> EvidenceJournal {
        EvidenceJournal::create(
            directory.0.join(JOURNAL_FILE_NAME),
            JournalStart {
                lease_id: format!("lease-{label}"),
                command: "amiibo".to_owned(),
                process_id: 7,
                started_unix_ns: 11,
                normalized_arguments: json!(["amiibo"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("journal")
    }

    fn instrumented(
        label: &str,
        directory: &TestDirectory,
        writes: impl IntoIterator<Item = Result<usize, TransportErrorKind>>,
        acks: impl IntoIterator<Item = Result<(), TransportErrorKind>>,
        payload_len: usize,
    ) -> (
        AmiiboEvidenceTransport,
        AmiiboEvidenceRecorder,
        EvidenceJournal,
    ) {
        let journal = journal(label, directory);
        let recorder = AmiiboEvidenceRecorder::default();
        let transport = AmiiboEvidenceTransport::new(
            Box::new(ScriptedTransport {
                writes: writes.into_iter().collect(),
                acks: acks.into_iter().collect(),
            }),
            journal.writer(),
            AmiiboEvidenceBinding {
                lease_id: format!("lease-{label}"),
                expected_stable_id: "DEVICE\\EXPECTED".to_owned(),
                observed_stable_id: "DEVICE\\EXPECTED".to_owned(),
                slot: 3,
                payload_len,
                payload_sha256: "A".repeat(64),
            },
            recorder.clone(),
        );
        (transport, recorder, journal)
    }

    #[test]
    fn typed_checked_close_failure_is_delegated_once_with_close_evidence() {
        let directory = TestDirectory::new("typed-close");
        let journal = journal("typed-close", &directory);
        let recorder = AmiiboEvidenceRecorder::default();
        let close_calls = Arc::new(AtomicUsize::new(0));
        let mut transport = AmiiboEvidenceTransport::new(
            Box::new(CheckedCloseFailureTransport {
                close_calls: close_calls.clone(),
            }),
            journal.writer(),
            AmiiboEvidenceBinding {
                lease_id: "lease-typed-close".to_owned(),
                expected_stable_id: "DEVICE\\EXPECTED".to_owned(),
                observed_stable_id: "DEVICE\\EXPECTED".to_owned(),
                slot: 3,
                payload_len: 3,
                payload_sha256: "A".repeat(64),
            },
            recorder.clone(),
        );

        let error = transport
            .close_checked()
            .expect_err("typed inner checked-close failure must be preserved");

        assert_eq!(error.kind(), TransportErrorKind::Io);
        assert_eq!(error.message(), "injected Amiibo typed checked-close failure");
        assert_eq!(close_calls.load(Ordering::Acquire), 1);
        assert_eq!(recorder.projection()["transport_closed_in_state"], "ready");
    }

    #[test]
    fn full_return_without_the_shared_settlement_never_records_amiibo_acceptance() {
        let directory = TestDirectory::new("unsettled-full-return");
        let journal = journal("unsettled-full-return", &directory);
        let recorder = AmiiboEvidenceRecorder::default();
        let mut transport = AmiiboEvidenceTransport::new(
            Box::new(FullReturnWithoutSettlementTransport),
            journal.writer(),
            AmiiboEvidenceBinding {
                lease_id: "lease-unsettled-full-return".to_owned(),
                expected_stable_id: "DEVICE\\EXPECTED".to_owned(),
                observed_stable_id: "DEVICE\\EXPECTED".to_owned(),
                slot: 3,
                payload_len: 3,
                payload_sha256: "A".repeat(64),
            },
            recorder.clone(),
        );
        let bytes = header(0, 3);
        let error = transport
            .write(WriteRequest {
                context: context(1, Some(OperationId::new(7)), bytes.len()),
                bytes: &bytes,
                deadline_ns: u64::MAX,
                cancellation: CancellationToken::root(),
                resource_cancellation: CancellationToken::root(),
                settlement: WriteSettlement::untracked(),
            })
            .expect_err("a full return without backend acceptance must be rejected");

        assert_eq!(error.kind(), TransportErrorKind::Io);
        let projection = recorder.projection();
        assert_eq!(projection["chunks"][0]["header_accepted_bytes"], 0);
        assert_eq!(projection["chunks"][0]["status"], "failed");
    }

    fn context(sequence: u64, operation_id: Option<OperationId>, total_len: usize) -> WriteContext {
        WriteContext {
            resource_id: ResourceId::new(1),
            operation_id,
            sequence,
            timestamp_ns: sequence,
            direct_timing: None,
            total_len,
            kind: WriteKind::Command,
        }
    }

    fn drive_write(
        transport: &mut AmiiboEvidenceTransport,
        operation_id: OperationId,
        sequence: u64,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        drive_write_with_context(transport, Some(operation_id), sequence, bytes)
    }

    fn drive_cleanup_write(
        transport: &mut AmiiboEvidenceTransport,
        sequence: u64,
    ) -> Result<(), TransportError> {
        drive_write_with_context(transport, None, sequence, &RESET)
    }

    fn drive_write_with_context(
        transport: &mut AmiiboEvidenceTransport,
        operation_id: Option<OperationId>,
        sequence: u64,
        bytes: &[u8],
    ) -> Result<(), TransportError> {
        let write_context = context(sequence, operation_id, bytes.len());
        let settlement = WriteSettlement::untracked();
        let mut accepted = 0;
        while accepted < bytes.len() {
            let count = transport.write(WriteRequest {
                context: write_context,
                bytes: &bytes[accepted..],
                deadline_ns: u64::MAX,
                cancellation: CancellationToken::root(),
                resource_cancellation: CancellationToken::root(),
                settlement: settlement.clone(),
            })?;
            accepted += count;
        }
        Ok(())
    }

    fn drive_ack(
        transport: &mut AmiiboEvidenceTransport,
        operation_id: OperationId,
        generation: u64,
        expected_reply: u8,
    ) -> Result<(), TransportError> {
        transport
            .wait_for_ack(AckRequest {
                operation_id,
                generation,
                expected_reply,
                deadline_ns: u64::MAX,
                cancellation: CancellationToken::root(),
                resource_cancellation: CancellationToken::root(),
            })
            .map(|_| ())
    }

    fn header(offset: usize, length: usize) -> [u8; 7] {
        [
            READY,
            (offset & 0x7f) as u8,
            (offset >> 7) as u8,
            (length & 0x7f) as u8,
            (length >> 7) as u8,
            3,
            SAVE,
        ]
    }

    fn journal_events(journal: &EvidenceJournal) -> Vec<Value> {
        parse_journal(&journal.readback().expect("journal bytes")).expect("journal events")
    }

    #[test]
    fn multi_chunk_partial_progress_is_durable_and_command_like_payload_is_not_reparsed() {
        let directory = TestDirectory::new("multi-chunk");
        let (mut transport, recorder, journal) = instrumented(
            "multi-chunk",
            &directory,
            [
                Ok(3),
                Ok(4),
                Ok(5),
                Ok(15),
                Ok(usize::MAX),
                Ok(usize::MAX),
                Ok(usize::MAX),
            ],
            [Ok(()), Ok(()), Ok(()), Ok(()), Ok(())],
            23,
        );
        let save = OperationId::new(10);
        let select = OperationId::new(11);
        let mut first_payload = [0x44_u8; 20];
        first_payload[..3].copy_from_slice(&[READY, 3, SELECT]);

        drive_write(&mut transport, save, 1, &header(0, 20)).expect("header 1");
        drive_ack(&mut transport, save, 1, 0xff).expect("header ACK 1");
        drive_write(&mut transport, save, 2, &first_payload).expect("payload 1");
        drive_ack(&mut transport, save, 2, 0xff).expect("payload ACK 1");
        drive_write(&mut transport, save, 3, &header(20, 3)).expect("header 2");
        drive_ack(&mut transport, save, 3, 0xff).expect("header ACK 2");
        drive_write(&mut transport, save, 4, &[1, 2, 3]).expect("payload 2");
        drive_ack(&mut transport, save, 4, 0xff).expect("payload ACK 2");
        drive_write(&mut transport, select, 5, &[READY, 3, SELECT]).expect("select");
        drive_ack(&mut transport, select, 5, 0xff).expect("select ACK");
        transport.close();

        let projection = recorder.projection();
        assert_eq!(projection["chunks"].as_array().map(Vec::len), Some(2));
        assert_eq!(projection["chunks"][0]["header_accepted_bytes"], 7);
        assert_eq!(projection["chunks"][0]["payload_accepted_bytes"], 20);
        assert_eq!(projection["chunks"][0]["status"], "acked");
        assert_eq!(projection["chunks"][1]["offset"], 20);
        assert_eq!(projection["chunks"][1]["payload_accepted_bytes"], 3);
        assert_eq!(projection["selects"][0]["status"], "acked");
        assert_eq!(projection["transport_closed_in_state"], "complete");

        let events = journal_events(&journal);
        let names = events
            .iter()
            .map(|event| event["event"].as_str().expect("event"))
            .collect::<Vec<_>>();
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "amiibo_chunk_intent")
                .count(),
            2
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "amiibo_chunk_terminal")
                .count(),
            2
        );
        assert_eq!(
            names
                .iter()
                .filter(|name| **name == "amiibo_select_terminal")
                .count(),
            1
        );
        for intent in names
            .iter()
            .enumerate()
            .filter_map(|(index, name)| (*name == "amiibo_chunk_intent").then_some(index))
        {
            assert!(names[intent + 1..].contains(&"amiibo_chunk_progress"));
        }
    }

    #[test]
    fn late_ack_generation_is_journaled_and_does_not_end_the_exchange() {
        let directory = TestDirectory::new("late-ack");
        let journal = journal("late-ack", &directory);
        let recorder = AmiiboEvidenceRecorder::default();
        let mut transport = AmiiboEvidenceTransport::new(
            Box::new(LateAckTransport {
                frames: VecDeque::from([
                    AckFrame {
                        generation: 1,
                        byte: 0xff,
                    },
                    AckFrame {
                        generation: 2,
                        byte: 0xff,
                    },
                    AckFrame {
                        generation: 3,
                        byte: 0xff,
                    },
                ]),
            }),
            journal.writer(),
            AmiiboEvidenceBinding {
                lease_id: "lease-late-ack".to_owned(),
                expected_stable_id: "DEVICE\\EXPECTED".to_owned(),
                observed_stable_id: "DEVICE\\EXPECTED".to_owned(),
                slot: 3,
                payload_len: 3,
                payload_sha256: "A".repeat(64),
            },
            recorder.clone(),
        );
        let operation = OperationId::new(40);

        drive_write(&mut transport, operation, 1, &header(0, 3)).expect("header");
        drive_ack(&mut transport, operation, 2, 0xff).expect("late ACK is returned");
        drive_ack(&mut transport, operation, 2, 0xff).expect("matching header ACK");
        drive_write(&mut transport, operation, 2, &[1, 2, 3]).expect("payload");
        drive_ack(&mut transport, operation, 3, 0xff).expect("payload ACK");

        assert_eq!(recorder.projection()["chunks"][0]["status"], "acked");
        let events = journal_events(&journal);
        assert!(events.iter().any(|event| {
            event["event"] == "amiibo_chunk_progress"
                && event["payload"]["detail"]["phase"] == "ack_late_ignored"
                && event["payload"]["detail"]["observed_generation"] == 1
                && event["payload"]["detail"]["expected_generation"] == 2
        }));
    }

    #[derive(Clone, Copy, Debug)]
    enum ChunkFailPoint {
        HeaderWrite,
        HeaderAck,
        PayloadWrite,
        PayloadAck,
        CleanupWrite,
        CleanupAck,
    }

    #[test]
    fn chunk_and_cleanup_failpoints_retain_exact_known_progress() {
        for point in [
            ChunkFailPoint::HeaderWrite,
            ChunkFailPoint::HeaderAck,
            ChunkFailPoint::PayloadWrite,
            ChunkFailPoint::PayloadAck,
            ChunkFailPoint::CleanupWrite,
            ChunkFailPoint::CleanupAck,
        ] {
            let directory = TestDirectory::new(&format!("chunk-fail-{point:?}"));
            let (writes, acks) = match point {
                ChunkFailPoint::HeaderWrite => (
                    vec![Err(TransportErrorKind::Io), Ok(usize::MAX)],
                    vec![Ok(())],
                ),
                ChunkFailPoint::HeaderAck => (
                    vec![Ok(usize::MAX), Ok(usize::MAX)],
                    vec![Err(TransportErrorKind::Timeout), Ok(())],
                ),
                ChunkFailPoint::PayloadWrite => (
                    vec![
                        Ok(usize::MAX),
                        Ok(3),
                        Err(TransportErrorKind::Io),
                        Ok(usize::MAX),
                    ],
                    vec![Ok(()), Ok(())],
                ),
                ChunkFailPoint::PayloadAck => (
                    vec![Ok(usize::MAX), Ok(usize::MAX), Ok(usize::MAX)],
                    vec![Ok(()), Err(TransportErrorKind::Timeout), Ok(())],
                ),
                ChunkFailPoint::CleanupWrite => (
                    vec![Ok(usize::MAX), Err(TransportErrorKind::Io)],
                    vec![Err(TransportErrorKind::Timeout)],
                ),
                ChunkFailPoint::CleanupAck => (
                    vec![Ok(usize::MAX), Ok(usize::MAX)],
                    vec![
                        Err(TransportErrorKind::Timeout),
                        Err(TransportErrorKind::Timeout),
                    ],
                ),
            };
            let (mut transport, recorder, journal) =
                instrumented("chunk-failure", &directory, writes, acks, 20);
            let operation = OperationId::new(20);

            let header_result = drive_write(&mut transport, operation, 1, &header(0, 20));
            let header_ack_result = if header_result.is_ok() {
                drive_ack(&mut transport, operation, 1, 0xff)
            } else {
                Err(protocol_error("header write failed"))
            };
            let payload_result = if header_ack_result.is_ok() {
                drive_write(&mut transport, operation, 2, &[0x55; 20])
            } else {
                Err(protocol_error("header ACK failed"))
            };
            let payload_ack_result = if payload_result.is_ok() {
                drive_ack(&mut transport, operation, 2, 0xff)
            } else {
                Err(protocol_error("payload write failed"))
            };
            assert!(
                header_result.is_err()
                    || header_ack_result.is_err()
                    || payload_result.is_err()
                    || payload_ack_result.is_err(),
                "{point:?} did not fail"
            );

            let reset_write = drive_cleanup_write(&mut transport, 3);
            let reset_ack = if reset_write.is_ok() {
                drive_ack(&mut transport, operation, 3, 0x80)
            } else {
                Err(protocol_error("reset write failed"))
            };
            if matches!(
                point,
                ChunkFailPoint::CleanupWrite | ChunkFailPoint::CleanupAck
            ) {
                assert!(reset_write.is_err() || reset_ack.is_err(), "{point:?}");
            } else {
                assert!(reset_write.is_ok() && reset_ack.is_ok(), "{point:?}");
            }

            let projection = recorder.projection();
            assert_eq!(projection["chunks"].as_array().map(Vec::len), Some(1));
            assert_eq!(projection["chunks"][0]["status"], "failed");
            assert_eq!(
                projection["chunks"][0]["payload_accepted_bytes"],
                if matches!(point, ChunkFailPoint::PayloadWrite) {
                    json!(3)
                } else if matches!(point, ChunkFailPoint::PayloadAck) {
                    json!(20)
                } else {
                    json!(0)
                },
                "{point:?}"
            );
            assert_eq!(
                projection["cleanup_resets"][0]["status"],
                if matches!(
                    point,
                    ChunkFailPoint::CleanupWrite | ChunkFailPoint::CleanupAck
                ) {
                    "failed"
                } else {
                    "acked"
                },
                "{point:?}"
            );
            let names = journal_events(&journal)
                .into_iter()
                .map(|event| event["event"].as_str().expect("event").to_owned())
                .collect::<Vec<_>>();
            assert!(
                names.contains(&"amiibo_chunk_intent".to_owned()),
                "{point:?}"
            );
            assert!(
                names.contains(&"amiibo_chunk_terminal".to_owned()),
                "{point:?}"
            );
            assert!(
                names.contains(&"amiibo_cleanup_intent".to_owned()),
                "{point:?}"
            );
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum SelectFailPoint {
        Write,
        Ack,
    }

    #[test]
    fn retry_and_select_failpoints_preserve_save_and_cleanup_prefixes() {
        for point in [SelectFailPoint::Write, SelectFailPoint::Ack] {
            let directory = TestDirectory::new(&format!("select-fail-{point:?}"));
            let mut writes = vec![
                Ok(usize::MAX),
                Ok(usize::MAX),
                Ok(usize::MAX),
                Ok(usize::MAX),
            ];
            let mut acks = vec![Err(TransportErrorKind::Timeout), Ok(()), Ok(()), Ok(())];
            match point {
                SelectFailPoint::Write => {
                    writes.extend([Err(TransportErrorKind::Io), Ok(usize::MAX)])
                }
                SelectFailPoint::Ack => {
                    writes.extend([Ok(usize::MAX), Ok(usize::MAX)]);
                    acks.push(Err(TransportErrorKind::Timeout));
                }
            }
            acks.push(Ok(()));
            let (mut transport, recorder, _) =
                instrumented("select-failure", &directory, writes, acks, 20);
            let save = OperationId::new(30);
            let select = OperationId::new(31);

            drive_write(&mut transport, save, 1, &header(0, 20)).expect("attempt 1 header");
            assert!(drive_ack(&mut transport, save, 1, 0xff).is_err());
            drive_cleanup_write(&mut transport, 2).expect("save reset");
            drive_ack(&mut transport, save, 2, 0x80).expect("save reset ACK");
            drive_write(&mut transport, save, 3, &header(0, 20)).expect("attempt 2 header");
            drive_ack(&mut transport, save, 3, 0xff).expect("attempt 2 header ACK");
            drive_write(&mut transport, save, 4, &[0x66; 20]).expect("attempt 2 payload");
            drive_ack(&mut transport, save, 4, 0xff).expect("attempt 2 payload ACK");
            assert!(
                drive_write(&mut transport, select, 5, &[READY, 3, SELECT]).is_err()
                    || drive_ack(&mut transport, select, 5, 0xff).is_err()
            );
            drive_cleanup_write(&mut transport, 6).expect("select reset");
            drive_ack(&mut transport, select, 6, 0x80).expect("select reset ACK");

            let projection = recorder.projection();
            assert_eq!(projection["chunks"].as_array().map(Vec::len), Some(2));
            assert_eq!(projection["chunks"][0]["attempt"], 1);
            assert_eq!(projection["chunks"][0]["status"], "failed");
            assert_eq!(projection["chunks"][1]["attempt"], 2);
            assert_eq!(projection["chunks"][1]["status"], "acked");
            assert_eq!(projection["selects"][0]["status"], "failed");
            assert_eq!(
                projection["cleanup_resets"].as_array().map(Vec::len),
                Some(2)
            );
            assert_eq!(projection["cleanup_resets"][0]["status"], "acked");
            assert_eq!(projection["cleanup_resets"][1]["status"], "acked");
        }
    }
}
