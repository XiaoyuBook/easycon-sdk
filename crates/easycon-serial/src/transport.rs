use std::sync::Arc;

use easycon_controller::{
    AckFrame, AckRequest, ControllerTransport, HandshakeRequest, TransportError,
    TransportErrorKind, WriteKind, WriteRequest,
};
use easycon_runtime::Clock;

use crate::{
    ByteIo, ByteIoFactory, ByteIoOperation, ByteIoRequest, SerialError, SerialErrorKind,
    SerialPortDescriptor,
};

/// `ControllerTransport` adapter over an injectable serial byte stream.
pub struct SerialControllerTransport {
    clock: Arc<dyn Clock>,
    port: SerialPortDescriptor,
    factory: Box<dyn ByteIoFactory>,
    io: Option<Box<dyn ByteIo>>,
    prepared_command_sequence: Option<u64>,
    active_write: Option<ActiveWrite>,
}

#[derive(Clone, Copy)]
struct ActiveWrite {
    context: easycon_controller::WriteContext,
    accepted: usize,
}

impl SerialControllerTransport {
    /// Creates a closed adapter. The first handshake opens the selected port.
    #[must_use]
    pub fn new(
        clock: Arc<dyn Clock>,
        port: SerialPortDescriptor,
        factory: Box<dyn ByteIoFactory>,
    ) -> Self {
        Self {
            clock,
            port,
            factory,
            io: None,
            prepared_command_sequence: None,
            active_write: None,
        }
    }

    /// Returns the stable descriptor selected for this transport.
    #[must_use]
    pub const fn port(&self) -> &SerialPortDescriptor {
        &self.port
    }

    fn request(
        &self,
        deadline_ns: u64,
        cancellation: easycon_runtime::CancellationToken,
        resource_cancellation: easycon_runtime::CancellationToken,
        operation: ByteIoOperation,
    ) -> ByteIoRequest {
        ByteIoRequest {
            operation,
            clock: self.clock.clone(),
            deadline_ns,
            cancellation,
            resource_cancellation,
        }
    }

    fn close_stream(&mut self) {
        if let Some(mut io) = self.io.take() {
            io.close();
        }
        self.prepared_command_sequence = None;
        self.active_write = None;
    }
}

impl ControllerTransport for SerialControllerTransport {
    fn handshake(&mut self, request: HandshakeRequest) -> Result<(), TransportError> {
        self.close_stream();
        let io_request = self.request(
            request.deadline_ns,
            request.cancellation.clone(),
            request.resource_cancellation.clone(),
            ByteIoOperation::Open,
        );
        check_interruption(&io_request, IoPhase::Protocol)?;
        let mut io = self
            .factory
            .open(&self.port, request.baud_rate, io_request.clone())
            .map_err(|error| map_error(error, IoPhase::Protocol))?;

        let result = (|| {
            write_all(
                io.as_mut(),
                &request.request_bytes,
                &self.request(
                    request.deadline_ns,
                    request.cancellation.clone(),
                    request.resource_cancellation.clone(),
                    ByteIoOperation::HandshakeWrite,
                ),
                IoPhase::Protocol,
            )?;
            let mut reply = [0_u8; 1];
            let read = io
                .read(
                    &mut reply,
                    self.request(
                        request.deadline_ns,
                        request.cancellation,
                        request.resource_cancellation,
                        ByteIoOperation::HandshakeRead,
                    ),
                )
                .map_err(|error| map_error(error, IoPhase::Protocol))?;
            validate_progress(read, reply.len(), IoPhase::Protocol)?;
            if reply[0] != request.expected_reply {
                return Err(TransportError::new(
                    TransportErrorKind::Protocol,
                    "serial handshake reply did not match",
                ));
            }
            Ok(())
        })();

        if result.is_ok() {
            self.io = Some(io);
        } else {
            io.close();
        }
        result
    }

