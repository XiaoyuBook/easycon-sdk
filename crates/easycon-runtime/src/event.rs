use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

use easycon_model::{OperationId, ResourceId};

use crate::wait::WaitTimeout;

/// Priority used by a subscription when it must shed load.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventClass {
    /// Debug, informational, and replaceable observations.
    Ordinary,
    /// State, error, and terminal observations that displace ordinary events.
    Critical,
}

/// Stable event category independent of a specific domain payload.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum EventKind {
    /// Runtime, operation, or resource state changed.
    State,
    /// An operation reached its terminal state.
    Terminal,
    /// Cleanup or degraded behavior that did not change the authoritative state.
    Warning,
    /// A target was delayed to preserve a scheduling invariant.
    TimingDeviation,
    /// Typed domain data such as an accepted report trace.
    Data,
    /// Replaceable diagnostic logging.
    Log,
    /// One or more subscription-local observations were dropped.
    Gap(EventGap),
}

/// Ordered severity filter for subscriptions.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    /// Verbose diagnostics.
    Debug,
    /// Normal progress information.
    Info,
    /// Recoverable degradation.
    Warning,
    /// Operation or resource failure.
    Error,
}

/// Summary of a contiguous or merged dropped sequence interval.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventGap {
    /// First dropped Runtime-global sequence.
    pub first_sequence: u64,
    /// Last dropped Runtime-global sequence.
    pub last_sequence: u64,
    /// Total dropped events represented by this summary.
    pub dropped_count: u64,
}

/// Immutable event copied into each matching subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Event {
    /// Runtime-global monotonically increasing sequence.
    pub sequence: u64,
    /// Nanoseconds since the Runtime clock epoch.
    pub timestamp_ns: u64,
    /// Backpressure priority.
    pub class: EventClass,
    /// Stable category.
    pub kind: EventKind,
    /// Stable machine-readable event code.
    pub code: &'static str,
    /// Event severity.
    pub severity: Severity,
    /// Related operation when applicable.
    pub operation_id: Option<OperationId>,
    /// Related resource when applicable.
    pub resource_id: Option<ResourceId>,
    /// Human-readable or trace detail that is not used for machine branching.
    pub detail: Option<Arc<str>>,
}

/// Event fields supplied by a Runtime domain before sequence/timestamp assignment.
#[derive(Clone, Debug)]
pub struct EventDraft {
    pub(crate) class: EventClass,
    pub(crate) kind: EventKind,
    pub(crate) code: &'static str,
    pub(crate) severity: Severity,
    pub(crate) operation_id: Option<OperationId>,
    pub(crate) resource_id: Option<ResourceId>,
    pub(crate) detail: Option<Arc<str>>,
}

impl EventDraft {
    /// Creates an ordinary event.
    #[must_use]
    pub const fn ordinary(kind: EventKind, code: &'static str, severity: Severity) -> Self {
        Self {
            class: EventClass::Ordinary,
            kind,
            code,
            severity,
            operation_id: None,
            resource_id: None,
            detail: None,
        }
    }

    /// Creates a critical state, error, or terminal event.
    #[must_use]
    pub const fn critical(kind: EventKind, code: &'static str, severity: Severity) -> Self {
        Self {
            class: EventClass::Critical,
            kind,
            code,
            severity,
            operation_id: None,
            resource_id: None,
            detail: None,
        }
    }

    /// Associates an operation.
    #[must_use]
    pub const fn with_operation(mut self, operation_id: OperationId) -> Self {
        self.operation_id = Some(operation_id);
        self
    }

    /// Associates a resource.
    #[must_use]
    pub const fn with_resource(mut self, resource_id: ResourceId) -> Self {
        self.resource_id = Some(resource_id);
        self
    }

    /// Adds non-authoritative detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<Arc<str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Per-subscriber queue and filter configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SubscriptionOptions {
    /// Maximum queued events, excluding one synthesized gap summary.
    pub capacity: usize,
    /// Minimum accepted severity.
    pub minimum_severity: Severity,
    /// Whether replaceable log events are included.
    pub include_logs: bool,
}

impl Default for SubscriptionOptions {
    fn default() -> Self {
        Self {
            capacity: 64,
            minimum_severity: Severity::Debug,
            include_logs: true,
        }
    }
}

/// Result of one pull from an event subscription.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubscriptionRead {
    /// An immutable event is available.
    Event(Event),
    /// The caller-side wait elapsed; the subscription remains open.
    Timeout,
    /// The Runtime closed and the queue has been drained.
    Closed,
}

/// Single-reader, bounded, pull-based event queue.
pub struct EventSubscription {
    pub(crate) inner: Arc<SubscriptionInner>,
}

