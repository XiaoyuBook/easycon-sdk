use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as STANDARD_BASE64;
use easycon_runtime::CancellationToken;
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::{
    EdgeMethod, Frame, Image, NativePool, OcrPool, Roi, TemplateMethod, VisionError,
    VisionErrorKind, VisionLimits,
};

pub const MAX_LABEL_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_LABEL_SOURCES: usize = 4096;
pub const MAX_LABEL_DIAGNOSTICS_PER_SOURCE: usize = 32;

const MAX_LABEL_SOURCE_BYTES: usize = 4096;
const MAX_LABEL_NAME_BYTES: usize = 255;
const MAX_LABEL_TEXT_BYTES: usize = 64 * 1024;
const MAX_LABEL_TEXT_SCALARS: usize = 4096;
const MAX_LABEL_EDIT_CELLS: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelMethod {
    SqDiffNormed,
    CCorrNormed,
    CCoeffNormed,
    EdgeXy,
    EdgeLaplacian,
    Ocr,
}

impl LabelMethod {
    fn from_legacy(value: u32) -> Option<Self> {
        match value {
            1 => Some(Self::SqDiffNormed),
            3 => Some(Self::CCorrNormed),
            5 => Some(Self::CCoeffNormed),
            11 => Some(Self::EdgeXy),
            12 => Some(Self::EdgeLaplacian),
            107 => Some(Self::Ocr),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LabelTarget {
    Image(Image),
    Text(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Label {
    name: Arc<str>,
    source: Arc<str>,
    method: LabelMethod,
    range: Roi,
    target_roi: Roi,
    target: LabelTarget,
}

impl Label {
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub const fn method(&self) -> LabelMethod {
        self.method
    }

    #[must_use]
    pub const fn range(&self) -> Roi {
        self.range
    }

    #[must_use]
    pub const fn target_roi(&self) -> Roi {
        self.target_roi
    }

    #[must_use]
    pub const fn target(&self) -> &LabelTarget {
        &self.target
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelDiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LabelDiagnosticCode {
    InputLimit,
    RegistryLimit,
    InvalidSource,
    UnsupportedExtension,
    InvalidUtf8,
    InvalidJson,
    DuplicateKey,
    UnknownField,
    InvalidType,
    InvalidNumber,
    UnsupportedMethod,
    InvalidRoi,
    RoiLimit,
    InvalidBase64,
    TargetLimit,
    TargetImage,
    TargetDimensions,
    TargetOutsideRange,
    DuplicateName,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LabelDiagnostic {
    severity: LabelDiagnosticSeverity,
    code: LabelDiagnosticCode,
    source: Arc<str>,
    field: Option<Arc<str>>,
    message: Arc<str>,
}

impl LabelDiagnostic {
    fn new(
        severity: LabelDiagnosticSeverity,
        code: LabelDiagnosticCode,
        source: Arc<str>,
        field: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity,
            code,
            source,
            field: field.map(Arc::from),
            message: Arc::from(message.into()),
        }
    }

    #[must_use]
    pub const fn severity(&self) -> LabelDiagnosticSeverity {
        self.severity
    }

    #[must_use]
    pub const fn code(&self) -> LabelDiagnosticCode {
        self.code
    }

    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    #[must_use]
    pub fn field(&self) -> Option<&str> {
        self.field.as_deref()
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Clone, Debug)]
pub struct LabelParseReport {
    label: Option<Label>,
    diagnostics: Vec<LabelDiagnostic>,
}

impl LabelParseReport {
    #[must_use]
    pub const fn label(&self) -> Option<&Label> {
        self.label.as_ref()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[LabelDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        has_errors(&self.diagnostics)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LabelSource<'a> {
    source: &'a str,
    bytes: &'a [u8],
}

impl<'a> LabelSource<'a> {
    #[must_use]
    pub const fn new(source: &'a str, bytes: &'a [u8]) -> Self {
        Self { source, bytes }
    }

    #[must_use]
    pub const fn source(self) -> &'a str {
        self.source
    }

    #[must_use]
    pub const fn bytes(self) -> &'a [u8] {
        self.bytes
    }
}

#[derive(Clone, Debug)]
pub struct LabelRegistry {
    labels: Arc<[Label]>,
    by_name: BTreeMap<Arc<str>, usize>,
}

impl LabelRegistry {
    fn new(labels: Vec<Label>) -> Self {
        let by_name = labels
            .iter()
            .enumerate()
            .map(|(index, label)| (Arc::clone(&label.name), index))
            .collect();
        Self {
            labels: labels.into(),
            by_name,
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Label> {
        self.labels.iter()
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Label> {
        self.by_name
            .get(name)
            .and_then(|index| self.labels.get(*index))
    }
}

#[derive(Clone, Debug)]
pub struct LabelRegistryReport {
    registry: Option<LabelRegistry>,
    diagnostics: Vec<LabelDiagnostic>,
}

impl LabelRegistryReport {
    #[must_use]
    pub const fn registry(&self) -> Option<&LabelRegistry> {
        self.registry.as_ref()
    }

    #[must_use]
    pub fn diagnostics(&self) -> &[LabelDiagnostic] {
        &self.diagnostics
    }

    #[must_use]
    pub fn has_errors(&self) -> bool {
        has_errors(&self.diagnostics)
    }
}

#[derive(Default)]
struct RawLabel {
    search_method: Option<Value>,
    image_base64: Option<Value>,
    range_x: Option<Value>,
    range_y: Option<Value>,
    range_width: Option<Value>,
    range_height: Option<Value>,
    target_x: Option<Value>,
    target_y: Option<Value>,
    target_width: Option<Value>,
    target_height: Option<Value>,
    unknown_fields: Vec<String>,
    duplicate_fields: Vec<String>,
}

impl<'de> Deserialize<'de> for RawLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_map(RawLabelVisitor)
    }
}

struct RawLabelVisitor;

impl<'de> Visitor<'de> for RawLabelVisitor {
    type Value = RawLabel;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a legacy .IL JSON object")
    }

    fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    where
        M: MapAccess<'de>,
    {
        let mut raw = RawLabel::default();
        let mut seen = HashSet::new();
        while let Some(key) = map.next_key::<String>()? {
            if !seen.insert(key.clone()) {
                map.next_value::<IgnoredAny>()?;
                if raw.duplicate_fields.len() < MAX_LABEL_DIAGNOSTICS_PER_SOURCE {
                    raw.duplicate_fields.push(key);
                }
                continue;
            }
            match key.as_str() {
                "searchMethod" => raw.search_method = Some(map.next_value()?),
                "ImgBase64" => raw.image_base64 = Some(map.next_value()?),
                "RangeX" => raw.range_x = Some(map.next_value()?),
                "RangeY" => raw.range_y = Some(map.next_value()?),
                "RangeWidth" => raw.range_width = Some(map.next_value()?),
                "RangeHeight" => raw.range_height = Some(map.next_value()?),
                "TargetX" => raw.target_x = Some(map.next_value()?),
                "TargetY" => raw.target_y = Some(map.next_value()?),
                "TargetWidth" => raw.target_width = Some(map.next_value()?),
                "TargetHeight" => raw.target_height = Some(map.next_value()?),
                _ => {
                    map.next_value::<IgnoredAny>()?;
                    if raw.unknown_fields.len() < MAX_LABEL_DIAGNOSTICS_PER_SOURCE {
                        raw.unknown_fields.push(key);
                    }
                }
            }
        }
        Ok(raw)
    }
}

pub fn parse_legacy_il(
    source: &str,
    bytes: &[u8],
    native_pool: &NativePool,
    limits: &VisionLimits,
    cancellation: &CancellationToken,
) -> Result<LabelParseReport, VisionError> {
    reject_cancelled(cancellation, "label parsing was cancelled")?;
    let diagnostic_source = bounded_diagnostic_source(source);
    if bytes.len() > MAX_LABEL_JSON_BYTES {
        return Ok(error_report(LabelDiagnostic::new(
            LabelDiagnosticSeverity::Error,
            LabelDiagnosticCode::InputLimit,
            diagnostic_source,
            None,
            format!("legacy .IL JSON exceeds {MAX_LABEL_JSON_BYTES} bytes"),
        )));
    }

    let name = match label_name(source) {
        Ok(name) => name,
        Err((code, message)) => {
            return Ok(error_report(LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                code,
                diagnostic_source,
                None,
                message,
            )));
        }
    };
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            return Ok(error_report(LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidUtf8,
                diagnostic_source,
                None,
                "legacy .IL input is not valid UTF-8",
            )));
        }
    };
    let raw = match serde_json::from_str::<RawLabel>(text) {
        Ok(raw) => raw,
        Err(error) => {
            return Ok(error_report(LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidJson,
                diagnostic_source,
                None,
                format!("invalid legacy .IL JSON: {error}"),
            )));
        }
    };

    let mut diagnostics = Vec::new();
    for field in raw.duplicate_fields.iter() {
        push_diagnostic(
            &mut diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::DuplicateKey,
                Arc::clone(&diagnostic_source),
                Some(field),
                format!("duplicate legacy .IL field `{field}`"),
            ),
        );
    }
    for field in raw.unknown_fields.iter() {
        push_diagnostic(
            &mut diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Warning,
                LabelDiagnosticCode::UnknownField,
                Arc::clone(&diagnostic_source),
                Some(field),
                format!("unknown legacy .IL field `{field}` was ignored"),
            ),
        );
    }
    if has_errors(&diagnostics) {
        return Ok(LabelParseReport {
            label: None,
            diagnostics,
        });
    }

