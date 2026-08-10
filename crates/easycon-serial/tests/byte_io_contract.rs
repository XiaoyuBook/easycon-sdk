use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use easycon_controller::{
    AckRequest, ControllerTransport, HANDSHAKE_REPLY, HANDSHAKE_REQUEST, HandshakeRequest,
    TransportErrorKind, WriteContext, WriteKind, WriteRequest, WriteSettlement,
};
use easycon_runtime::{CancellationToken, Clock, VirtualClock};
use easycon_serial::{
    ByteIo, ByteIoFactory, ByteIoRequest, SerialControllerTransport, SerialError,
    SerialPortDescriptor,
};

#[derive(Default)]
struct State {
    opened_bauds: Vec<u32>,
    reads: VecDeque<Result<Vec<u8>, SerialError>>,
    writes: Vec<u8>,
    chunks: VecDeque<usize>,
    discard_count: usize,
    closes: usize,
}

struct Factory(Arc<Mutex<State>>);

impl ByteIoFactory for Factory {
    fn open(
        &mut self,
        _port: &SerialPortDescriptor,
        baud_rate: u32,
        request: ByteIoRequest,
    ) -> Result<Box<dyn ByteIo>, SerialError> {
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        self.0.lock().expect("state").opened_bauds.push(baud_rate);
        Ok(Box::new(Io(self.0.clone())))
    }
}

struct Io(Arc<Mutex<State>>);

