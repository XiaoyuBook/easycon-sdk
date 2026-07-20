use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use easycon_controller::{HANDSHAKE_REPLY, HANDSHAKE_REQUEST, WriteContext, WriteKind};
use easycon_runtime::{Clock, ClockChangeRegistration, VirtualClock};
use easycon_serial::{
    ByteIo, ByteIoFactory, ByteIoOperation, ByteIoRequest, SerialControllerTransport, SerialError,
    SerialErrorKind, SerialPortDescriptor,
};

/// Scripted result produced after an exact CH32 handshake request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ch32HandshakeBehavior {
    /// Fail before a stream is opened, for example access denied or port busy.
    OpenError(SerialErrorKind),
    /// Queue the source-exact hello reply.
    Success,
    /// Leave the read pending until its absolute deadline.
    Timeout,
    /// Queue a non-matching protocol byte.
    WrongReply(u8),
    /// Fail the handshake read with a stable serial error.
    Error(SerialErrorKind),
}

/// Scripted byte response produced after one complete Controller command write.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Ch32AckBehavior {
    /// Queue one byte immediately.
    Reply(u8),
    /// Queue one byte at a future virtual-clock instant.
    Delayed { byte: u8, elapsed_ns: u64 },
    /// Queue the same byte twice to model a duplicate reply.
    Duplicate(u8),
    /// Leave the read pending until deadline or cancellation.
    NoReply,
    /// Fail the next read.
    Error(SerialErrorKind),
    /// Disconnect immediately after command acceptance.
    Disconnect,
}

/// One exact eight-byte report reconstructed below `ControllerTransport`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ch32AcceptedReport {
    /// Controller logical-write metadata shared by every partial byte call.
    pub context: WriteContext,
    /// Reassembled source-exact report bytes.
    pub bytes: [u8; 8],
}

/// One Amiibo chunk payload observed after its source-exact save header.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ch32AmiiboChunk {
    /// Zero-based slot from the save header.
    pub slot: u8,
    /// Byte offset decoded from the two seven-bit fields.
    pub offset: usize,
    /// Exact payload bytes written for this attempt.
    pub bytes: Vec<u8>,
}

/// Immutable byte-device accounting snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Ch32Snapshot {
    /// Baud attempts in open order.
    pub baud_attempts: Vec<u32>,
    /// Complete reports in transport order.
    pub reports: Vec<Ch32AcceptedReport>,
    /// Complete request/response command payloads.
    pub commands: Vec<Vec<u8>>,
    /// Amiibo payload attempts reconstructed from save headers.
    pub amiibo_chunks: Vec<Ch32AmiiboChunk>,
    /// Source-exact zero-based select requests.
    pub amiibo_selections: Vec<u8>,
    /// Bytes removed at request-generation boundaries.
    pub discarded_input_bytes: usize,
    /// Streams currently owned by a transport.
    pub active_streams: usize,
    /// Streams closed exactly once.
    pub closed_streams: usize,
    /// Successful non-zero partial byte-write calls.
    pub byte_write_calls: usize,
}

/// Cloneable, byte-level CH32 protocol simulator compiled only in test support.
#[derive(Clone)]
pub struct Ch32ByteSimulator {
    clock: Arc<VirtualClock>,
    shared: Arc<Shared>,
    _clock_hook: Arc<ClockChangeRegistration>,
}

struct Shared {
    state: Mutex<State>,
    changed: Condvar,
}