    let method_value = parse_u32(
        raw.search_method.as_ref(),
        5,
        "searchMethod",
        &diagnostic_source,
        &mut diagnostics,
    );
    let image_base64 = parse_string(
        raw.image_base64.as_ref(),
        "",
        "ImgBase64",
        &diagnostic_source,
        &mut diagnostics,
    );
    let range_x = parse_u32(
        raw.range_x.as_ref(),
        0,
        "RangeX",
        &diagnostic_source,
        &mut diagnostics,
    );
    let range_y = parse_u32(
        raw.range_y.as_ref(),
        0,
        "RangeY",
        &diagnostic_source,
        &mut diagnostics,
    );
    let range_width = parse_u32(
        raw.range_width.as_ref(),
        0,
        "RangeWidth",
        &diagnostic_source,
        &mut diagnostics,
    );
    let range_height = parse_u32(
        raw.range_height.as_ref(),
        0,
        "RangeHeight",
        &diagnostic_source,
        &mut diagnostics,
    );
    let target_x = parse_u32(
        raw.target_x.as_ref(),
        0,
        "TargetX",
        &diagnostic_source,
        &mut diagnostics,
    );
    let target_y = parse_u32(
        raw.target_y.as_ref(),
        0,
        "TargetY",
        &diagnostic_source,
        &mut diagnostics,
    );
    let target_width = parse_u32(
        raw.target_width.as_ref(),
        0,
        "TargetWidth",
        &diagnostic_source,
        &mut diagnostics,
    );
    let target_height = parse_u32(
        raw.target_height.as_ref(),
        0,
        "TargetHeight",
        &diagnostic_source,
        &mut diagnostics,
    );
    if has_errors(&diagnostics) {
        return Ok(LabelParseReport {
            label: None,
            diagnostics,
        });
    }