    fn write(&mut self, request: WriteRequest<'_>) -> Result<usize, TransportError> {
        if request.context.total_len == 0 || request.bytes.is_empty() {
            return Err(TransportError::new(
                TransportErrorKind::Protocol,
                "serial logical write must be non-empty",
            ));
        }
        let Some(offset) = request.context.total_len.checked_sub(request.bytes.len()) else {
            return Err(TransportError::new(
                TransportErrorKind::Protocol,
                "serial write remainder exceeds its logical payload",
            ));
        };
        match self.active_write {
            Some(active) if active.context != request.context || active.accepted != offset => {
                self.close_stream();
                return Err(TransportError::new(
                    TransportErrorKind::Disconnected,
                    "serial partial-write continuity was lost; stream closed",
                ));
            }
            None if offset != 0 => {
                self.close_stream();
                return Err(TransportError::new(
                    TransportErrorKind::Disconnected,
                    "serial write resumed without an owned partial payload; stream closed",
                ));
            }
            Some(_) | None => {}
        }
        let io_request = self.request(
            request.deadline_ns,
            request.cancellation,
            request.resource_cancellation,
            ByteIoOperation::ControllerWrite(request.context),
        );
        if let Err(error) = check_interruption(&io_request, IoPhase::Write) {
            return Err(self.close_after_partial_failure(offset, error));
        }
        if self.io.is_none() {
            return Err(self.close_after_partial_failure(offset, disconnected()));
        }
        let io = self.io.as_mut().expect("serial stream checked above");

        if request.context.kind == WriteKind::Command
            && self.prepared_command_sequence != Some(request.context.sequence)
        {
            let mut purge_request = io_request.clone();
            purge_request.operation = ByteIoOperation::DiscardInput {
                write_sequence: request.context.sequence,
            };
            io.discard_input(purge_request)
                .map_err(|error| map_error(error, IoPhase::Write))?;
            self.prepared_command_sequence = Some(request.context.sequence);
        }

        let result = io
            .write(request.bytes, io_request)
            .map_err(|error| map_error(error, IoPhase::Write))
            .and_then(|written| {
                validate_progress(written, request.bytes.len(), IoPhase::Write)?;
                Ok(written)
            });
        let written = match result {
            Ok(written) => written,
            Err(error) if offset != 0 => {
                return Err(self.close_after_partial_failure(offset, error));
            }
            Err(error) => return Err(error),
        };
        let accepted = offset
            .checked_add(written)
            .expect("serial accepted-byte count cannot overflow total length");
        if accepted == request.context.total_len {
            self.active_write = None;
        } else {
            self.active_write = Some(ActiveWrite {
                context: request.context,
                accepted,
            });
        }
        Ok(written)
    }

    fn wait_for_ack(&mut self, request: AckRequest) -> Result<AckFrame, TransportError> {
        let io_request = self.request(
            request.deadline_ns,
            request.cancellation,
            request.resource_cancellation,
            ByteIoOperation::AckRead {
                generation: request.generation,
            },
        );
        check_interruption(&io_request, IoPhase::Protocol)?;
        let io = self.io.as_mut().ok_or_else(disconnected)?;
        let mut byte = [0_u8; 1];
        let read = io
            .read(&mut byte, io_request)
            .map_err(|error| map_error(error, IoPhase::Protocol))?;
        validate_progress(read, byte.len(), IoPhase::Protocol)?;
        Ok(AckFrame {
            generation: request.generation,
            byte: byte[0],
        })
    }

    fn close(&mut self) {
        self.close_stream();
    }
}

impl SerialControllerTransport {
    fn close_after_partial_failure(
        &mut self,
        accepted_prefix: usize,
        error: TransportError,
    ) -> TransportError {
        if accepted_prefix == 0 {
            return error;
        }
        self.close_stream();
        TransportError::new(
            TransportErrorKind::Disconnected,
            format!("serial stream closed after partial-write failure: {error}"),
        )
    }
}

impl Drop for SerialControllerTransport {
    fn drop(&mut self) {
        self.close_stream();
    }
}

#[derive(Clone, Copy)]
enum IoPhase {
    Protocol,
    Write,
}

fn write_all(
    io: &mut dyn ByteIo,
    bytes: &[u8],
    request: &ByteIoRequest,
    phase: IoPhase,
) -> Result<(), TransportError> {
    let mut written = 0;
    while written < bytes.len() {
        check_interruption(request, phase)?;
        let accepted = io
            .write(&bytes[written..], request.clone())
            .map_err(|error| map_error(error, phase))?;
        validate_progress(accepted, bytes.len() - written, phase)?;
        written += accepted;
    }
    Ok(())
}

fn validate_progress(
    progress: usize,
    remaining: usize,
    phase: IoPhase,
) -> Result<(), TransportError> {
    if progress == 0 {
        return Err(map_error(
            SerialError::new(
                SerialErrorKind::ZeroProgress,
                "serial byte I/O returned zero progress",
            ),
            phase,
        ));
    }
    if progress > remaining {
        return Err(TransportError::new(
            TransportErrorKind::Protocol,
            "serial byte I/O exceeded the supplied buffer",
        ));
    }
    Ok(())
}

fn check_interruption(request: &ByteIoRequest, phase: IoPhase) -> Result<(), TransportError> {
    request
        .interruption()
        .map_or(Ok(()), |error| Err(map_error(error, phase)))
}

fn disconnected() -> TransportError {
    TransportError::new(TransportErrorKind::Disconnected, "serial port is not open")
}

fn map_error(error: SerialError, phase: IoPhase) -> TransportError {
    let kind = match error.kind() {
        SerialErrorKind::Cancelled => TransportErrorKind::Cancelled,
        SerialErrorKind::DeadlineExceeded => match phase {
            IoPhase::Protocol => TransportErrorKind::Timeout,
            IoPhase::Write => TransportErrorKind::WriteTimeout,
        },
        SerialErrorKind::Disconnected | SerialErrorKind::NotFound => {
            TransportErrorKind::Disconnected
        }
        SerialErrorKind::Protocol => TransportErrorKind::Protocol,
        SerialErrorKind::AccessDenied
        | SerialErrorKind::PortBusy
        | SerialErrorKind::ZeroProgress
        | SerialErrorKind::InvalidPort
        | SerialErrorKind::Io
        | SerialErrorKind::UnsupportedPlatform => TransportErrorKind::Io,
    };
    TransportError::new(kind, error.to_string())
}
