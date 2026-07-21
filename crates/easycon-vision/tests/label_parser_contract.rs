use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use easycon_runtime::{CancellationToken, CloseOutcome, Runtime, VirtualClock};
use easycon_vision::{
    LabelDiagnosticCode, LabelDiagnosticSeverity, LabelMethod, LabelSource, LabelTarget,
    MAX_LABEL_DIAGNOSTICS_PER_SOURCE, MAX_LABEL_JSON_BYTES, MAX_LABEL_SOURCES, NativePool,
    NativePoolOptions, VisionLimits, build_legacy_label_registry, parse_legacy_il,
};

struct TestNative {
    runtime: Runtime,
    pool: NativePool,
    cancellation: CancellationToken,
}

impl TestNative {
    fn new() -> Self {
        let runtime = Runtime::new(Arc::new(VirtualClock::default()));
        let pool = NativePool::new(
            &runtime,
            NativePoolOptions::new(2, 16).expect("native pool limits"),
        )
        .expect("native pool");
        let cancellation = runtime.child_cancellation_token();
        Self {
            runtime,
            pool,
            cancellation,
        }
    }
}

impl Drop for TestNative {
    fn drop(&mut self) {
        self.pool.close().expect("native pool close");
        assert_eq!(self.runtime.close(), Ok(CloseOutcome::Closed));
    }
}

fn fixture_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spec/fixtures/vision/labels")
}

fn fixture(name: &str) -> Vec<u8> {
    fs::read(fixture_root().join(name)).expect("label fixture")
}

fn limits() -> VisionLimits {
    VisionLimits::try_for_images(4096, 1024, 1024, 1024 * 1024, 4 * 1024 * 1024, 4096)
        .expect("label test limits")
}

#[test]
fn valid_legacy_bmp_png_ocr_default_and_unknown_field_are_exact() {
    let native = TestNative::new();
    let cases = [
        ("valid-bmp.IL", LabelMethod::CCoeffNormed, true),
        ("valid-png.IL", LabelMethod::CCorrNormed, true),
        ("valid-ocr.IL", LabelMethod::Ocr, false),
        ("default-method.IL", LabelMethod::CCoeffNormed, true),
    ];
    for (source, expected_method, image_target) in cases {
        let report = parse_legacy_il(
            source,
            &fixture(source),
            &native.pool,
            &limits(),
            &native.cancellation,
        )
        .expect("parse operation");
        assert!(!report.has_errors(), "{source}: {:?}", report.diagnostics());
        assert!(report.diagnostics().is_empty());
        let label = report.label().expect("published label");
        assert_eq!(label.name(), source.trim_end_matches(".IL"));
        assert_eq!(label.source(), source);
        assert_eq!(label.method(), expected_method);
        assert_eq!(
            (label.range().width(), label.range().height()),
            if image_target { (4, 4) } else { (273, 77) }
        );
        match label.target() {
            LabelTarget::Image(image) if image_target => {
                assert_eq!((image.width(), image.height()), (2, 2));
            }
            LabelTarget::Text(text) if !image_target => assert_eq!(text.as_ref(), "EASCON"),
            target => panic!("unexpected target: {target:?}"),
        }
    }

    let report = parse_legacy_il(
        "unknown-field.IL",
        &fixture("unknown-field.IL"),
        &native.pool,
        &limits(),
        &native.cancellation,
    )
    .expect("unknown field parse");
    assert!(report.label().is_some());
    assert_eq!(report.diagnostics().len(), 1);
    assert_eq!(
        report.diagnostics()[0].severity(),
        LabelDiagnosticSeverity::Warning
    );
    assert_eq!(
        report.diagnostics()[0].code(),
        LabelDiagnosticCode::UnknownField
    );
    assert_eq!(report.diagnostics()[0].field(), Some("FutureField"));
}