    let method = method_value.and_then(LabelMethod::from_legacy);
    if method.is_none() {
        push_diagnostic(
            &mut diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::UnsupportedMethod,
                Arc::clone(&diagnostic_source),
                Some("searchMethod"),
                format!(
                    "legacy search method {} is not active in Phase 3",
                    method_value.expect("validated method value")
                ),
            ),
        );
    }
    let range = make_roi(
        range_x,
        range_y,
        range_width,
        range_height,
        "Range",
        &diagnostic_source,
        &mut diagnostics,
    );
    let target_roi = make_roi(
        target_x,
        target_y,
        target_width,
        target_height,
        "Target",
        &diagnostic_source,
        &mut diagnostics,
    );
    if let Some(range) = range {
        validate_roi_limit(range, "Range", limits, &diagnostic_source, &mut diagnostics);
    }
    if let Some(target_roi) = target_roi {
        validate_roi_limit(
            target_roi,
            "Target",
            limits,
            &diagnostic_source,
            &mut diagnostics,
        );
    }
    if let (Some(range), Some(target_roi)) = (range, target_roi)
        && !roi_contains(range, target_roi)
    {
        push_diagnostic(
            &mut diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::TargetOutsideRange,
                Arc::clone(&diagnostic_source),
                None,
                "Target ROI must be fully contained by Range ROI",
            ),
        );
    }
    if has_errors(&diagnostics) {
        return Ok(LabelParseReport {
            label: None,
            diagnostics,
        });
    }

    let method = method.expect("method validation precedes label construction");
    let image_base64 = image_base64.expect("string validation precedes label construction");
    let target = if method == LabelMethod::Ocr {
        if image_base64.len() > MAX_LABEL_TEXT_BYTES
            || image_base64
                .chars()
                .take(MAX_LABEL_TEXT_SCALARS + 1)
                .count()
                > MAX_LABEL_TEXT_SCALARS
        {
            push_diagnostic(
                &mut diagnostics,
                LabelDiagnostic::new(
                    LabelDiagnosticSeverity::Error,
                    LabelDiagnosticCode::TargetLimit,
                    Arc::clone(&diagnostic_source),
                    Some("ImgBase64"),
                    "OCR expected text exceeds label limits",
                ),
            );
            None
        } else {
            Some(LabelTarget::Text(Arc::from(image_base64)))
        }
    } else {
        let encoded =
            decode_target_base64(image_base64, limits, &diagnostic_source, &mut diagnostics);
        match encoded {
            Some(encoded) => {
                reject_cancelled(cancellation, "label target decode was cancelled")?;
                match native_pool.decode(&encoded, limits, cancellation) {
                    Ok(image) => {
                        if image.width() != target_width.expect("validated TargetWidth")
                            || image.height() != target_height.expect("validated TargetHeight")
                        {
                            push_diagnostic(
                                &mut diagnostics,
                                LabelDiagnostic::new(
                                    LabelDiagnosticSeverity::Error,
                                    LabelDiagnosticCode::TargetDimensions,
                                    Arc::clone(&diagnostic_source),
                                    Some("ImgBase64"),
                                    "decoded target dimensions do not match TargetWidth/TargetHeight",
                                ),
                            );
                            None
                        } else {
                            Some(LabelTarget::Image(image))
                        }
                    }
                    Err(error) => match decode_diagnostic_code(error.kind()) {
                        Some(code) => {
                            push_diagnostic(
                                &mut diagnostics,
                                LabelDiagnostic::new(
                                    LabelDiagnosticSeverity::Error,
                                    code,
                                    Arc::clone(&diagnostic_source),
                                    Some("ImgBase64"),
                                    format!("failed to decode label target: {}", error.message()),
                                ),
                            );
                            None
                        }
                        None => return Err(error),
                    },
                }
            }
            None => None,
        }
    };

    if has_errors(&diagnostics) || target.is_none() {
        return Ok(LabelParseReport {
            label: None,
            diagnostics,
        });
    }
    Ok(LabelParseReport {
        label: Some(Label {
            name,
            source: Arc::from(source),
            method,
            range: range.expect("ROI validation precedes label construction"),
            target_roi: target_roi.expect("ROI validation precedes label construction"),
            target: target.expect("target validation precedes label construction"),
        }),
        diagnostics,
    })
}

