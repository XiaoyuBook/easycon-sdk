use std::collections::BTreeMap;

use easycon_serial::{SerialDiscovery, SerialError, SerialPortDescriptor, WindowsSerialDiscovery};

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
    use easycon_serial::UsbIdentifiers;

    use super::*;

    fn descriptor(stable_id: &str, port: &str) -> SerialPortDescriptor {
        SerialPortDescriptor::new(stable_id, port).expect("descriptor")
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
}