struct State {
    handshake_scripts: VecDeque<(u32, Ch32HandshakeBehavior)>,
    ack_scripts: VecDeque<Ch32AckBehavior>,
    baud_attempts: Vec<u32>,
    next_stream_id: u64,
    active_stream_id: Option<u64>,
    active_streams: usize,
    closed_streams: usize,
    stream_connected: bool,
    handshake_behavior: Option<Ch32HandshakeBehavior>,
    handshake_bytes: Vec<u8>,
    controller_partial: Option<ControllerPartial>,
    reports: Vec<Ch32AcceptedReport>,
    commands: Vec<Vec<u8>>,
    amiibo_chunks: Vec<Ch32AmiiboChunk>,
    amiibo_selections: Vec<u8>,
    pending_amiibo: Option<PendingAmiibo>,
    incoming: VecDeque<u8>,
    scheduled: Vec<ScheduledByte>,
    next_read_error: Option<SerialErrorKind>,
    maximum_write_chunk: usize,
    maximum_read_chunk: usize,
    zero_next_write: bool,
    zero_next_read: bool,
    fail_next_write: Option<SerialErrorKind>,
    fail_next_read: Option<SerialErrorKind>,
    fail_controller_write_after_bytes: Option<(usize, SerialErrorKind)>,
    block_next_write: bool,
    block_next_read: bool,
    block_controller_write_after_bytes: Option<usize>,
    write_waiting: bool,
    read_waiting: bool,
    discarded_input_bytes: usize,
    byte_write_calls: usize,
}

struct ControllerPartial {
    context: WriteContext,
    bytes: Vec<u8>,
}

#[derive(Clone, Copy)]
struct PendingAmiibo {
    slot: u8,
    offset: usize,
    length: usize,
}

#[derive(Clone, Copy)]
struct ScheduledByte {
    target_ns: u64,
    byte: u8,
}