pub fn build_legacy_label_registry(
    sources: &[LabelSource<'_>],
    native_pool: &NativePool,
    limits: &VisionLimits,
    cancellation: &CancellationToken,
) -> Result<LabelRegistryReport, VisionError> {
    reject_cancelled(cancellation, "label registry construction was cancelled")?;
    if sources.len() > MAX_LABEL_SOURCES {
        return Ok(LabelRegistryReport {
            registry: None,
            diagnostics: vec![LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::RegistryLimit,
                Arc::from("<registry>"),
                None,
                format!("label registry exceeds {MAX_LABEL_SOURCES} sources"),
            )],
        });
    }

    let mut ordered = sources.to_vec();
    ordered.sort_by(|left, right| left.source.as_bytes().cmp(right.source.as_bytes()));
    let mut labels = Vec::with_capacity(ordered.len());
    let mut diagnostics = Vec::new();
    let mut diagnostic_budgets = BTreeMap::new();
    for source in ordered {
        reject_cancelled(cancellation, "label registry construction was cancelled")?;
        let report = parse_legacy_il(
            source.source,
            source.bytes,
            native_pool,
            limits,
            cancellation,
        )?;
        for diagnostic in report.diagnostics {
            push_registry_diagnostic(&mut diagnostics, &mut diagnostic_budgets, diagnostic);
        }
        if let Some(label) = report.label {
            labels.push(label);
        }
    }

    let mut sources_by_name: BTreeMap<Arc<str>, Vec<Arc<str>>> = BTreeMap::new();
    for label in &labels {
        sources_by_name
            .entry(Arc::clone(&label.name))
            .or_default()
            .push(Arc::clone(&label.source));
    }
    for (name, duplicate_sources) in sources_by_name {
        if duplicate_sources.len() < 2 {
            continue;
        }
        for source in duplicate_sources {
            push_registry_diagnostic(
                &mut diagnostics,
                &mut diagnostic_budgets,
                LabelDiagnostic::new(
                    LabelDiagnosticSeverity::Error,
                    LabelDiagnosticCode::DuplicateName,
                    source,
                    None,
                    format!("duplicate label name `{name}`"),
                ),
            );
        }
    }
    diagnostics.sort_by(|left, right| {
        left.source
            .as_bytes()
            .cmp(right.source.as_bytes())
            .then_with(|| diagnostic_rank(left).cmp(&diagnostic_rank(right)))
            .then_with(|| left.field.as_deref().cmp(&right.field.as_deref()))
    });

    let registry = if has_errors(&diagnostics) {
        None
    } else {
        Some(LabelRegistry::new(labels))
    };
    Ok(LabelRegistryReport {
        registry,
        diagnostics,
    })
}

#[derive(Clone)]
pub struct LabelEvaluator {
    native_pool: NativePool,
    limits: VisionLimits,
    ocr_pool: Option<OcrPool>,
}

impl LabelEvaluator {
    #[must_use]
    pub const fn new(native_pool: NativePool, limits: VisionLimits) -> Self {
        Self {
            native_pool,
            limits,
            ocr_pool: None,
        }
    }

    #[must_use]
    pub fn with_ocr_pool(mut self, ocr_pool: OcrPool) -> Self {
        self.ocr_pool = Some(ocr_pool);
        self
    }

