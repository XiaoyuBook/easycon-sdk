use easycon_native_sys::operations as native;
use easycon_runtime::CancellationToken;

use crate::{Image, ImageError, ImageErrorKind, NativePool, VisionError, VisionLimits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemplateMethod {
    SqDiffNormed,
    CCorrNormed,
    CCoeffNormed,
}

impl TemplateMethod {
    const fn into_native(self) -> native::TemplateMethod {
        match self {
            Self::SqDiffNormed => native::TemplateMethod::SqDiffNormed,
            Self::CCorrNormed => native::TemplateMethod::CCorrNormed,
            Self::CCoeffNormed => native::TemplateMethod::CCoeffNormed,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeMethod {
    Xy,
    Laplacian,
}

impl EdgeMethod {
    const fn into_native(self) -> native::EdgeMethod {
        match self {
            Self::Xy => native::EdgeMethod::Xy,
            Self::Laplacian => native::EdgeMethod::Laplacian,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchResult {
    pub x: u32,
    pub y: u32,
    pub score: f32,
    raw: f32,
}

impl MatchResult {
    #[must_use]
    pub const fn x(self) -> u32 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> u32 {
        self.y
    }

    #[must_use]
    pub const fn score(self) -> f32 {
        self.score
    }

    #[must_use]
    pub const fn raw(self) -> f32 {
        self.raw
    }
}

pub fn match_template(
    pool: &NativePool,
    cancellation: &CancellationToken,
    search: &Image,
    target: &Image,
    method: TemplateMethod,
    limits: &VisionLimits,
) -> Result<MatchResult, VisionError> {
    pool.match_template(search, target, method, limits, cancellation)
}

pub(crate) fn match_template_direct(
    search: &Image,
    target: &Image,
    method: TemplateMethod,
    limits: &VisionLimits,
) -> Result<MatchResult, ImageError> {
    if search.format() != target.format() {
        return Err(ImageError::new(
            ImageErrorKind::InvalidArgument,
            "template image formats differ",
        ));
    }
    if target.width() > search.width() || target.height() > search.height() {
        return Err(ImageError::new(
            ImageErrorKind::OutOfRange,
            "target exceeds search image",
        ));
    }
    let extrema = native::match_template(
        search.native_view(limits)?,
        target.native_view(limits)?,
        method.into_native(),
        limits.native_limits(),
    )
    .map_err(ImageError::from_native)?;
    normalize(extrema, search, target, method)
}

pub fn preprocess_edge(
    pool: &NativePool,
    cancellation: &CancellationToken,
    image: &Image,
    method: EdgeMethod,
    limits: &VisionLimits,
) -> Result<Image, VisionError> {
    pool.preprocess_edge(image, method, limits, cancellation)
}

pub(crate) fn preprocess_edge_direct(
    image: &Image,
    method: EdgeMethod,
    limits: &VisionLimits,
) -> Result<Image, ImageError> {
    let output = native::preprocess_edge(
        image.native_view(limits)?,
        method.into_native(),
        limits.native_limits(),
    )
    .map_err(ImageError::from_native)?;
    Image::from_native(output, limits)
}

pub fn match_edge(
    pool: &NativePool,
    cancellation: &CancellationToken,
    search: &Image,
    target: &Image,
    method: EdgeMethod,
    limits: &VisionLimits,
) -> Result<MatchResult, VisionError> {
    pool.match_edge(search, target, method, limits, cancellation)
}

pub(crate) fn match_edge_direct(
    search: &Image,
    target: &Image,
    method: EdgeMethod,
    limits: &VisionLimits,
) -> Result<MatchResult, ImageError> {
    let search_edge = preprocess_edge_direct(search, method, limits)?;
    let target_edge = preprocess_edge_direct(target, method, limits)?;
    match_template_direct(
        &search_edge,
        &target_edge,
        TemplateMethod::CCoeffNormed,
        limits,
    )
}

fn normalize(
    extrema: native::MatchExtrema,
    search: &Image,
    target: &Image,
    method: TemplateMethod,
) -> Result<MatchResult, ImageError> {
    let (x, y, raw, score) = match method {
        TemplateMethod::SqDiffNormed => (
            extrema.min_x,
            extrema.min_y,
            extrema.min_value,
            1.0 - extrema.min_value,
        ),
        TemplateMethod::CCorrNormed => (
            extrema.max_x,
            extrema.max_y,
            extrema.max_value,
            extrema.max_value,
        ),
        TemplateMethod::CCoeffNormed => (
            extrema.max_x,
            extrema.max_y,
            extrema.max_value,
            (extrema.max_value + 1.0) / 2.0,
        ),
    };
    if !raw.is_finite() || !score.is_finite() {
        return Err(ImageError::new(
            ImageErrorKind::Internal,
            "template score is non-finite",
        ));
    }
    let right = x
        .checked_add(target.width())
        .ok_or_else(|| ImageError::new(ImageErrorKind::Internal, "match x overflows"))?;
    let bottom = y
        .checked_add(target.height())
        .ok_or_else(|| ImageError::new(ImageErrorKind::Internal, "match y overflows"))?;
    if right > search.width() || bottom > search.height() {
        return Err(ImageError::new(
            ImageErrorKind::Internal,
            "native match location exceeds search image",
        ));
    }
    let raw = f64_to_f32(raw, "template raw value")?;
    let score = f64_to_f32(score.clamp(0.0, 1.0), "template normalized score")?;
    Ok(MatchResult { x, y, score, raw })
}

fn f64_to_f32(value: f64, label: &str) -> Result<f32, ImageError> {
    if value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(ImageError::new(
            ImageErrorKind::Internal,
            format!("{label} exceeds f32"),
        ));
    }
    let result = value as f32;
    if !result.is_finite() {
        return Err(ImageError::new(
            ImageErrorKind::Internal,
            format!("{label} is non-finite"),
        ));
    }
    Ok(result)
}