impl Ch32ByteSimulator {
    /// Creates an initially closed simulator on a manually advanced clock.
    #[must_use]
    pub fn new(clock: Arc<VirtualClock>) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                handshake_scripts: VecDeque::new(),
                ack_scripts: VecDeque::new(),
                baud_attempts: Vec::new(),
                next_stream_id: 1,
                active_stream_id: None,
                active_streams: 0,
                closed_streams: 0,
                stream_connected: false,
                handshake_behavior: None,
                handshake_bytes: Vec::new(),
                controller_partial: None,
                reports: Vec::new(),
                commands: Vec::new(),
                amiibo_chunks: Vec::new(),
                amiibo_selections: Vec::new(),
                pending_amiibo: None,
                incoming: VecDeque::new(),
                scheduled: Vec::new(),
                next_read_error: None,
                maximum_write_chunk: usize::MAX,
                maximum_read_chunk: usize::MAX,
                zero_next_write: false,
                zero_next_read: false,
                fail_next_write: None,
                fail_next_read: None,
                fail_controller_write_after_bytes: None,
                block_next_write: false,
                block_next_read: false,
                block_controller_write_after_bytes: None,
                write_waiting: false,
                read_waiting: false,
                discarded_input_bytes: 0,
                byte_write_calls: 0,
            }),
            changed: Condvar::new(),
        });
        let wake = Arc::downgrade(&shared);
        let hook = clock.on_change(Arc::new(move || {
            if let Some(shared) = wake.upgrade() {
                shared.changed.notify_all();
            }
        }));
        Self {
            clock,
            shared,
            _clock_hook: Arc::new(hook),
        }
    }

    /// Appends one exact baud attempt and its handshake behavior.
    pub fn push_handshake(&self, baud_rate: u32, behavior: Ch32HandshakeBehavior) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .handshake_scripts
            .push_back((baud_rate, behavior));
    }

    /// Appends one response for the next complete command.
    pub fn push_ack(&self, behavior: Ch32AckBehavior) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .ack_scripts
            .push_back(behavior);
    }

    /// Limits every byte write to a non-zero prefix.
    pub fn set_maximum_write_chunk(&self, maximum: usize) {
        assert!(maximum != 0, "CH32 write chunk must be non-zero");
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .maximum_write_chunk = maximum;
    }

    /// Limits every byte read to a non-zero prefix.
    pub fn set_maximum_read_chunk(&self, maximum: usize) {
        assert!(maximum != 0, "CH32 read chunk must be non-zero");
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .maximum_read_chunk = maximum;
    }

    /// Makes the next write report zero progress.
    pub fn zero_next_write(&self) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .zero_next_write = true;
    }

    /// Makes the next read report zero progress.
    pub fn zero_next_read(&self) {
        self.shared.state.lock().expect("CH32 state").zero_next_read = true;
    }

    /// Fails the next byte write before accepting its prefix.
    pub fn fail_next_write(&self, kind: SerialErrorKind) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .fail_next_write = Some(kind);
    }

    /// Fails the next byte read.
    pub fn fail_next_read(&self, kind: SerialErrorKind) {
        self.shared.state.lock().expect("CH32 state").fail_next_read = Some(kind);
    }

    /// Fails a Controller logical write after an exact accepted prefix length.
    pub fn fail_controller_write_after_bytes(&self, accepted: usize, kind: SerialErrorKind) {
        assert!(accepted != 0, "partial-failure prefix must be non-zero");
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .fail_controller_write_after_bytes = Some((accepted, kind));
    }

    /// Blocks the next write before accepting bytes until cancel, deadline, or disconnect.
    pub fn block_next_write(&self) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .block_next_write = true;
    }

    /// Blocks the next read even if a byte is queued until cancel, deadline, or disconnect.
    pub fn block_next_read(&self) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .block_next_read = true;
    }

    /// Blocks a Controller logical write after an exact accepted prefix length.
    pub fn block_controller_write_after_bytes(&self, accepted: usize) {
        assert!(accepted != 0, "partial-block prefix must be non-zero");
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .block_controller_write_after_bytes = Some(accepted);
    }

    /// Simulates hot unplug and wakes every blocked byte call.
    pub fn disconnect(&self) {
        self.shared
            .state
            .lock()
            .expect("CH32 state")
            .stream_connected = false;
        self.shared.changed.notify_all();
    }

    /// Waits on a controlled barrier until a write is blocked.
    #[must_use]
    pub fn wait_until_write_blocked(&self, timeout: Duration) -> bool {
        wait_for_flag(&self.shared, timeout, |state| state.write_waiting)
    }

    /// Waits on a controlled barrier until a read is blocked.
    #[must_use]
    pub fn wait_until_read_blocked(&self, timeout: Duration) -> bool {
        wait_for_flag(&self.shared, timeout, |state| state.read_waiting)
    }

    /// Returns immutable protocol and ownership accounting.
    #[must_use]
    pub fn snapshot(&self) -> Ch32Snapshot {
        let state = self.shared.state.lock().expect("CH32 state");
        Ch32Snapshot {
            baud_attempts: state.baud_attempts.clone(),
            reports: state.reports.clone(),
            commands: state.commands.clone(),
            amiibo_chunks: state.amiibo_chunks.clone(),
            amiibo_selections: state.amiibo_selections.clone(),
            discarded_input_bytes: state.discarded_input_bytes,
            active_streams: state.active_streams,
            closed_streams: state.closed_streams,
            byte_write_calls: state.byte_write_calls,
        }
    }

    /// Creates the production serial adapter over this test-only byte factory.
    #[must_use]
    pub fn transport(&self, port: SerialPortDescriptor) -> SerialControllerTransport {
        SerialControllerTransport::new(self.clock.clone(), port, Box::new(self.clone()))
    }
}