#[test]
fn invalid_corpus_has_stable_primary_diagnostics_and_no_partial_label() {
    let native = TestNative::new();
    let cases = [
        ("unknown-method.IL", LabelDiagnosticCode::UnsupportedMethod),
        ("string-method.IL", LabelDiagnosticCode::InvalidType),
        ("fraction-roi.IL", LabelDiagnosticCode::InvalidNumber),
        ("negative-roi.IL", LabelDiagnosticCode::InvalidNumber),
        ("overflow-roi.IL", LabelDiagnosticCode::InvalidNumber),
        ("invalid-utf8.IL", LabelDiagnosticCode::InvalidUtf8),
        ("duplicate-key.IL", LabelDiagnosticCode::DuplicateKey),
        ("bad-base64.IL", LabelDiagnosticCode::InvalidBase64),
        ("missing-padding.IL", LabelDiagnosticCode::InvalidBase64),
        (
            "dimension-mismatch.IL",
            LabelDiagnosticCode::TargetDimensions,
        ),
        (
            "target-outside-range.IL",
            LabelDiagnosticCode::TargetOutsideRange,
        ),
        ("rejected.ILX", LabelDiagnosticCode::UnsupportedExtension),
    ];
    for (source, expected) in cases {
        let report = parse_legacy_il(
            source,
            &fixture(source),
            &native.pool,
            &limits(),
            &native.cancellation,
        )
        .expect("parse operation");
        assert!(report.has_errors(), "{source}");
        assert!(report.label().is_none(), "{source}");
        assert_eq!(report.diagnostics()[0].source(), source);
        assert!(
            report
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code() == expected),
            "{source}: {:?}",
            report.diagnostics()
        );
    }

    let small_limits =
        VisionLimits::try_for_images(64, 64, 64, 4096, 16_384, 1024).expect("small limits");
    let report = parse_legacy_il(
        "decoded-limit.IL",
        &fixture("decoded-limit.IL"),
        &native.pool,
        &small_limits,
        &native.cancellation,
    )
    .expect("bounded parse");
    assert_eq!(
        report.diagnostics()[0].code(),
        LabelDiagnosticCode::TargetLimit
    );
    assert!(report.label().is_none());
}

#[test]
fn registry_sorts_sources_rejects_each_duplicate_and_never_publishes_partial_state() {
    let native = TestNative::new();
    let duplicate_a = fixture("duplicates/a/shared.IL");
    let duplicate_b = fixture("duplicates/b/shared.IL");
    let duplicate_sources = [
        LabelSource::new("duplicates/b/shared.IL", &duplicate_b),
        LabelSource::new("duplicates/a/shared.IL", &duplicate_a),
    ];
    let report = build_legacy_label_registry(
        &duplicate_sources,
        &native.pool,
        &limits(),
        &native.cancellation,
    )
    .expect("registry build");
    assert!(report.registry().is_none());
    let duplicates = report
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.code() == LabelDiagnosticCode::DuplicateName)
        .collect::<Vec<_>>();
    assert_eq!(duplicates.len(), 2);
    assert_eq!(duplicates[0].source(), "duplicates/a/shared.IL");
    assert_eq!(duplicates[1].source(), "duplicates/b/shared.IL");

    let png = fixture("valid-png.IL");
    let ocr = fixture("valid-ocr.IL");
    let sources = [
        LabelSource::new("z/valid-ocr.IL", &ocr),
        LabelSource::new("a/valid-png.IL", &png),
    ];
    let report =
        build_legacy_label_registry(&sources, &native.pool, &limits(), &native.cancellation)
            .expect("valid registry");
    assert!(!report.has_errors());
    let registry = report.registry().expect("published registry");
    assert_eq!(registry.len(), 2);
    assert_eq!(
        registry
            .iter()
            .map(|label| label.source())
            .collect::<Vec<_>>(),
        ["a/valid-png.IL", "z/valid-ocr.IL"]
    );
    assert_eq!(
        registry.get("valid-png").expect("lookup").method(),
        LabelMethod::CCorrNormed
    );
}

#[test]
fn parser_fuzz_seeds_and_hard_resource_limits_are_bounded() {
    let native = TestNative::new();
    for (index, name) in [
        "fuzz/truncated-json.seed",
        "fuzz/deep-unknown.seed",
        "fuzz/random-bytes.seed",
    ]
    .into_iter()
    .enumerate()
    {
        let report = parse_legacy_il(
            &format!("seed-{index}.IL"),
            &fixture(name),
            &native.pool,
            &limits(),
            &native.cancellation,
        )
        .expect("fuzz seed parse");
        assert!(report.has_errors());
        assert!(report.diagnostics().len() <= MAX_LABEL_DIAGNOSTICS_PER_SOURCE);
    }

    let oversized = vec![b' '; MAX_LABEL_JSON_BYTES + 1];
    let report = parse_legacy_il(
        "oversized.IL",
        &oversized,
        &native.pool,
        &limits(),
        &native.cancellation,
    )
    .expect("oversized parse");
    assert_eq!(
        report.diagnostics()[0].code(),
        LabelDiagnosticCode::InputLimit
    );

    let valid = fixture("valid-ocr.IL");
    let names = (0..=MAX_LABEL_SOURCES)
        .map(|index| format!("label-{index}.IL"))
        .collect::<Vec<_>>();
    let sources = names
        .iter()
        .map(|name| LabelSource::new(name, &valid))
        .collect::<Vec<_>>();
    let report =
        build_legacy_label_registry(&sources, &native.pool, &limits(), &native.cancellation)
            .expect("bounded registry");
    assert!(report.registry().is_none());
    assert_eq!(
        report.diagnostics()[0].code(),
        LabelDiagnosticCode::RegistryLimit
    );
}

