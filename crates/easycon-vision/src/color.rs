use easycon_native_sys::operations as native;
use easycon_runtime::CancellationToken;

use crate::{Image, ImageError, ImageErrorKind, NativePool, Roi, VisionError, VisionLimits};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HsvRange {
    native: native::HsvRange,
}

impl HsvRange {
    pub fn new(
        h_min: u32,
        h_max: u32,
        s_min: u32,
        s_max: u32,
        v_min: u32,
        v_max: u32,
    ) -> Result<Self, ImageError> {
        native::HsvRange::new(h_min, h_max, s_min, s_max, v_min, v_max)
            .map(|native| Self { native })
            .map_err(ImageError::from_native)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorStatistics {
    count: u64,
    ratio: f32,
    bounding_box: Option<Roi>,
}

impl ColorStatistics {
    #[must_use]
    pub const fn count(self) -> u64 {
        self.count
    }

    #[must_use]
    pub const fn ratio(self) -> f32 {
        self.ratio
    }

    #[must_use]
    pub const fn bounding_box(self) -> Option<Roi> {
        self.bounding_box
    }

    pub fn meets(self, threshold: f32) -> Result<bool, ImageError> {
        if !threshold.is_finite() {
            return Err(ImageError::new(
                ImageErrorKind::InvalidArgument,
                "color ratio threshold must be finite",
            ));
        }
        if !(0.0..=1.0).contains(&threshold) {
            return Err(ImageError::new(
                ImageErrorKind::OutOfRange,
                "color ratio threshold must be within 0.0..=1.0",
            ));
        }
        Ok(self.ratio >= threshold)
    }
}

impl Image {
    pub fn hsv_statistics(
        &self,
        pool: &NativePool,
        cancellation: &CancellationToken,
        roi: Roi,
        range: HsvRange,
        limits: &VisionLimits,
    ) -> Result<ColorStatistics, VisionError> {
        pool.hsv_statistics(self, roi, range, limits, cancellation)
    }

    pub(crate) fn hsv_statistics_direct(
        &self,
        roi: Roi,
        range: HsvRange,
        limits: &VisionLimits,
    ) -> Result<ColorStatistics, ImageError> {
        let right = roi
            .x()
            .checked_add(roi.width())
            .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "HSV ROI x overflows"))?;
        let bottom = roi
            .y()
            .checked_add(roi.height())
            .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "HSV ROI y overflows"))?;
        if right > self.width() || bottom > self.height() {
            return Err(ImageError::new(
                ImageErrorKind::OutOfRange,
                "HSV ROI is outside the image",
            ));
        }
        let area = u64::from(roi.width())
            .checked_mul(u64::from(roi.height()))
            .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "HSV ROI area overflows"))?;
        let result = native::hsv_count(
            self.native_view(limits)?,
            native::RelativeRect {
                x: roi.x(),
                y: roi.y(),
                width: roi.width(),
                height: roi.height(),
            },
            range.native,
            limits.native_limits(),
        )
        .map_err(ImageError::from_native)?;
        if result.count > area {
            return Err(ImageError::new(
                ImageErrorKind::Internal,
                "native HSV count exceeds ROI area",
            ));
        }
        let ratio = (result.count as f64) / (area as f64);
        if !ratio.is_finite() || !(0.0..=1.0).contains(&ratio) {
            return Err(ImageError::new(
                ImageErrorKind::Internal,
                "HSV ratio is invalid",
            ));
        }
        let ratio = ratio as f32;
        if !ratio.is_finite() {
            return Err(ImageError::new(
                ImageErrorKind::Internal,
                "HSV ratio does not fit f32",
            ));
        }
        let bounding_box = result
            .bounding_box
            .map(|relative| {
                let x = roi.x().checked_add(relative.x).ok_or_else(|| {
                    ImageError::new(ImageErrorKind::Internal, "HSV bbox x overflows")
                })?;
                let y = roi.y().checked_add(relative.y).ok_or_else(|| {
                    ImageError::new(ImageErrorKind::Internal, "HSV bbox y overflows")
                })?;
                let absolute = Roi::new(x, y, relative.width, relative.height)?;
                let absolute_right =
                    absolute.x().checked_add(absolute.width()).ok_or_else(|| {
                        ImageError::new(ImageErrorKind::Internal, "HSV bbox right overflows")
                    })?;
                let absolute_bottom =
                    absolute.y().checked_add(absolute.height()).ok_or_else(|| {
                        ImageError::new(ImageErrorKind::Internal, "HSV bbox bottom overflows")
                    })?;
                if absolute_right > right || absolute_bottom > bottom {
                    return Err(ImageError::new(
                        ImageErrorKind::Internal,
                        "HSV bbox exceeds ROI",
                    ));
                }
                Ok(absolute)
            })
            .transpose()?;
        if (result.count == 0) != bounding_box.is_none() {
            return Err(ImageError::new(
                ImageErrorKind::Internal,
                "HSV count and bbox presence differ",
            ));
        }
        Ok(ColorStatistics {
            count: result.count,
            ratio,
            bounding_box,
        })
    }
}