impl ByteIoFactory for Ch32ByteSimulator {
    fn open(
        &mut self,
        _port: &SerialPortDescriptor,
        baud_rate: u32,
        request: ByteIoRequest,
    ) -> Result<Box<dyn ByteIo>, SerialError> {
        if request.operation != ByteIoOperation::Open {
            return Err(protocol_error("CH32 factory received a non-open request"));
        }
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        let mut state = self.shared.state.lock().expect("CH32 state");
        if state.active_streams != 0 {
            return Err(SerialError::new(
                SerialErrorKind::PortBusy,
                "CH32 simulated port already has an owner",
            ));
        }
        let Some((expected_baud, behavior)) = state.handshake_scripts.pop_front() else {
            return Err(protocol_error("no scripted CH32 handshake attempt"));
        };
        if expected_baud != baud_rate {
            return Err(protocol_error("CH32 baud order differed from the script"));
        }
        state.baud_attempts.push(baud_rate);
        if let Ch32HandshakeBehavior::OpenError(kind) = behavior {
            return Err(scripted_error(kind, "scripted CH32 open failure"));
        }
        let stream_id = state.next_stream_id;
        state.next_stream_id = state
            .next_stream_id
            .checked_add(1)
            .expect("CH32 stream ID exhausted");
        state.active_stream_id = Some(stream_id);
        state.active_streams = 1;
        state.stream_connected = true;
        state.handshake_behavior = Some(behavior);
        state.handshake_bytes.clear();
        state.controller_partial = None;
        state.incoming.clear();
        state.scheduled.clear();
        state.next_read_error = None;
        state.pending_amiibo = None;
        drop(state);
        Ok(Box::new(SimulatedByteIo {
            clock: self.clock.clone(),
            shared: self.shared.clone(),
            stream_id,
            closed: false,
        }))
    }
}

struct SimulatedByteIo {
    clock: Arc<VirtualClock>,
    shared: Arc<Shared>,
    stream_id: u64,
    closed: bool,
}

