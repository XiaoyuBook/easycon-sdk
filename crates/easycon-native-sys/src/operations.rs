//! Safe synchronous matching, edge, and color calls over the private bridge.

use crate::codec::{ImageLimits, ImageView, OwnedImage};
use crate::{NativeError, call, ffi};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TemplateMethod {
    SqDiffNormed,
    CCorrNormed,
    CCoeffNormed,
}

impl TemplateMethod {
    pub(crate) const fn into_raw(self) -> u32 {
        match self {
            Self::SqDiffNormed => ffi::TEMPLATE_SQDIFF_NORMED,
            Self::CCorrNormed => ffi::TEMPLATE_CCORR_NORMED,
            Self::CCoeffNormed => ffi::TEMPLATE_CCOEFF_NORMED,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EdgeMethod {
    Xy,
    Laplacian,
}

impl EdgeMethod {
    pub(crate) const fn into_raw(self) -> u32 {
        match self {
            Self::Xy => ffi::EDGE_XY,
            Self::Laplacian => ffi::EDGE_LAPLACIAN,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchExtrema {
    pub min_value: f64,
    pub max_value: f64,
    pub min_x: u32,
    pub min_y: u32,
    pub max_x: u32,
    pub max_y: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HsvRange {
    h_min: u32,
    h_max: u32,
    s_min: u32,
    s_max: u32,
    v_min: u32,
    v_max: u32,
}

impl HsvRange {
    pub fn new(
        h_min: u32,
        h_max: u32,
        s_min: u32,
        s_max: u32,
        v_min: u32,
        v_max: u32,
    ) -> Result<Self, NativeError> {
        if h_min > 179 || h_max > 179 || s_min > 255 || s_max > 255 || v_min > 255 || v_max > 255 {
            return Err(NativeError::out_of_range("HSV range exceeds OpenCV scale"));
        }
        if s_min > s_max || v_min > v_max {
            return Err(NativeError::invalid_argument("HSV S/V range is reversed"));
        }
        Ok(Self {
            h_min,
            h_max,
            s_min,
            s_max,
            v_min,
            v_max,
        })
    }

    pub(crate) const fn into_raw(self) -> ffi::HsvRange {
        ffi::HsvRange {
            h_min: self.h_min,
            h_max: self.h_max,
            s_min: self.s_min,
            s_max: self.s_max,
            v_min: self.v_min,
            v_max: self.v_max,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelativeRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorResult {
    pub count: u64,
    pub bounding_box: Option<RelativeRect>,
}

pub fn match_template(
    search: ImageView<'_>,
    target: ImageView<'_>,
    method: TemplateMethod,
    limits: ImageLimits,
) -> Result<MatchExtrema, NativeError> {
    call::vision::match_template(search, target, method, limits)
}

pub fn preprocess_edge(
    image: ImageView<'_>,
    method: EdgeMethod,
    limits: ImageLimits,
) -> Result<OwnedImage, NativeError> {
    call::vision::preprocess_edge(image, method, limits)
}

pub fn hsv_count(
    image: ImageView<'_>,
    roi: RelativeRect,
    range: HsvRange,
    limits: ImageLimits,
) -> Result<ColorResult, NativeError> {
    call::vision::hsv_count(image, roi, range, limits)
}