#[test]
fn warning_saturation_cannot_hide_a_later_schema_error() {
    let native = TestNative::new();
    let unknown = (0..MAX_LABEL_DIAGNOSTICS_PER_SOURCE + 8)
        .map(|index| format!(r#""Future{index}":null"#))
        .collect::<Vec<_>>()
        .join(",");
    let bytes = format!(r#"{{{unknown},"searchMethod":"invalid"}}"#);
    let report = parse_legacy_il(
        "warning-saturation.IL",
        bytes.as_bytes(),
        &native.pool,
        &limits(),
        &native.cancellation,
    )
    .expect("bounded parse");
    assert!(report.has_errors());
    assert!(report.label().is_none());
    assert!(report.diagnostics().len() <= MAX_LABEL_DIAGNOSTICS_PER_SOURCE);
    assert!(
        report
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code() == LabelDiagnosticCode::InvalidType)
    );
}

#[test]
fn duplicate_name_errors_displace_warnings_without_exceeding_the_per_source_cap() {
    let native = TestNative::new();
    let unknown = (0..MAX_LABEL_DIAGNOSTICS_PER_SOURCE + 8)
        .map(|index| format!(r#""Future{index}":null"#))
        .collect::<Vec<_>>()
        .join(",");
    let bytes = format!(
        r#"{{{unknown},"searchMethod":107,"ImgBase64":"x","RangeX":0,"RangeY":0,"RangeWidth":1,"RangeHeight":1,"TargetX":0,"TargetY":0,"TargetWidth":1,"TargetHeight":1}}"#
    );
    let sources = [
        LabelSource::new("a/shared.IL", bytes.as_bytes()),
        LabelSource::new("b/shared.IL", bytes.as_bytes()),
    ];
    let report =
        build_legacy_label_registry(&sources, &native.pool, &limits(), &native.cancellation)
            .expect("bounded duplicate registry");
    assert!(report.registry().is_none());
    for source in ["a/shared.IL", "b/shared.IL"] {
        let diagnostics = report
            .diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.source() == source)
            .collect::<Vec<_>>();
        assert!(diagnostics.len() <= MAX_LABEL_DIAGNOSTICS_PER_SOURCE);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code() == LabelDiagnosticCode::DuplicateName)
        );
    }

    let repeated_source = [
        LabelSource::new("shared.IL", bytes.as_bytes()),
        LabelSource::new("shared.IL", bytes.as_bytes()),
    ];
    let report = build_legacy_label_registry(
        &repeated_source,
        &native.pool,
        &limits(),
        &native.cancellation,
    )
    .expect("same-source duplicate registry");
    let diagnostics = report
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.source() == "shared.IL")
        .collect::<Vec<_>>();
    assert!(diagnostics.len() <= MAX_LABEL_DIAGNOSTICS_PER_SOURCE);
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code() == LabelDiagnosticCode::DuplicateName)
    );
}

#[test]
fn invalid_base64_precedes_decoded_size_preflight() {
    let native = TestNative::new();
    let bytes = br#"{"searchMethod":5,"ImgBase64":"!!!!","RangeX":0,"RangeY":0,"RangeWidth":1,"RangeHeight":1,"TargetX":0,"TargetY":0,"TargetWidth":1,"TargetHeight":1}"#;
    let tiny_limits = VisionLimits::try_for_images(1, 16, 16, 256, 1024, 64).expect("tiny limits");
    let report = parse_legacy_il(
        "invalid-before-limit.IL",
        bytes,
        &native.pool,
        &tiny_limits,
        &native.cancellation,
    )
    .expect("bounded Base64 parse");
    assert_eq!(
        report.diagnostics()[0].code(),
        LabelDiagnosticCode::InvalidBase64
    );
}