impl ByteIo for SimulatedByteIo {
    fn read(&mut self, buffer: &mut [u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        if buffer.is_empty() {
            return Err(protocol_error("CH32 read buffer is empty"));
        }
        if !matches!(
            request.operation,
            ByteIoOperation::HandshakeRead | ByteIoOperation::AckRead { .. }
        ) {
            return Err(protocol_error("CH32 received an unexpected read purpose"));
        }
        let operation_wake = self.shared.clone();
        let _operation_hook = request
            .cancellation
            .on_cancel_scoped(move || operation_wake.changed.notify_all());
        let resource_wake = self.shared.clone();
        let _resource_hook = request
            .resource_cancellation
            .on_cancel_scoped(move || resource_wake.changed.notify_all());
        let mut state = self.shared.state.lock().expect("CH32 state");
        ensure_stream(&state, self.stream_id)?;
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        if state.zero_next_read {
            state.zero_next_read = false;
            return Ok(0);
        }
        if let Some(kind) = state.fail_next_read.take() {
            return Err(scripted_error(kind, "scripted CH32 read failure"));
        }
        let forced_block = std::mem::take(&mut state.block_next_read);
        loop {
            promote_scheduled(&mut state, self.clock.now_ns());
            if let Some(error) = request.interruption() {
                state.read_waiting = false;
                self.shared.changed.notify_all();
                return Err(error);
            }
            if !state.stream_connected || state.active_stream_id != Some(self.stream_id) {
                state.read_waiting = false;
                self.shared.changed.notify_all();
                return Err(disconnected_error("CH32 disconnected during read"));
            }
            if let Some(kind) = state.next_read_error.take() {
                state.read_waiting = false;
                self.shared.changed.notify_all();
                return Err(scripted_error(kind, "scripted CH32 protocol read failure"));
            }
            if !forced_block && !state.incoming.is_empty() {
                break;
            }
            state.read_waiting = true;
            self.shared.changed.notify_all();
            state = self
                .shared
                .changed
                .wait(state)
                .expect("CH32 state while read waits");
        }
        state.read_waiting = false;
        let count = buffer
            .len()
            .min(state.maximum_read_chunk)
            .min(state.incoming.len());
        for output in &mut buffer[..count] {
            *output = state.incoming.pop_front().expect("CH32 incoming count");
        }
        self.shared.changed.notify_all();
        Ok(count)
    }

    fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        if buffer.is_empty() {
            return Err(protocol_error("CH32 write buffer is empty"));
        }
        if !matches!(
            request.operation,
            ByteIoOperation::HandshakeWrite | ByteIoOperation::ControllerWrite(_)
        ) {
            return Err(protocol_error("CH32 received an unexpected write purpose"));
        }
        let operation_wake = self.shared.clone();
        let _operation_hook = request
            .cancellation
            .on_cancel_scoped(move || operation_wake.changed.notify_all());
        let resource_wake = self.shared.clone();
        let _resource_hook = request
            .resource_cancellation
            .on_cancel_scoped(move || resource_wake.changed.notify_all());
        let mut state = self.shared.state.lock().expect("CH32 state");
        ensure_stream(&state, self.stream_id)?;
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        if let Some(kind) = state.fail_next_write.take() {
            state.controller_partial = None;
            state.pending_amiibo = None;
            return Err(scripted_error(kind, "scripted CH32 write failure"));
        }
        if matches!(request.operation, ByteIoOperation::ControllerWrite(_))
            && state
                .fail_controller_write_after_bytes
                .is_some_and(|(accepted, _)| {
                    state
                        .controller_partial
                        .as_ref()
                        .is_some_and(|partial| partial.bytes.len() == accepted)
                })
        {
            let (_, kind) = state
                .fail_controller_write_after_bytes
                .take()
                .expect("CH32 partial failure checked above");
            return Err(scripted_error(
                kind,
                "scripted CH32 failure after a partial logical write",
            ));
        }
        if state.zero_next_write {
            state.zero_next_write = false;
            return Ok(0);
        }
        let block_after_partial = matches!(request.operation, ByteIoOperation::ControllerWrite(_))
            && state
                .block_controller_write_after_bytes
                .is_some_and(|accepted| {
                    state
                        .controller_partial
                        .as_ref()
                        .is_some_and(|partial| partial.bytes.len() == accepted)
                });
        if block_after_partial {
            state.block_controller_write_after_bytes = None;
        }
        if std::mem::take(&mut state.block_next_write) || block_after_partial {
            state.write_waiting = true;
            self.shared.changed.notify_all();
            loop {
                if let Some(error) = request.interruption() {
                    state.write_waiting = false;
                    self.shared.changed.notify_all();
                    return Err(error);
                }
                if !state.stream_connected || state.active_stream_id != Some(self.stream_id) {
                    state.write_waiting = false;
                    self.shared.changed.notify_all();
                    return Err(disconnected_error("CH32 disconnected during write"));
                }
                state = self
                    .shared
                    .changed
                    .wait(state)
                    .expect("CH32 state while write waits");
            }
        }
        let accepted = buffer.len().min(state.maximum_write_chunk);
        state.byte_write_calls = state
            .byte_write_calls
            .checked_add(1)
            .expect("CH32 byte write count exhausted");
        match request.operation {
            ByteIoOperation::HandshakeWrite => {
                state.handshake_bytes.extend_from_slice(&buffer[..accepted]);
                finish_handshake_if_complete(&mut state)?;
            }
            ByteIoOperation::ControllerWrite(context) => {
                append_controller_bytes(
                    &mut state,
                    context,
                    &buffer[..accepted],
                    self.clock.now_ns(),
                )?;
            }
            ByteIoOperation::Open
            | ByteIoOperation::HandshakeRead
            | ByteIoOperation::AckRead { .. }
            | ByteIoOperation::DiscardInput { .. } => unreachable!("write purpose checked above"),
        }
        self.shared.changed.notify_all();
        Ok(accepted)
    }

    fn discard_input(&mut self, request: ByteIoRequest) -> Result<(), SerialError> {
        if !matches!(request.operation, ByteIoOperation::DiscardInput { .. }) {
            return Err(protocol_error("CH32 received an unexpected purge purpose"));
        }
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        let mut state = self.shared.state.lock().expect("CH32 state");
        ensure_stream(&state, self.stream_id)?;
        let discarded = state.incoming.len();
        state.discarded_input_bytes = state.discarded_input_bytes.saturating_add(discarded);
        state.incoming.clear();
        state.next_read_error = None;
        Ok(())
    }

    fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let mut state = self.shared.state.lock().expect("CH32 state");
        if state.active_stream_id == Some(self.stream_id) {
            state.active_stream_id = None;
            state.active_streams = 0;
            state.stream_connected = false;
            state.handshake_behavior = None;
            state.handshake_bytes.clear();
            state.controller_partial = None;
            state.pending_amiibo = None;
            state.incoming.clear();
            state.scheduled.clear();
            state.closed_streams = state
                .closed_streams
                .checked_add(1)
                .expect("CH32 closed stream count exhausted");
        }
        drop(state);
        self.shared.changed.notify_all();
    }
}

impl Drop for SimulatedByteIo {
    fn drop(&mut self) {
        self.close();
    }
}

fn finish_handshake_if_complete(state: &mut State) -> Result<(), SerialError> {
    if state.handshake_bytes.len() < HANDSHAKE_REQUEST.len() {
        return Ok(());
    }
    if state.handshake_bytes.as_slice() != HANDSHAKE_REQUEST {
        return Err(protocol_error("CH32 handshake bytes were not source-exact"));
    }
    let behavior = state
        .handshake_behavior
        .take()
        .ok_or_else(|| protocol_error("CH32 handshake completed more than once"))?;
    match behavior {
        Ch32HandshakeBehavior::OpenError(_) => {
            return Err(protocol_error(
                "CH32 open failure unexpectedly reached handshake completion",
            ));
        }
        Ch32HandshakeBehavior::Success => state.incoming.push_back(HANDSHAKE_REPLY),
        Ch32HandshakeBehavior::Timeout => {}
        Ch32HandshakeBehavior::WrongReply(byte) => state.incoming.push_back(byte),
        Ch32HandshakeBehavior::Error(kind) => state.next_read_error = Some(kind),
    }
    Ok(())
}

fn append_controller_bytes(
    state: &mut State,
    context: WriteContext,
    bytes: &[u8],
    now_ns: u64,
) -> Result<(), SerialError> {
    if state.controller_partial.is_none() {
        state.controller_partial = Some(ControllerPartial {
            context,
            bytes: Vec::with_capacity(context.total_len),
        });
    }
    let partial = state
        .controller_partial
        .as_mut()
        .expect("CH32 partial initialized");
    if partial.context != context {
        return Err(protocol_error(
            "CH32 logical-write context changed across partial bytes",
        ));
    }
    partial.bytes.extend_from_slice(bytes);
    if partial.bytes.len() > context.total_len {
        state.controller_partial = None;
        return Err(protocol_error(
            "CH32 logical write exceeded its total length",
        ));
    }
    if partial.bytes.len() != context.total_len {
        return Ok(());
    }
    let complete = state
        .controller_partial
        .take()
        .expect("CH32 complete partial exists");
    match context.kind {
        WriteKind::Report | WriteKind::Neutralize => {
            let bytes: [u8; 8] = complete.bytes.try_into().map_err(|_| {
                protocol_error("CH32 Controller report must contain exactly eight bytes")
            })?;
            if bytes[..7].iter().any(|byte| byte & 0x80 != 0) || bytes[7] & 0x80 == 0 {
                return Err(protocol_error("CH32 Controller report framing is invalid"));
            }
            state.reports.push(Ch32AcceptedReport { context, bytes });
        }
        WriteKind::Command => {
            inspect_amiibo_command(state, &complete.bytes)?;
            state.commands.push(complete.bytes);
            apply_ack_script(state, now_ns)?;
        }
    }
    Ok(())
}