    pub fn evaluate(
        &self,
        label: &Label,
        frame: Arc<Frame>,
        cancellation: &CancellationToken,
    ) -> Result<LabelEvaluation, VisionError> {
        reject_cancelled(cancellation, "label evaluation was cancelled")?;
        let sequence = frame.sequence();
        let timestamp_ns = frame.timestamp_ns();

        match (&label.method, &label.target) {
            (LabelMethod::Ocr, LabelTarget::Text(expected)) => {
                ensure_roi_in_image(label.target_roi, frame.image(), "label Target")?;
                let ocr_pool = self.ocr_pool.as_ref().ok_or_else(|| {
                    VisionError::validation("OCR label evaluation requires an OCR pool")
                })?;
                let target = frame.crop(
                    &self.native_pool,
                    cancellation,
                    label.target_roi,
                    &self.limits,
                )?;
                let output =
                    ocr_pool.recognize(&self.native_pool, &target, &self.limits, cancellation)?;
                let confidence = finite_score(output.confidence(), "OCR confidence")?;
                let similarity = text_similarity(output.text(), expected, cancellation)?;
                let score = finite_score(similarity * confidence, "OCR label score")?;
                Ok(LabelEvaluation {
                    x: label.target_roi.x(),
                    y: label.target_roi.y(),
                    score,
                    sequence,
                    timestamp_ns,
                    recognized_text: Some(Arc::from(output.text())),
                    ocr_confidence: Some(confidence),
                    text_similarity: Some(similarity),
                })
            }
            (method, LabelTarget::Image(target)) if *method != LabelMethod::Ocr => {
                ensure_roi_in_image(label.range, frame.image(), "label Range")?;
                let range =
                    frame.crop(&self.native_pool, cancellation, label.range, &self.limits)?;
                let matched = match method {
                    LabelMethod::SqDiffNormed
                    | LabelMethod::CCorrNormed
                    | LabelMethod::CCoeffNormed => {
                        let search = if range.format() == target.format() {
                            range
                        } else {
                            self.native_pool.convert(
                                &range,
                                target.format(),
                                &self.limits,
                                cancellation,
                            )?
                        };
                        let template_method = match method {
                            LabelMethod::SqDiffNormed => TemplateMethod::SqDiffNormed,
                            LabelMethod::CCorrNormed => TemplateMethod::CCorrNormed,
                            LabelMethod::CCoeffNormed => TemplateMethod::CCoeffNormed,
                            _ => unreachable!("guarded template method"),
                        };
                        self.native_pool.match_template(
                            &search,
                            target,
                            template_method,
                            &self.limits,
                            cancellation,
                        )?
                    }
                    LabelMethod::EdgeXy | LabelMethod::EdgeLaplacian => {
                        let edge_method = match method {
                            LabelMethod::EdgeXy => EdgeMethod::Xy,
                            LabelMethod::EdgeLaplacian => EdgeMethod::Laplacian,
                            _ => unreachable!("guarded edge method"),
                        };
                        self.native_pool.match_edge(
                            &range,
                            target,
                            edge_method,
                            &self.limits,
                            cancellation,
                        )?
                    }
                    LabelMethod::Ocr => unreachable!("image target excludes OCR"),
                };
                let x = label.range.x().checked_add(matched.x()).ok_or_else(|| {
                    VisionError::internal("absolute label match x coordinate overflows")
                })?;
                let y = label.range.y().checked_add(matched.y()).ok_or_else(|| {
                    VisionError::internal("absolute label match y coordinate overflows")
                })?;
                Ok(LabelEvaluation {
                    x,
                    y,
                    score: finite_score(matched.score(), "template label score")?,
                    sequence,
                    timestamp_ns,
                    recognized_text: None,
                    ocr_confidence: None,
                    text_similarity: None,
                })
            }
            _ => Err(VisionError::internal(
                "label method and immutable target kind are inconsistent",
            )),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LabelEvaluation {
    x: u32,
    y: u32,
    score: f32,
    sequence: u64,
    timestamp_ns: u64,
    recognized_text: Option<Arc<str>>,
    ocr_confidence: Option<f32>,
    text_similarity: Option<f32>,
}

impl LabelEvaluation {
    #[must_use]
    pub const fn x(&self) -> u32 {
        self.x
    }

    #[must_use]
    pub const fn y(&self) -> u32 {
        self.y
    }

    #[must_use]
    pub const fn score(&self) -> f32 {
        self.score
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }

    #[must_use]
    pub fn recognized_text(&self) -> Option<&str> {
        self.recognized_text.as_deref()
    }

    #[must_use]
    pub const fn ocr_confidence(&self) -> Option<f32> {
        self.ocr_confidence
    }

    #[must_use]
    pub const fn text_similarity(&self) -> Option<f32> {
        self.text_similarity
    }
}

fn parse_u32(
    value: Option<&Value>,
    default: u32,
    field: &str,
    source: &Arc<str>,
    diagnostics: &mut Vec<LabelDiagnostic>,
) -> Option<u32> {
    let Some(value) = value else {
        return Some(default);
    };
    if !value.is_number() {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidType,
                Arc::clone(source),
                Some(field),
                format!("legacy .IL field `{field}` must be an integer"),
            ),
        );
        return None;
    }
    let Some(number) = value.as_u64().and_then(|number| u32::try_from(number).ok()) else {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidNumber,
                Arc::clone(source),
                Some(field),
                format!("legacy .IL field `{field}` must fit an unsigned 32-bit integer"),
            ),
        );
        return None;
    };
    Some(number)
}

fn parse_string<'a>(
    value: Option<&'a Value>,
    default: &'a str,
    field: &str,
    source: &Arc<str>,
    diagnostics: &mut Vec<LabelDiagnostic>,
) -> Option<&'a str> {
    let Some(value) = value else {
        return Some(default);
    };
    let Some(text) = value.as_str() else {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidType,
                Arc::clone(source),
                Some(field),
                format!("legacy .IL field `{field}` must be a string"),
            ),
        );
        return None;
    };
    Some(text)
}