impl ByteIo for Io {
    fn read(&mut self, buffer: &mut [u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        let bytes = self
            .0
            .lock()
            .expect("state")
            .reads
            .pop_front()
            .expect("scripted read")?;
        let count = bytes.len().min(buffer.len());
        buffer[..count].copy_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn write(&mut self, buffer: &[u8], request: ByteIoRequest) -> Result<usize, SerialError> {
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        let count = self
            .0
            .lock()
            .expect("state")
            .chunks
            .pop_front()
            .unwrap_or(buffer.len())
            .min(buffer.len());
        request.publish_final_write_acceptance(count)?;
        let mut state = self.0.lock().expect("state after completion");
        state.writes.extend_from_slice(&buffer[..count]);
        Ok(count)
    }

    fn discard_input(&mut self, request: ByteIoRequest) -> Result<(), SerialError> {
        if let Some(error) = request.interruption() {
            return Err(error);
        }
        self.0.lock().expect("state").discard_count += 1;
        Ok(())
    }

    fn close(&mut self) {
        self.0.lock().expect("state").closes += 1;
    }
}

fn transport(clock: Arc<VirtualClock>, state: Arc<Mutex<State>>) -> SerialControllerTransport {
    let port = SerialPortDescriptor::new("ROOT\\TEST\\0", "COM-test").expect("port");
    SerialControllerTransport::new(clock, port, Box::new(Factory(state)))
}

fn handshake_request(clock: &VirtualClock) -> HandshakeRequest {
    HandshakeRequest {
        operation_id: easycon_model_id(1),
        baud_rate: 115_200,
        request_bytes: HANDSHAKE_REQUEST,
        expected_reply: HANDSHAKE_REPLY,
        deadline_ns: clock.now_ns() + 100,
        cancellation: CancellationToken::root(),
        resource_cancellation: CancellationToken::root(),
    }
}

fn easycon_model_id(value: u64) -> easycon_model::OperationId {
    easycon_model::OperationId::new(value)
}

// conformance: phase2a.serial.injectable-contract
#[test]
fn injectable_adapter_handles_partial_handshake_and_command_generation() {
    let clock = Arc::new(VirtualClock::new(10));
    let state = Arc::new(Mutex::new(State {
        reads: VecDeque::from([Ok(vec![HANDSHAKE_REPLY]), Ok(vec![0xff])]),
        chunks: VecDeque::from([1, 1, 1, 1, 1]),
        ..State::default()
    }));
    let mut transport = transport(clock.clone(), state.clone());

    transport
        .handshake(handshake_request(&clock))
        .expect("handshake");
    let context = WriteContext {
        resource_id: easycon_model::ResourceId::new(1),
        operation_id: Some(easycon_model_id(2)),
        sequence: 7,
        timestamp_ns: clock.now_ns(),
        direct_timing: None,
        total_len: 2,
        kind: WriteKind::Command,
    };
    let cancellation = CancellationToken::root();
    let resource_cancellation = CancellationToken::root();
    let settlement = WriteSettlement::untracked();
    assert_eq!(
        transport
            .write(WriteRequest {
                context,
                bytes: &[0xa5, 0x91],
                deadline_ns: 100,
                cancellation: cancellation.clone(),
                resource_cancellation: resource_cancellation.clone(),
                settlement: settlement.clone(),
            })
            .expect("first partial"),
        1
    );
    assert!(
        !settlement.is_full_accepted(),
        "partial transport completion must not accept the logical report"
    );
    assert_eq!(
        transport
            .write(WriteRequest {
                context,
                bytes: &[0x91],
                deadline_ns: 100,
                cancellation: cancellation.clone(),
                resource_cancellation: resource_cancellation.clone(),
                settlement: settlement.clone(),
            })
            .expect("second partial"),
        1
    );
    assert!(
        settlement.is_full_accepted(),
        "serial final-byte completion must claim the logical settlement gate"
    );
    let frame = transport
        .wait_for_ack(AckRequest {
            operation_id: easycon_model_id(2),
            generation: 41,
            expected_reply: 0xff,
            deadline_ns: 100,
            cancellation,
            resource_cancellation,
        })
        .expect("ACK");

    assert_eq!(frame.generation, 41);
    assert_eq!(frame.byte, 0xff);
    let state = state.lock().expect("state");
    assert_eq!(state.opened_bauds, [115_200]);
    assert_eq!(state.writes, [0xa5, 0xa5, 0x81, 0xa5, 0x91]);
    assert_eq!(state.discard_count, 1);
}

#[test]
fn zero_progress_deadline_and_cancellation_are_normalized() {
    let clock = Arc::new(VirtualClock::new(10));
    let state = Arc::new(Mutex::new(State {
        reads: VecDeque::from([Ok(vec![HANDSHAKE_REPLY])]),
        chunks: VecDeque::from([3, 0]),
        ..State::default()
    }));
    let mut transport = transport(clock.clone(), state);
    transport
        .handshake(handshake_request(&clock))
        .expect("handshake");

    let cancellation = CancellationToken::root();
    let resource_cancellation = CancellationToken::root();
    let context = WriteContext {
        resource_id: easycon_model::ResourceId::new(1),
        operation_id: Some(easycon_model_id(2)),
        sequence: 1,
        timestamp_ns: 10,
        direct_timing: None,
        total_len: 1,
        kind: WriteKind::Report,
    };
    let error = transport
        .write(WriteRequest {
            context,
            bytes: &[1],
            deadline_ns: 100,
            cancellation: cancellation.clone(),
            resource_cancellation: resource_cancellation.clone(),
            settlement: WriteSettlement::untracked(),
        })
        .expect_err("zero progress");
    assert_eq!(error.kind(), TransportErrorKind::Io);

    clock.advance_to(100);
    let error = transport
        .write(WriteRequest {
            context,
            bytes: &[1],
            deadline_ns: 100,
            cancellation: cancellation.clone(),
            resource_cancellation: resource_cancellation.clone(),
            settlement: WriteSettlement::untracked(),
        })
        .expect_err("deadline");
    assert_eq!(error.kind(), TransportErrorKind::WriteTimeout);

    cancellation.cancel();
    let error = transport
        .wait_for_ack(AckRequest {
            operation_id: easycon_model_id(2),
            generation: 1,
            expected_reply: 0xff,
            deadline_ns: 200,
            cancellation,
            resource_cancellation,
        })
        .expect_err("cancelled");
    assert_eq!(error.kind(), TransportErrorKind::Cancelled);
}

#[test]
fn close_is_idempotent_and_reopen_replaces_the_previous_stream() {
    let clock = Arc::new(VirtualClock::new(10));
    let state = Arc::new(Mutex::new(State {
        reads: VecDeque::from([Ok(vec![HANDSHAKE_REPLY]), Ok(vec![HANDSHAKE_REPLY])]),
        chunks: VecDeque::from([3, 3]),
        ..State::default()
    }));
    let mut transport = transport(clock.clone(), state.clone());

    transport
        .handshake(handshake_request(&clock))
        .expect("first handshake");
    transport
        .handshake(handshake_request(&clock))
        .expect("replacement handshake");
    transport.close();
    transport.close();

    assert_eq!(state.lock().expect("state").closes, 2);
}