pub(crate) struct SubscriptionInner {
    options: SubscriptionOptions,
    state: Mutex<QueueState>,
    changed: Condvar,
}

#[derive(Default)]
struct QueueState {
    events: VecDeque<Event>,
    pending_gap: Option<PendingGap>,
    closed: bool,
}

#[derive(Clone, Copy)]
struct PendingGap {
    first: u64,
    last: u64,
    count: u64,
    timestamp_ns: u64,
}

impl EventSubscription {
    pub(crate) fn new(options: SubscriptionOptions) -> Self {
        Self {
            inner: Arc::new(SubscriptionInner {
                options,
                state: Mutex::new(QueueState::default()),
                changed: Condvar::new(),
            }),
        }
    }

    /// Pulls one event without invoking user code from a core thread.
    #[must_use]
    pub fn read(&self, wait: WaitTimeout) -> SubscriptionRead {
        self.inner.read(wait)
    }

    /// Returns the number of queued concrete events, excluding a pending gap summary.
    #[must_use]
    pub fn queued_len(&self) -> usize {
        self.inner
            .state
            .lock()
            .expect("subscription queue lock poisoned")
            .events
            .len()
    }
}

impl SubscriptionInner {
    pub(crate) fn enqueue(&self, event: Event) {
        if event.severity < self.options.minimum_severity
            || (!self.options.include_logs && event.kind == EventKind::Log)
        {
            return;
        }

        let mut state = self.state.lock().expect("subscription queue lock poisoned");
        if state.closed {
            return;
        }

        if state.events.len() >= self.options.capacity {
            let ordinary = state
                .events
                .iter()
                .position(|queued| queued.class == EventClass::Ordinary);
            match (event.class, ordinary) {
                (_, Some(index)) => {
                    let dropped = state
                        .events
                        .remove(index)
                        .expect("event index came from the same queue");
                    record_gap(&mut state, &dropped);
                    state.events.push_back(event);
                }
                (EventClass::Critical, None) => {
                    if let Some(dropped) = state.events.pop_front() {
                        record_gap(&mut state, &dropped);
                    }
                    state.events.push_back(event);
                }
                (EventClass::Ordinary, None) => record_gap(&mut state, &event),
            }
        } else {
            state.events.push_back(event);
        }
        drop(state);
        self.changed.notify_one();
    }

    fn read(&self, wait: WaitTimeout) -> SubscriptionRead {
        let started = Instant::now();
        let mut state = self.state.lock().expect("subscription queue lock poisoned");
        loop {
            if let Some(gap) = state.pending_gap.take() {
                return SubscriptionRead::Event(Event {
                    sequence: gap.last,
                    timestamp_ns: gap.timestamp_ns,
                    class: EventClass::Critical,
                    kind: EventKind::Gap(EventGap {
                        first_sequence: gap.first,
                        last_sequence: gap.last,
                        dropped_count: gap.count,
                    }),
                    code: "runtime.event_gap",
                    severity: Severity::Warning,
                    operation_id: None,
                    resource_id: None,
                    detail: None,
                });
            }
            if let Some(event) = state.events.pop_front() {
                return SubscriptionRead::Event(event);
            }
            if state.closed {
                return SubscriptionRead::Closed;
            }

            match wait {
                WaitTimeout::Poll => return SubscriptionRead::Timeout,
                WaitTimeout::Infinite => {
                    state = self
                        .changed
                        .wait(state)
                        .expect("subscription queue lock poisoned while waiting");
                }
                WaitTimeout::For(limit) => {
                    let Some(remaining) = limit.checked_sub(started.elapsed()) else {
                        return SubscriptionRead::Timeout;
                    };
                    let (next, timed_out) = self
                        .changed
                        .wait_timeout(state, remaining)
                        .expect("subscription queue lock poisoned while waiting");
                    state = next;
                    if timed_out.timed_out()
                        && state.pending_gap.is_none()
                        && state.events.is_empty()
                    {
                        return SubscriptionRead::Timeout;
                    }
                }
            }
        }
    }

    pub(crate) fn close(&self) {
        self.state
            .lock()
            .expect("subscription queue lock poisoned")
            .closed = true;
        self.changed.notify_all();
    }
}

fn record_gap(state: &mut QueueState, dropped: &Event) {
    if let Some(gap) = &mut state.pending_gap {
        gap.first = gap.first.min(dropped.sequence);
        gap.last = gap.last.max(dropped.sequence);
        gap.count = gap.count.saturating_add(1);
        gap.timestamp_ns = gap.timestamp_ns.max(dropped.timestamp_ns);
    } else {
        state.pending_gap = Some(PendingGap {
            first: dropped.sequence,
            last: dropped.sequence,
            count: 1,
            timestamp_ns: dropped.timestamp_ns,
        });
    }
}
