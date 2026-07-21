use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use easycon_serial::{
    ByteIo, ByteIoFactory, ByteIoRequest, SerialDiscovery, SerialError, SerialErrorKind,
    SerialPortDescriptor, WindowsSerialDiscovery,
};

pub(super) trait DeviceDiscovery: Send + Sync {
    fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError>;
}

pub(super) struct SystemDeviceDiscovery;

impl DeviceDiscovery for SystemDeviceDiscovery {
    fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError> {
        WindowsSerialDiscovery.discover()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DeviceTargetRequest {
    expected_stable_id: String,
    initial_port_hint: String,
}

impl DeviceTargetRequest {
    pub(super) fn new(
        expected_stable_id: impl Into<String>,
        initial_port_hint: impl Into<String>,
    ) -> Result<Self, String> {
        let expected_stable_id = expected_stable_id.into();
        let initial_port_hint = initial_port_hint.into();
        if expected_stable_id.trim().is_empty() {
            return Err("--expected-identity must be non-empty".to_owned());
        }
        if !valid_com_port_name(&initial_port_hint) {
            return Err("--port must be a COM name such as COM8".to_owned());
        }
        Ok(Self {
            expected_stable_id,
            initial_port_hint,
        })
    }

    pub(super) fn expected_stable_id(&self) -> &str {
        &self.expected_stable_id
    }

    pub(super) fn initial_port_hint(&self) -> &str {
        &self.initial_port_hint
    }
}

fn valid_com_port_name(value: &str) -> bool {
    value
        .get(..3)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("COM"))
        && value.get(3..).is_some_and(|suffix| {
            !suffix.is_empty()
                && suffix.bytes().all(|value| value.is_ascii_digit())
                && suffix.parse::<u32>().is_ok_and(|number| number != 0)
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AdmissionReason {
    ExpectedAbsent,
    ExpectedAtDifferentPort,
    HintOwnedByDifferentIdentity,
    AmbiguousSnapshot,
}

impl AdmissionReason {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::ExpectedAbsent => "expected_absent",
            Self::ExpectedAtDifferentPort => "expected_at_different_port",
            Self::HintOwnedByDifferentIdentity => "hint_owned_by_different_identity",
            Self::AmbiguousSnapshot => "ambiguous_snapshot",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct AdmissionEvidence {
    request: DeviceTargetRequest,
    snapshot: Vec<SerialPortDescriptor>,
    reason: Option<AdmissionReason>,
    expected: Option<SerialPortDescriptor>,
    hint: Option<SerialPortDescriptor>,
}

impl AdmissionEvidence {
    pub(super) fn request(&self) -> &DeviceTargetRequest {
        &self.request
    }

    pub(super) fn snapshot(&self) -> &[SerialPortDescriptor] {
        &self.snapshot
    }

    pub(super) const fn reason(&self) -> Option<AdmissionReason> {
        self.reason
    }

    pub(super) fn expected(&self) -> Option<&SerialPortDescriptor> {
        self.expected.as_ref()
    }

    pub(super) fn hint(&self) -> Option<&SerialPortDescriptor> {
        self.hint.as_ref()
    }
}

#[derive(Clone, Debug)]
pub(super) struct AdmittedDevice {
    descriptor: SerialPortDescriptor,
    evidence: AdmissionEvidence,
}

impl AdmittedDevice {
    pub(super) fn descriptor(&self) -> &SerialPortDescriptor {
        &self.descriptor
    }

    pub(super) fn request(&self) -> &DeviceTargetRequest {
        self.evidence.request()
    }

    pub(super) fn evidence(&self) -> &AdmissionEvidence {
        &self.evidence
    }
}

#[derive(Clone, Debug)]
pub(super) enum AdmissionDecision {
    Admitted(AdmittedDevice),
    Rejected(AdmissionEvidence),
    Ambiguous(AdmissionEvidence),
}

pub(super) fn admit_device(
    discovery: &dyn DeviceDiscovery,
    request: DeviceTargetRequest,
) -> Result<AdmissionDecision, SerialError> {
    let snapshot = discovery.discover()?;
    Ok(select_device(request, snapshot))
}

pub(super) fn select_device(
    request: DeviceTargetRequest,
    snapshot: Vec<SerialPortDescriptor>,
) -> AdmissionDecision {
    if snapshot_is_ambiguous(&snapshot) {
        return AdmissionDecision::Ambiguous(AdmissionEvidence {
            request,
            snapshot,
            reason: Some(AdmissionReason::AmbiguousSnapshot),
            expected: None,
            hint: None,
        });
    }

    let expected = snapshot
        .iter()
        .find(|descriptor| descriptor.stable_id() == request.expected_stable_id())
        .cloned();
    let hint = snapshot
        .iter()
        .find(|descriptor| {
            descriptor
                .port_name()
                .eq_ignore_ascii_case(request.initial_port_hint())
        })
        .cloned();

    if let Some(descriptor) = expected.as_ref()
        && descriptor
            .port_name()
            .eq_ignore_ascii_case(request.initial_port_hint())
    {
        return AdmissionDecision::Admitted(AdmittedDevice {
            descriptor: descriptor.clone(),
            evidence: AdmissionEvidence {
                request,
                snapshot,
                reason: None,
                expected,
                hint,
            },
        });
    }

    let reason = if expected.is_some() {
        AdmissionReason::ExpectedAtDifferentPort
    } else if hint.is_some() {
        AdmissionReason::HintOwnedByDifferentIdentity
    } else {
        AdmissionReason::ExpectedAbsent
    };
    AdmissionDecision::Rejected(AdmissionEvidence {
        request,
        snapshot,
        reason: Some(reason),
        expected,
        hint,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum OpenGuardStatus {
    Matched,
    ExpectedAbsent,
    ExpectedAtDifferentPort,
    HintOwnedByDifferentIdentity,
    AmbiguousSnapshot,
    DiscoveryError,
    DescriptorMismatch,
    Interrupted,
}

impl OpenGuardStatus {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Matched => "matched",
            Self::ExpectedAbsent => "expected_absent",
            Self::ExpectedAtDifferentPort => "expected_at_different_port",
            Self::HintOwnedByDifferentIdentity => "hint_owned_by_different_identity",
            Self::AmbiguousSnapshot => "ambiguous_snapshot",
            Self::DiscoveryError => "discovery_error",
            Self::DescriptorMismatch => "descriptor_mismatch",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct OpenGuardCheck {
    pub(super) status: OpenGuardStatus,
    pub(super) snapshot: Vec<SerialPortDescriptor>,
    pub(super) expected: Option<SerialPortDescriptor>,
    pub(super) hint: Option<SerialPortDescriptor>,
    pub(super) error: Option<SerialError>,
}

#[derive(Clone, Debug)]
pub(super) enum InnerOpenOutcome {
    NotAttempted,
    Opened,
    Failed(SerialError),
}

#[derive(Clone, Debug)]
pub(super) struct IdentityOpenAttempt {
    pub(super) baud: u32,
    pub(super) pre_open: OpenGuardCheck,
    pub(super) inner_open: InnerOpenOutcome,
    pub(super) post_open: Option<OpenGuardCheck>,
    pub(super) stream_returned: bool,
}

#[derive(Clone, Default)]
pub(super) struct IdentityOpenRecorder {
    attempts: Arc<Mutex<Vec<IdentityOpenAttempt>>>,
}

impl IdentityOpenRecorder {
    pub(super) fn attempts(&self) -> Vec<IdentityOpenAttempt> {
        self.attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    fn record(&self, attempt: IdentityOpenAttempt) {
        self.attempts
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(attempt);
    }
}

pub(super) struct IdentityGuardedByteIoFactory {
    inner: Box<dyn ByteIoFactory>,
    discovery: Arc<dyn DeviceDiscovery>,
    target: AdmittedDevice,
    recorder: IdentityOpenRecorder,
}

impl IdentityGuardedByteIoFactory {
    pub(super) fn new(
        inner: Box<dyn ByteIoFactory>,
        discovery: Arc<dyn DeviceDiscovery>,
        target: AdmittedDevice,
        recorder: IdentityOpenRecorder,
    ) -> Self {
        Self {
            inner,
            discovery,
            target,
            recorder,
        }
    }

    fn check_mapping(&self, port: &SerialPortDescriptor) -> OpenGuardCheck {
        if port.stable_id() != self.target.descriptor().stable_id()
            || !port
                .port_name()
                .eq_ignore_ascii_case(self.target.descriptor().port_name())
        {
            return OpenGuardCheck {
                status: OpenGuardStatus::DescriptorMismatch,
                snapshot: Vec::new(),
                expected: None,
                hint: None,
                error: None,
            };
        }

        let snapshot = match self.discovery.discover() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return OpenGuardCheck {
                    status: OpenGuardStatus::DiscoveryError,
                    snapshot: Vec::new(),
                    expected: None,
                    hint: None,
                    error: Some(error),
                };
            }
        };
        let request =
            DeviceTargetRequest::new(self.target.request().expected_stable_id(), port.port_name())
                .expect("admitted descriptor retains a valid COM name");
        match select_device(request, snapshot) {
            AdmissionDecision::Admitted(target) => OpenGuardCheck {
                status: OpenGuardStatus::Matched,
                snapshot: target.evidence().snapshot().to_vec(),
                expected: Some(target.descriptor().clone()),
                hint: Some(target.descriptor().clone()),
                error: None,
            },
            AdmissionDecision::Rejected(evidence) => OpenGuardCheck {
                status: match evidence.reason().expect("rejection has a reason") {
                    AdmissionReason::ExpectedAbsent => OpenGuardStatus::ExpectedAbsent,
                    AdmissionReason::ExpectedAtDifferentPort => {
                        OpenGuardStatus::ExpectedAtDifferentPort
                    }
                    AdmissionReason::HintOwnedByDifferentIdentity => {
                        OpenGuardStatus::HintOwnedByDifferentIdentity
                    }
                    AdmissionReason::AmbiguousSnapshot => OpenGuardStatus::AmbiguousSnapshot,
                },
                snapshot: evidence.snapshot().to_vec(),
                expected: evidence.expected().cloned(),
                hint: evidence.hint().cloned(),
                error: None,
            },
            AdmissionDecision::Ambiguous(evidence) => OpenGuardCheck {
                status: OpenGuardStatus::AmbiguousSnapshot,
                snapshot: evidence.snapshot().to_vec(),
                expected: evidence.expected().cloned(),
                hint: evidence.hint().cloned(),
                error: None,
            },
        }
    }
}

impl ByteIoFactory for IdentityGuardedByteIoFactory {
    fn open(
        &mut self,
        port: &SerialPortDescriptor,
        baud_rate: u32,
        request: ByteIoRequest,
    ) -> Result<Box<dyn ByteIo>, SerialError> {
        if let Some(error) = request.interruption() {
            self.recorder.record(IdentityOpenAttempt {
                baud: baud_rate,
                pre_open: OpenGuardCheck {
                    status: OpenGuardStatus::Interrupted,
                    snapshot: Vec::new(),
                    expected: None,
                    hint: None,
                    error: Some(error.clone()),
                },
                inner_open: InnerOpenOutcome::NotAttempted,
                post_open: None,
                stream_returned: false,
            });
            return Err(error);
        }

        let pre_open = self.check_mapping(port);
        if pre_open.status != OpenGuardStatus::Matched {
            let error = guard_error(&pre_open, "pre-open");
            self.recorder.record(IdentityOpenAttempt {
                baud: baud_rate,
                pre_open,
                inner_open: InnerOpenOutcome::NotAttempted,
                post_open: None,
                stream_returned: false,
            });
            return Err(error);
        }

        let mut stream = match self.inner.open(port, baud_rate, request) {
            Ok(stream) => stream,
            Err(error) => {
                self.recorder.record(IdentityOpenAttempt {
                    baud: baud_rate,
                    pre_open,
                    inner_open: InnerOpenOutcome::Failed(error.clone()),
                    post_open: None,
                    stream_returned: false,
                });
                return Err(error);
            }
        };
        let post_open = self.check_mapping(port);
        if post_open.status != OpenGuardStatus::Matched {
            stream.close();
            let error = guard_error(&post_open, "post-open");
            self.recorder.record(IdentityOpenAttempt {
                baud: baud_rate,
                pre_open,
                inner_open: InnerOpenOutcome::Opened,
                post_open: Some(post_open),
                stream_returned: false,
            });
            return Err(error);
        }

        self.recorder.record(IdentityOpenAttempt {
            baud: baud_rate,
            pre_open,
            inner_open: InnerOpenOutcome::Opened,
            post_open: Some(post_open),
            stream_returned: true,
        });
        Ok(stream)
    }
}

fn guard_error(check: &OpenGuardCheck, phase: &str) -> SerialError {
    if let Some(error) = &check.error {
        return error.clone();
    }
    let kind = if check.status == OpenGuardStatus::AmbiguousSnapshot {
        SerialErrorKind::Io
    } else {
        SerialErrorKind::InvalidPort
    };
    SerialError::new(
        kind,
        format!("identity {phase} guard failed: {}", check.status.as_str()),
    )
}

fn snapshot_is_ambiguous(snapshot: &[SerialPortDescriptor]) -> bool {
    let mut identities = BTreeMap::<&str, usize>::new();
    let mut ports = BTreeMap::<String, usize>::new();
    for descriptor in snapshot {
        *identities.entry(descriptor.stable_id()).or_default() += 1;
        *ports
            .entry(descriptor.port_name().to_ascii_uppercase())
            .or_default() += 1;
    }
    identities.values().any(|count| *count != 1) || ports.values().any(|count| *count != 1)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use easycon_runtime::{CancellationToken, SystemClock};
    use easycon_serial::{ByteIoOperation, UsbIdentifiers};

    use super::*;

    fn descriptor(stable_id: &str, port: &str) -> SerialPortDescriptor {
        SerialPortDescriptor::new(stable_id, port).expect("descriptor")
    }

    fn admitted_target() -> AdmittedDevice {
        let request = DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request");
        let AdmissionDecision::Admitted(target) =
            select_device(request, vec![descriptor("DEVICE\\EXPECTED", "COM8")])
        else {
            panic!("target must be admitted");
        };
        target
    }

    #[derive(Default)]
    struct IoTrace {
        opens: Vec<u32>,
        writes: usize,
        closes: usize,
    }

    type SharedIoTrace = Arc<Mutex<IoTrace>>;

    struct FakeByteIo {
        trace: SharedIoTrace,
    }

    impl ByteIo for FakeByteIo {
        fn read(
            &mut self,
            _buffer: &mut [u8],
            _request: ByteIoRequest,
        ) -> Result<usize, SerialError> {
            Err(SerialError::new(
                SerialErrorKind::Protocol,
                "read is not scripted",
            ))
        }

        fn write(&mut self, buffer: &[u8], _request: ByteIoRequest) -> Result<usize, SerialError> {
            self.trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .writes += 1;
            Ok(buffer.len())
        }

        fn discard_input(&mut self, _request: ByteIoRequest) -> Result<(), SerialError> {
            Ok(())
        }

        fn close(&mut self) {
            self.trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .closes += 1;
        }
    }

    struct FakeByteIoFactory {
        trace: SharedIoTrace,
        error: Option<SerialError>,
    }

    impl ByteIoFactory for FakeByteIoFactory {
        fn open(
            &mut self,
            _port: &SerialPortDescriptor,
            baud_rate: u32,
            _request: ByteIoRequest,
        ) -> Result<Box<dyn ByteIo>, SerialError> {
            self.trace
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .opens
                .push(baud_rate);
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            Ok(Box::new(FakeByteIo {
                trace: Arc::clone(&self.trace),
            }))
        }
    }

    struct ScriptedDiscovery {
        results: Mutex<VecDeque<Result<Vec<SerialPortDescriptor>, SerialError>>>,
        calls: AtomicUsize,
    }

    impl ScriptedDiscovery {
        fn new(results: Vec<Result<Vec<SerialPortDescriptor>, SerialError>>) -> Self {
            Self {
                results: Mutex::new(results.into()),
                calls: AtomicUsize::new(0),
            }
        }
    }

    impl DeviceDiscovery for ScriptedDiscovery {
        fn discover(&self) -> Result<Vec<SerialPortDescriptor>, SerialError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pop_front()
                .expect("scripted discovery result")
        }
    }

    fn open_request() -> ByteIoRequest {
        ByteIoRequest {
            operation: ByteIoOperation::Open,
            clock: Arc::new(SystemClock::default()),
            deadline_ns: u64::MAX,
            cancellation: CancellationToken::root(),
            resource_cancellation: CancellationToken::root(),
        }
    }

    fn guarded_factory(
        discovery: Arc<ScriptedDiscovery>,
        trace: SharedIoTrace,
        inner_error: Option<SerialError>,
    ) -> (IdentityGuardedByteIoFactory, IdentityOpenRecorder) {
        let recorder = IdentityOpenRecorder::default();
        let factory = IdentityGuardedByteIoFactory::new(
            Box::new(FakeByteIoFactory {
                trace,
                error: inner_error,
            }),
            discovery,
            admitted_target(),
            recorder.clone(),
        );
        (factory, recorder)
    }

    #[test]
    fn selector_never_uses_the_hint_when_expected_is_on_another_port() {
        let request = DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request");
        let decision = select_device(
            request,
            vec![
                descriptor("DEVICE\\OTHER", "COM8"),
                descriptor("DEVICE\\EXPECTED", "COM11"),
            ],
        );

        let AdmissionDecision::Rejected(evidence) = decision else {
            panic!("expected rejection");
        };
        assert_eq!(
            evidence.reason(),
            Some(AdmissionReason::ExpectedAtDifferentPort)
        );
        assert_eq!(
            evidence.expected().map(SerialPortDescriptor::port_name),
            Some("COM11")
        );
        assert_eq!(
            evidence.hint().map(SerialPortDescriptor::stable_id),
            Some("DEVICE\\OTHER")
        );
    }

    #[test]
    fn selector_distinguishes_absence_hint_conflict_and_exact_admission() {
        let absent = select_device(
            DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request"),
            Vec::new(),
        );
        assert!(matches!(
            absent,
            AdmissionDecision::Rejected(AdmissionEvidence {
                reason: Some(AdmissionReason::ExpectedAbsent),
                ..
            })
        ));

        let conflict = select_device(
            DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request"),
            vec![descriptor("DEVICE\\OTHER", "COM8")],
        );
        assert!(matches!(
            conflict,
            AdmissionDecision::Rejected(AdmissionEvidence {
                reason: Some(AdmissionReason::HintOwnedByDifferentIdentity),
                ..
            })
        ));

        let admitted = select_device(
            DeviceTargetRequest::new("DEVICE\\EXPECTED", "com8").expect("request"),
            vec![descriptor("DEVICE\\EXPECTED", "COM8")],
        );
        let AdmissionDecision::Admitted(admitted) = admitted else {
            panic!("expected admission");
        };
        assert_eq!(admitted.descriptor().port_name(), "COM8");
    }

    #[test]
    fn identity_is_exact_and_optional_labels_cannot_substitute_for_it() {
        let lookalike = descriptor("device\\expected", "COM8")
            .with_friendly_name("DEVICE\\EXPECTED")
            .with_usb_identifiers(UsbIdentifiers {
                vid: 0x1a86,
                pid: 0xfe0c,
            });
        let decision = select_device(
            DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request"),
            vec![lookalike],
        );
        let AdmissionDecision::Rejected(evidence) = decision else {
            panic!("case-changed identity must be rejected");
        };
        assert_eq!(
            evidence.reason(),
            Some(AdmissionReason::HintOwnedByDifferentIdentity)
        );
    }

    #[test]
    fn duplicate_identity_or_port_mapping_is_ambiguous() {
        for snapshot in [
            vec![
                descriptor("DEVICE\\EXPECTED", "COM8"),
                descriptor("DEVICE\\EXPECTED", "COM11"),
            ],
            vec![
                descriptor("DEVICE\\EXPECTED", "COM8"),
                descriptor("DEVICE\\OTHER", "com8"),
            ],
        ] {
            let decision = select_device(
                DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").expect("request"),
                snapshot,
            );
            assert!(matches!(decision, AdmissionDecision::Ambiguous(_)));
        }
    }

    #[test]
    fn target_request_rejects_missing_identity_and_non_com_hint() {
        assert!(DeviceTargetRequest::new(" ", "COM8").is_err());
        assert!(DeviceTargetRequest::new("DEVICE\\EXPECTED", "ttyS0").is_err());
        assert!(DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM0").is_err());
        assert!(DeviceTargetRequest::new("DEVICE\\EXPECTED", "COM8").is_ok());
    }

    #[test]
    fn pre_open_mismatch_never_calls_the_inner_factory() {
        let discovery = Arc::new(ScriptedDiscovery::new(vec![Ok(vec![descriptor(
            "DEVICE\\OTHER",
            "COM8",
        )])]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) = guarded_factory(discovery, Arc::clone(&trace), None);
        let port = descriptor("DEVICE\\EXPECTED", "COM8");

        let error = factory
            .open(&port, 115_200, open_request())
            .err()
            .expect("mismatch must reject open");

        assert_eq!(error.kind(), SerialErrorKind::InvalidPort);
        assert!(trace.lock().expect("trace").opens.is_empty());
        let attempts = recorder.attempts();
        assert_eq!(attempts.len(), 1);
        assert_eq!(
            attempts[0].pre_open.status,
            OpenGuardStatus::HintOwnedByDifferentIdentity
        );
        assert!(attempts[0].post_open.is_none());
        assert!(!attempts[0].stream_returned);
    }

    #[test]
    fn pre_open_discovery_error_never_opens_and_preserves_native_detail() {
        let discovery_error = SerialError::with_os_code(
            SerialErrorKind::Io,
            "injected pre-open discovery failure",
            31,
        );
        let discovery = Arc::new(ScriptedDiscovery::new(vec![Err(discovery_error.clone())]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) = guarded_factory(discovery, Arc::clone(&trace), None);

        let error = factory
            .open(
                &descriptor("DEVICE\\EXPECTED", "COM8"),
                115_200,
                open_request(),
            )
            .err()
            .expect("pre-open discovery failure");

        assert_eq!(error, discovery_error);
        assert!(trace.lock().expect("trace").opens.is_empty());
        let attempts = recorder.attempts();
        assert_eq!(attempts[0].pre_open.status, OpenGuardStatus::DiscoveryError);
        let observed = attempts[0]
            .pre_open
            .error
            .as_ref()
            .expect("structured discovery error");
        assert_eq!(observed.kind(), SerialErrorKind::Io);
        assert_eq!(observed.os_code(), Some(31));
        assert!(attempts[0].post_open.is_none());
    }

    #[test]
    fn descriptor_mismatch_is_rejected_before_discovery_or_open() {
        let discovery = Arc::new(ScriptedDiscovery::new(Vec::new()));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) =
            guarded_factory(Arc::clone(&discovery), Arc::clone(&trace), None);

        let error = factory
            .open(
                &descriptor("DEVICE\\EXPECTED", "COM11"),
                115_200,
                open_request(),
            )
            .err()
            .expect("descriptor mismatch");

        assert_eq!(error.kind(), SerialErrorKind::InvalidPort);
        assert_eq!(discovery.calls.load(Ordering::SeqCst), 0);
        assert!(trace.lock().expect("trace").opens.is_empty());
        assert_eq!(
            recorder.attempts()[0].pre_open.status,
            OpenGuardStatus::DescriptorMismatch
        );
    }

    #[test]
    fn post_open_mismatch_closes_the_stream_before_returning() {
        let matched = vec![descriptor("DEVICE\\EXPECTED", "COM8")];
        let mismatched = vec![descriptor("DEVICE\\OTHER", "COM8")];
        let discovery = Arc::new(ScriptedDiscovery::new(vec![Ok(matched), Ok(mismatched)]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) = guarded_factory(discovery, Arc::clone(&trace), None);
        let port = descriptor("DEVICE\\EXPECTED", "COM8");

        let error = factory
            .open(&port, 115_200, open_request())
            .err()
            .expect("post-open mismatch must reject stream");

        assert_eq!(error.kind(), SerialErrorKind::InvalidPort);
        let trace = trace.lock().expect("trace");
        assert_eq!(trace.opens, vec![115_200]);
        assert_eq!(trace.closes, 1);
        assert_eq!(trace.writes, 0);
        let attempts = recorder.attempts();
        assert_eq!(
            attempts[0]
                .post_open
                .as_ref()
                .expect("post-open evidence")
                .status,
            OpenGuardStatus::HintOwnedByDifferentIdentity
        );
        assert!(!attempts[0].stream_returned);
    }

    #[test]
    fn post_open_discovery_error_closes_and_preserves_native_detail() {
        let discovery_error = SerialError::with_os_code(
            SerialErrorKind::Io,
            "injected post-open discovery failure",
            31,
        );
        let discovery = Arc::new(ScriptedDiscovery::new(vec![
            Ok(vec![descriptor("DEVICE\\EXPECTED", "COM8")]),
            Err(discovery_error.clone()),
        ]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) = guarded_factory(discovery, Arc::clone(&trace), None);

        let error = factory
            .open(
                &descriptor("DEVICE\\EXPECTED", "COM8"),
                115_200,
                open_request(),
            )
            .err()
            .expect("post-open discovery failure");

        assert_eq!(error, discovery_error);
        assert_eq!(trace.lock().expect("trace").closes, 1);
        let attempts = recorder.attempts();
        let observed = attempts[0]
            .post_open
            .as_ref()
            .and_then(|check| check.error.as_ref())
            .expect("structured discovery error");
        assert_eq!(observed.kind(), SerialErrorKind::Io);
        assert_eq!(observed.os_code(), Some(31));
    }

    #[test]
    fn successful_guard_returns_a_writable_stream_after_both_checks() {
        let matched = || Ok(vec![descriptor("DEVICE\\EXPECTED", "COM8")]);
        let discovery = Arc::new(ScriptedDiscovery::new(vec![matched(), matched()]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) =
            guarded_factory(Arc::clone(&discovery), Arc::clone(&trace), None);

        let mut stream = factory
            .open(
                &descriptor("DEVICE\\EXPECTED", "COM8"),
                115_200,
                open_request(),
            )
            .expect("guarded stream");
        assert_eq!(stream.write(&[1, 2, 3], open_request()), Ok(3));
        stream.close();

        assert_eq!(discovery.calls.load(Ordering::SeqCst), 2);
        let trace = trace.lock().expect("trace");
        assert_eq!(trace.opens, vec![115_200]);
        assert_eq!(trace.writes, 1);
        assert_eq!(trace.closes, 1);
        let attempts = recorder.attempts();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].pre_open.status, OpenGuardStatus::Matched);
        assert_eq!(
            attempts[0].post_open.as_ref().map(|check| check.status),
            Some(OpenGuardStatus::Matched)
        );
        assert!(attempts[0].stream_returned);
    }

    #[test]
    fn every_baud_attempt_repeats_both_identity_checks() {
        let matched = || Ok(vec![descriptor("DEVICE\\EXPECTED", "COM8")]);
        let discovery = Arc::new(ScriptedDiscovery::new(vec![
            matched(),
            matched(),
            matched(),
            matched(),
        ]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) =
            guarded_factory(Arc::clone(&discovery), Arc::clone(&trace), None);
        let port = descriptor("DEVICE\\EXPECTED", "COM8");

        let mut first = factory
            .open(&port, 115_200, open_request())
            .expect("first guarded stream");
        first.close();
        let mut second = factory
            .open(&port, 9_600, open_request())
            .expect("second guarded stream");
        second.close();

        assert_eq!(discovery.calls.load(Ordering::SeqCst), 4);
        assert_eq!(trace.lock().expect("trace").opens, vec![115_200, 9_600]);
        let attempts = recorder.attempts();
        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().all(|attempt| {
            attempt.pre_open.status == OpenGuardStatus::Matched
                && attempt
                    .post_open
                    .as_ref()
                    .is_some_and(|check| check.status == OpenGuardStatus::Matched)
                && attempt.stream_returned
        }));
    }

    #[test]
    fn cancellation_and_deadline_do_not_discover_or_open() {
        let discovery = Arc::new(ScriptedDiscovery::new(Vec::new()));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let (mut factory, recorder) =
            guarded_factory(Arc::clone(&discovery), Arc::clone(&trace), None);
        let port = descriptor("DEVICE\\EXPECTED", "COM8");
        let cancelled = CancellationToken::root();
        cancelled.cancel();
        let mut cancelled_request = open_request();
        cancelled_request.cancellation = cancelled;
        let error = factory
            .open(&port, 115_200, cancelled_request)
            .err()
            .expect("cancelled guard");
        assert_eq!(error.kind(), SerialErrorKind::Cancelled);

        let mut deadline_request = open_request();
        deadline_request.deadline_ns = 0;
        let error = factory
            .open(&port, 9_600, deadline_request)
            .err()
            .expect("elapsed guard deadline");
        assert_eq!(error.kind(), SerialErrorKind::DeadlineExceeded);

        assert_eq!(discovery.calls.load(Ordering::SeqCst), 0);
        assert!(trace.lock().expect("trace").opens.is_empty());
        assert_eq!(recorder.attempts().len(), 2);
    }

    #[test]
    fn inner_native_open_error_is_preserved_without_a_post_check() {
        let discovery = Arc::new(ScriptedDiscovery::new(vec![Ok(vec![descriptor(
            "DEVICE\\EXPECTED",
            "COM8",
        )])]));
        let trace = Arc::new(Mutex::new(IoTrace::default()));
        let native_error =
            SerialError::with_os_code(SerialErrorKind::PortBusy, "injected sharing violation", 32);
        let (mut factory, recorder) =
            guarded_factory(discovery, Arc::clone(&trace), Some(native_error.clone()));

        let error = factory
            .open(
                &descriptor("DEVICE\\EXPECTED", "COM8"),
                115_200,
                open_request(),
            )
            .err()
            .expect("inner native open failure");

        assert_eq!(error, native_error);
        let attempts = recorder.attempts();
        let InnerOpenOutcome::Failed(observed) = &attempts[0].inner_open else {
            panic!("inner failure evidence");
        };
        assert_eq!(observed.kind(), SerialErrorKind::PortBusy);
        assert_eq!(observed.os_code(), Some(32));
        assert!(attempts[0].post_open.is_none());
        assert_eq!(trace.lock().expect("trace").closes, 0);
    }
}