fn make_roi(
    x: Option<u32>,
    y: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
    field: &str,
    source: &Arc<str>,
    diagnostics: &mut Vec<LabelDiagnostic>,
) -> Option<Roi> {
    let (Some(x), Some(y), Some(width), Some(height)) = (x, y, width, height) else {
        return None;
    };
    match Roi::new(x, y, width, height) {
        Ok(roi) => Some(roi),
        Err(error) => {
            push_diagnostic(
                diagnostics,
                LabelDiagnostic::new(
                    LabelDiagnosticSeverity::Error,
                    LabelDiagnosticCode::InvalidRoi,
                    Arc::clone(source),
                    Some(field),
                    format!("invalid {field} ROI: {}", error.message()),
                ),
            );
            None
        }
    }
}

fn validate_roi_limit(
    roi: Roi,
    field: &str,
    limits: &VisionLimits,
    source: &Arc<str>,
    diagnostics: &mut Vec<LabelDiagnostic>,
) {
    let right = roi.x().checked_add(roi.width());
    let bottom = roi.y().checked_add(roi.height());
    let pixels = u64::from(roi.width()).checked_mul(u64::from(roi.height()));
    if right.is_none_or(|value| value > limits.max_width())
        || bottom.is_none_or(|value| value > limits.max_height())
        || pixels.is_none_or(|value| value > limits.max_pixels())
    {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::RoiLimit,
                Arc::clone(source),
                Some(field),
                format!("{field} ROI exceeds Vision limits"),
            ),
        );
    }
}

fn decode_target_base64(
    encoded: &str,
    limits: &VisionLimits,
    source: &Arc<str>,
    diagnostics: &mut Vec<LabelDiagnostic>,
) -> Option<Vec<u8>> {
    let decoded_length = strict_base64_decoded_length(encoded).ok();
    if encoded.is_empty() || decoded_length.is_none() {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidBase64,
                Arc::clone(source),
                Some("ImgBase64"),
                "image label target must use canonical padded Base64",
            ),
        );
        return None;
    }
    if decoded_length.expect("checked above") > limits.max_encoded_bytes() {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::TargetLimit,
                Arc::clone(source),
                Some("ImgBase64"),
                "decoded label target exceeds encoded image limits",
            ),
        );
        return None;
    }
    let decoded = match STANDARD_BASE64.decode(encoded) {
        Ok(decoded) => decoded,
        Err(_) => {
            push_diagnostic(
                diagnostics,
                LabelDiagnostic::new(
                    LabelDiagnosticSeverity::Error,
                    LabelDiagnosticCode::InvalidBase64,
                    Arc::clone(source),
                    Some("ImgBase64"),
                    "image label target must use canonical padded Base64",
                ),
            );
            return None;
        }
    };
    if decoded.len() != decoded_length.expect("checked above")
        || STANDARD_BASE64.encode(&decoded) != encoded
    {
        push_diagnostic(
            diagnostics,
            LabelDiagnostic::new(
                LabelDiagnosticSeverity::Error,
                LabelDiagnosticCode::InvalidBase64,
                Arc::clone(source),
                Some("ImgBase64"),
                "image label target Base64 is not canonical",
            ),
        );
        return None;
    }
    Some(decoded)
}

fn strict_base64_decoded_length(encoded: &str) -> Result<usize, ()> {
    let bytes = encoded.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return Err(());
    }
    let padding = match bytes {
        [.., b'=', b'='] => 2,
        [.., b'='] => 1,
        _ => 0,
    };
    if bytes[..bytes.len() - padding].contains(&b'=') {
        return Err(());
    }
    let content = &bytes[..bytes.len() - padding];
    if content.iter().any(|byte| base64_value(*byte).is_none()) {
        return Err(());
    }
    match padding {
        2 if base64_value(*content.last().ok_or(())?).ok_or(())? & 0x0f != 0 => {
            return Err(());
        }
        1 if base64_value(*content.last().ok_or(())?).ok_or(())? & 0x03 != 0 => {
            return Err(());
        }
        _ => {}
    }
    bytes
        .len()
        .checked_div(4)
        .and_then(|blocks| blocks.checked_mul(3))
        .and_then(|length| length.checked_sub(padding))
        .ok_or(())
}

fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn label_name(source: &str) -> Result<Arc<str>, (LabelDiagnosticCode, &'static str)> {
    if source.is_empty() || source.contains('\0') || source.len() > MAX_LABEL_SOURCE_BYTES {
        return Err((
            LabelDiagnosticCode::InvalidSource,
            "label source must be non-empty, bounded UTF-8 without NUL",
        ));
    }
    let file_name = source.rsplit(['/', '\\']).next().unwrap_or_default();
    let Some(name) = file_name.strip_suffix(".IL") else {
        return Err((
            LabelDiagnosticCode::UnsupportedExtension,
            "Phase 3 accepts only the exact .IL extension; .ILX has no entry",
        ));
    };
    if name.is_empty() || name.contains('\0') || name.len() > MAX_LABEL_NAME_BYTES {
        return Err((
            LabelDiagnosticCode::InvalidSource,
            "label file stem must be non-empty, bounded UTF-8 without NUL",
        ));
    }
    Ok(Arc::from(name))
}

