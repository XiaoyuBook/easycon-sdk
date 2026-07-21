use std::io::{self, IsTerminal, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

const OPERATOR_POLL_SLICE: Duration = Duration::from_millis(50);

#[derive(Clone, Debug, Default)]
pub(crate) struct InterruptToken {
    requested: Arc<AtomicBool>,
}

impl InterruptToken {
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::Release);
    }

    pub(crate) fn is_requested(&self) -> bool {
        self.requested.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperatorOutcome {
    Yes,
    No,
    Eof,
    Timeout,
    Ambiguous,
    Interrupted,
}

impl OperatorOutcome {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Yes => "yes",
            Self::No => "no",
            Self::Eof => "eof",
            Self::Timeout => "timeout",
            Self::Ambiguous => "ambiguous",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ActionMarker {
    pub(crate) lease_id: String,
    pub(crate) command: String,
    pub(crate) expected_stable_id: String,
    pub(crate) observed_stable_id: String,
    pub(crate) action_id: String,
    pub(crate) label: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ObservationRequest {
    pub(crate) marker: ActionMarker,
    pub(crate) response_timeout: Duration,
}

pub(crate) trait OperatorPort {
    fn announce(&mut self, marker: &ActionMarker) -> Result<(), String>;

    fn observe(
        &mut self,
        request: &ObservationRequest,
        interrupt: &InterruptToken,
    ) -> Result<OperatorOutcome, String>;
}

pub(crate) struct ConsoleOperatorPort {
    interactive: bool,
}

impl ConsoleOperatorPort {
    pub(crate) fn new() -> Self {
        Self {
            interactive: io::stdin().is_terminal(),
        }
    }

    #[cfg(test)]
    fn with_interactive(interactive: bool) -> Self {
        Self { interactive }
    }
}

impl OperatorPort for ConsoleOperatorPort {
    fn announce(&mut self, marker: &ActionMarker) -> Result<(), String> {
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        writeln!(
            stderr,
            "ACTION run={} device={} action={} label={}",
            marker.lease_id, marker.observed_stable_id, marker.action_id, marker.label
        )
        .map_err(|error| format!("cannot write operator marker: {error}"))?;
        stderr
            .flush()
            .map_err(|error| format!("cannot flush operator marker: {error}"))
    }

    fn observe(
        &mut self,
        request: &ObservationRequest,
        interrupt: &InterruptToken,
    ) -> Result<OperatorOutcome, String> {
        if interrupt.is_requested() {
            return Ok(OperatorOutcome::Interrupted);
        }
        if !self.interactive {
            return Ok(OperatorOutcome::Eof);
        }

        while event::poll(Duration::ZERO)
            .map_err(|error| format!("cannot drain operator console: {error}"))?
        {
            let pending = event::read()
                .map_err(|error| format!("cannot drain operator console event: {error}"))?;
            if pending.matches_key_press(KeyCode::Char('c'), KeyModifiers::CONTROL)
                || pending.matches_key_press(KeyCode::Char('C'), KeyModifiers::CONTROL)
            {
                interrupt.request();
                return Ok(OperatorOutcome::Interrupted);
            }
        }
        let stderr = io::stderr();
        let mut stderr = stderr.lock();
        writeln!(
            stderr,
            "OBSERVE run={} action={} response=Y/N",
            request.marker.lease_id, request.marker.action_id
        )
        .map_err(|error| format!("cannot write operator prompt: {error}"))?;
        stderr
            .flush()
            .map_err(|error| format!("cannot flush operator prompt: {error}"))?;

        let started = Instant::now();
        loop {
            if interrupt.is_requested() {
                return Ok(OperatorOutcome::Interrupted);
            }
            let Some(remaining) = request.response_timeout.checked_sub(started.elapsed()) else {
                return Ok(OperatorOutcome::Timeout);
            };
            if remaining.is_zero() {
                return Ok(OperatorOutcome::Timeout);
            }
            if !event::poll(remaining.min(OPERATOR_POLL_SLICE))
                .map_err(|error| format!("cannot poll operator console: {error}"))?
            {
                continue;
            }
            let input = event::read()
                .map_err(|error| format!("cannot read operator console event: {error}"))?;
            if interrupt.is_requested() {
                return Ok(OperatorOutcome::Interrupted);
            }
            let Event::Key(key) = input else {
                continue;
            };
            if !matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                continue;
            }
            if Event::Key(key).matches_key_press(KeyCode::Char('c'), KeyModifiers::CONTROL)
                || Event::Key(key).matches_key_press(KeyCode::Char('C'), KeyModifiers::CONTROL)
            {
                interrupt.request();
                return Ok(OperatorOutcome::Interrupted);
            }
            return Ok(match key.code {
                KeyCode::Char('y' | 'Y') => OperatorOutcome::Yes,
                KeyCode::Char('n' | 'N') => OperatorOutcome::No,
                _ => OperatorOutcome::Ambiguous,
            });
        }
    }
}

trait ConsoleEventExt {
    fn matches_key_press(&self, code: KeyCode, modifiers: KeyModifiers) -> bool;
}

impl ConsoleEventExt for Event {
    fn matches_key_press(&self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        matches!(
            self,
            Event::Key(key)
                if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                    && key.code == code
                    && key.modifiers.contains(modifiers)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ObservationRequest {
        ObservationRequest {
            marker: ActionMarker {
                lease_id: "lease".to_owned(),
                command: "smoke".to_owned(),
                expected_stable_id: "DEVICE\\EXPECTED".to_owned(),
                observed_stable_id: "DEVICE\\EXPECTED".to_owned(),
                action_id: "button.A".to_owned(),
                label: "Button A".to_owned(),
            },
            response_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn noninteractive_console_is_eof_without_a_reader_thread() {
        let mut port = ConsoleOperatorPort::with_interactive(false);
        assert_eq!(
            port.observe(&request(), &InterruptToken::default()),
            Ok(OperatorOutcome::Eof)
        );
    }

    #[test]
    fn interrupt_preempts_noninteractive_eof() {
        let token = InterruptToken::default();
        token.request();
        let mut port = ConsoleOperatorPort::with_interactive(false);
        assert_eq!(
            port.observe(&request(), &token),
            Ok(OperatorOutcome::Interrupted)
        );
    }
}