fn inspect_amiibo_command(state: &mut State, bytes: &[u8]) -> Result<(), SerialError> {
    const RESET: [u8; 6] = [0xa5, 0x81, 0xa5, 0x81, 0xa5, 0x81];
    if let Some(pending) = state.pending_amiibo.take() {
        if bytes.len() != pending.length {
            return Err(protocol_error(
                "CH32 Amiibo payload length differs from its save header",
            ));
        }
        state.amiibo_chunks.push(Ch32AmiiboChunk {
            slot: pending.slot,
            offset: pending.offset,
            bytes: bytes.to_vec(),
        });
        return Ok(());
    }
    if bytes == RESET {
        return Ok(());
    }
    if let [
        0xa5,
        offset_low,
        offset_high,
        length_low,
        length_high,
        slot,
        0x90,
    ] = bytes
    {
        if offset_low & 0x80 != 0
            || offset_high & 0x80 != 0
            || length_low & 0x80 != 0
            || length_high & 0x80 != 0
        {
            return Err(protocol_error(
                "CH32 Amiibo save header is not seven-bit encoded",
            ));
        }
        let length = usize::from(*length_low) | (usize::from(*length_high) << 7);
        if length == 0 || length > 20 {
            return Err(protocol_error("CH32 Amiibo save chunk length is invalid"));
        }
        state.pending_amiibo = Some(PendingAmiibo {
            slot: *slot,
            offset: usize::from(*offset_low) | (usize::from(*offset_high) << 7),
            length,
        });
        return Ok(());
    }
    if let [0xa5, slot, 0x91] = bytes {
        state.amiibo_selections.push(*slot);
    }
    Ok(())
}

fn apply_ack_script(state: &mut State, now_ns: u64) -> Result<(), SerialError> {
    let behavior = state
        .ack_scripts
        .pop_front()
        .ok_or_else(|| protocol_error("no scripted CH32 ACK behavior"))?;
    let keeps_amiibo_payload_pending = matches!(
        behavior,
        Ch32AckBehavior::Reply(0xff)
            | Ch32AckBehavior::Delayed { byte: 0xff, .. }
            | Ch32AckBehavior::Duplicate(0xff)
    );
    match behavior {
        Ch32AckBehavior::Reply(byte) => state.incoming.push_back(byte),
        Ch32AckBehavior::Delayed { byte, elapsed_ns } => {
            state.scheduled.push(ScheduledByte {
                target_ns: now_ns.saturating_add(elapsed_ns),
                byte,
            });
        }
        Ch32AckBehavior::Duplicate(byte) => {
            state.incoming.push_back(byte);
            state.incoming.push_back(byte);
        }
        Ch32AckBehavior::NoReply => {}
        Ch32AckBehavior::Error(kind) => state.next_read_error = Some(kind),
        Ch32AckBehavior::Disconnect => state.stream_connected = false,
    }
    if state.pending_amiibo.is_some() && !keeps_amiibo_payload_pending {
        state.pending_amiibo = None;
    }
    Ok(())
}

fn promote_scheduled(state: &mut State, now_ns: u64) {
    let mut due = Vec::new();
    state.scheduled.retain(|scheduled| {
        if scheduled.target_ns <= now_ns {
            due.push(*scheduled);
            false
        } else {
            true
        }
    });
    due.sort_by_key(|scheduled| scheduled.target_ns);
    state
        .incoming
        .extend(due.into_iter().map(|scheduled| scheduled.byte));
}

fn ensure_stream(state: &State, stream_id: u64) -> Result<(), SerialError> {
    if !state.stream_connected || state.active_stream_id != Some(stream_id) {
        Err(disconnected_error("CH32 simulated stream is disconnected"))
    } else {
        Ok(())
    }
}

fn wait_for_flag(shared: &Shared, timeout: Duration, predicate: impl Fn(&State) -> bool) -> bool {
    let state = shared.state.lock().expect("CH32 state");
    let (state, _) = shared
        .changed
        .wait_timeout_while(state, timeout, |state| !predicate(state))
        .expect("CH32 state while waiting for barrier");
    predicate(&state)
}

fn protocol_error(message: &'static str) -> SerialError {
    SerialError::new(SerialErrorKind::Protocol, message)
}

fn disconnected_error(message: &'static str) -> SerialError {
    SerialError::new(SerialErrorKind::Disconnected, message)
}

fn scripted_error(kind: SerialErrorKind, message: &'static str) -> SerialError {
    SerialError::new(kind, message)
}