fn bounded_diagnostic_source(source: &str) -> Arc<str> {
    if source.len() <= MAX_LABEL_SOURCE_BYTES {
        Arc::from(source)
    } else {
        Arc::from("<source-over-limit>")
    }
}

fn decode_diagnostic_code(kind: VisionErrorKind) -> Option<LabelDiagnosticCode> {
    match kind {
        VisionErrorKind::Validation | VisionErrorKind::InvalidImage => {
            Some(LabelDiagnosticCode::TargetImage)
        }
        VisionErrorKind::Limit => Some(LabelDiagnosticCode::TargetLimit),
        VisionErrorKind::NoFrame
        | VisionErrorKind::Closed
        | VisionErrorKind::Faulted
        | VisionErrorKind::Cancelled
        | VisionErrorKind::Deadline
        | VisionErrorKind::ModelNotFound
        | VisionErrorKind::Native
        | VisionErrorKind::PoolClosed
        | VisionErrorKind::Internal => None,
    }
}

fn roi_contains(outer: Roi, inner: Roi) -> bool {
    let outer_right = outer.x() + outer.width();
    let outer_bottom = outer.y() + outer.height();
    let inner_right = inner.x() + inner.width();
    let inner_bottom = inner.y() + inner.height();
    inner.x() >= outer.x()
        && inner.y() >= outer.y()
        && inner_right <= outer_right
        && inner_bottom <= outer_bottom
}

fn ensure_roi_in_image(roi: Roi, image: &Image, label: &str) -> Result<(), VisionError> {
    let right = roi
        .x()
        .checked_add(roi.width())
        .ok_or_else(|| VisionError::limit(format!("{label} right bound overflows")))?;
    let bottom = roi
        .y()
        .checked_add(roi.height())
        .ok_or_else(|| VisionError::limit(format!("{label} bottom bound overflows")))?;
    if right > image.width() || bottom > image.height() {
        return Err(VisionError::limit(format!(
            "{label} is outside the evaluated Frame"
        )));
    }
    Ok(())
}

fn finite_score(value: f32, label: &str) -> Result<f32, VisionError> {
    if !value.is_finite() {
        return Err(VisionError::internal(format!("{label} is non-finite")));
    }
    Ok(value.clamp(0.0, 1.0))
}

fn text_similarity(
    actual: &str,
    expected: &str,
    cancellation: &CancellationToken,
) -> Result<f32, VisionError> {
    reject_cancelled(cancellation, "OCR text comparison was cancelled")?;
    let actual = actual.trim_matches(char::is_whitespace);
    let actual_length = bounded_scalar_count(actual)?;
    let expected_length = bounded_scalar_count(expected)?;
    reject_cancelled(cancellation, "OCR text comparison was cancelled")?;
    if actual_length == 0 || expected_length == 0 {
        return Ok(if actual_length == expected_length {
            1.0
        } else {
            0.0
        });
    }
    let edit_cells = actual_length
        .checked_mul(expected_length)
        .ok_or_else(|| VisionError::limit("OCR edit cell count overflows"))?;
    if edit_cells > MAX_LABEL_EDIT_CELLS {
        return Err(VisionError::limit(format!(
            "OCR edit cell count exceeds {MAX_LABEL_EDIT_CELLS}"
        )));
    }

    let expected_chars = expected.chars().collect::<Vec<_>>();
    let mut previous = (0..=expected_length).collect::<Vec<_>>();
    let mut current = vec![0; expected_length + 1];
    for (row, actual_character) in actual.chars().enumerate() {
        if row % 64 == 0 {
            reject_cancelled(cancellation, "OCR text comparison was cancelled")?;
        }
        current[0] = row + 1;
        for (column, expected_character) in expected_chars.iter().enumerate() {
            let substitution =
                previous[column] + usize::from(actual_character != *expected_character);
            let deletion = previous[column + 1] + 1;
            let insertion = current[column] + 1;
            current[column + 1] = substitution.min(deletion).min(insertion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    reject_cancelled(cancellation, "OCR text comparison was cancelled")?;
    let distance = previous[expected_length];
    let denominator = actual_length.max(expected_length);
    let similarity = 1.0 - distance as f64 / denominator as f64;
    if !similarity.is_finite() {
        return Err(VisionError::internal("OCR text similarity is non-finite"));
    }
    Ok((similarity as f32).clamp(0.0, 1.0))
}

fn bounded_scalar_count(text: &str) -> Result<usize, VisionError> {
    if text.len() > MAX_LABEL_TEXT_BYTES {
        return Err(VisionError::limit(format!(
            "OCR text exceeds {MAX_LABEL_TEXT_BYTES} bytes"
        )));
    }
    let count = text.chars().take(MAX_LABEL_TEXT_SCALARS + 1).count();
    if count > MAX_LABEL_TEXT_SCALARS {
        return Err(VisionError::limit(format!(
            "OCR text exceeds {MAX_LABEL_TEXT_SCALARS} Unicode scalars"
        )));
    }
    Ok(count)
}

fn reject_cancelled(
    cancellation: &CancellationToken,
    message: &'static str,
) -> Result<(), VisionError> {
    if cancellation.is_cancelled() {
        Err(VisionError::cancelled(message))
    } else {
        Ok(())
    }
}

fn push_diagnostic(diagnostics: &mut Vec<LabelDiagnostic>, diagnostic: LabelDiagnostic) {
    if diagnostics.len() < MAX_LABEL_DIAGNOSTICS_PER_SOURCE {
        diagnostics.push(diagnostic);
    } else if diagnostic.severity == LabelDiagnosticSeverity::Error
        && let Some(index) = diagnostics
            .iter()
            .rposition(|existing| existing.severity == LabelDiagnosticSeverity::Warning)
    {
        diagnostics[index] = diagnostic;
    }
}

fn push_registry_diagnostic(
    diagnostics: &mut Vec<LabelDiagnostic>,
    budgets: &mut BTreeMap<Arc<str>, SourceDiagnosticBudget>,
    diagnostic: LabelDiagnostic,
) {
    let budget = budgets.entry(Arc::clone(&diagnostic.source)).or_default();
    if budget.count < MAX_LABEL_DIAGNOSTICS_PER_SOURCE {
        if diagnostic.severity == LabelDiagnosticSeverity::Warning {
            budget.warning_indices.push(diagnostics.len());
        }
        diagnostics.push(diagnostic);
        budget.count += 1;
    } else if diagnostic.severity == LabelDiagnosticSeverity::Error
        && let Some(index) = budget.warning_indices.pop()
    {
        diagnostics[index] = diagnostic;
    }
}

#[derive(Default)]
struct SourceDiagnosticBudget {
    count: usize,
    warning_indices: Vec<usize>,
}

fn has_errors(diagnostics: &[LabelDiagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == LabelDiagnosticSeverity::Error)
}

fn error_report(diagnostic: LabelDiagnostic) -> LabelParseReport {
    LabelParseReport {
        label: None,
        diagnostics: vec![diagnostic],
    }
}

fn diagnostic_rank(diagnostic: &LabelDiagnostic) -> u8 {
    match diagnostic.severity {
        LabelDiagnosticSeverity::Error => 0,
        LabelDiagnosticSeverity::Warning => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_scalar_similarity_trims_only_actual_and_handles_empty_values() {
        let cancellation = CancellationToken::root();
        assert_eq!(
            text_similarity("\u{2003}EASCON\r\n", "EASCON", &cancellation),
            Ok(1.0)
        );
        assert_eq!(text_similarity("", "", &cancellation), Ok(1.0));
        assert_eq!(text_similarity(" \n", "x", &cancellation), Ok(0.0));
        assert_eq!(text_similarity(" ", " ", &cancellation), Ok(0.0));
        assert_eq!(text_similarity("猫", "狗", &cancellation), Ok(0.0));
        assert_eq!(text_similarity("a猫", "a狗", &cancellation), Ok(0.5));
    }

    #[test]
    fn text_similarity_checks_cancellation_and_edit_cell_ceiling() {
        let cancelled = CancellationToken::root();
        cancelled.cancel();
        assert_eq!(
            text_similarity("a", "a", &cancelled)
                .expect_err("cancelled comparison")
                .kind(),
            VisionErrorKind::Cancelled
        );
        assert_eq!(
            text_similarity("", "", &cancelled)
                .expect_err("cancelled empty comparison")
                .kind(),
            VisionErrorKind::Cancelled
        );

        let cancellation = CancellationToken::root();
        let large = "a".repeat(MAX_LABEL_TEXT_SCALARS);
        assert_eq!(
            text_similarity(&large, &large, &cancellation)
                .expect_err("edit cell ceiling")
                .kind(),
            VisionErrorKind::Limit
        );
    }

    #[test]
    fn duplicate_unknown_keys_are_detected_without_last_wins() {
        let raw = serde_json::from_str::<RawLabel>(
            r#"{"Future":1,"Future":2,"searchMethod":5,"searchMethod":3}"#,
        )
        .expect("structured duplicate parse");
        assert_eq!(raw.unknown_fields, ["Future"]);
        assert_eq!(raw.duplicate_fields, ["Future", "searchMethod"]);
        assert_eq!(raw.search_method, Some(Value::from(5)));
    }

    #[test]
    fn strict_base64_length_requires_canonical_padding_shape() {
        assert_eq!(strict_base64_decoded_length("YWI="), Ok(2));
        assert_eq!(strict_base64_decoded_length("YQ=="), Ok(1));
        assert_eq!(strict_base64_decoded_length("YWI"), Err(()));
        assert_eq!(strict_base64_decoded_length("YW=I"), Err(()));
    }
}
