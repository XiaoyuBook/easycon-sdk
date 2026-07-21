use crate::codec::{ImageLimits, ImageView, OwnedImage};
use crate::operations::{
    ColorResult, EdgeMethod, HsvRange, MatchExtrema, RelativeRect, TemplateMethod,
};
use crate::{NativeError, ffi};

pub(crate) fn match_template(
    search: ImageView<'_>,
    target: ImageView<'_>,
    method: TemplateMethod,
    limits: ImageLimits,
) -> Result<MatchExtrema, NativeError> {
    let raw_search = super::image::raw_view(search)?;
    let raw_target = super::image::raw_view(target)?;
    let raw_limits = super::image::raw_limits(limits)?;
    let mut output = ffi::MatchExtrema::default();
    let mut error = ffi::Error::default();
    // SAFETY: both image views borrow live slices and all out pointers are unique for this call.
    let status = unsafe {
        ffi::easycon_native_match_template(
            &raw_search,
            &raw_target,
            method.into_raw(),
            &raw_limits,
            &mut output,
            &mut error,
        )
    };
    super::finish(status, error)?;

    let max_x = search
        .width()
        .checked_sub(target.width())
        .ok_or_else(|| NativeError::internal("native accepted an oversized target width"))?;
    let max_y = search
        .height()
        .checked_sub(target.height())
        .ok_or_else(|| NativeError::internal("native accepted an oversized target height"))?;
    let min_x = coordinate(output.min_x, max_x, "minimum x")?;
    let min_y = coordinate(output.min_y, max_y, "minimum y")?;
    let result_max_x = coordinate(output.max_x, max_x, "maximum x")?;
    let result_max_y = coordinate(output.max_y, max_y, "maximum y")?;
    if !output.min_value.is_finite() || !output.max_value.is_finite() {
        return Err(NativeError::internal(
            "native returned a non-finite template result",
        ));
    }
    Ok(MatchExtrema {
        min_value: output.min_value,
        max_value: output.max_value,
        min_x,
        min_y,
        max_x: result_max_x,
        max_y: result_max_y,
    })
}

pub(crate) fn preprocess_edge(
    image: ImageView<'_>,
    method: EdgeMethod,
    limits: ImageLimits,
) -> Result<OwnedImage, NativeError> {
    let raw_view = super::image::raw_view(image)?;
    let raw_limits = super::image::raw_limits(limits)?;
    super::image::call_image(|output, error| {
        // SAFETY: raw_view borrows live pixels and both out pointers are unique for the call.
        unsafe {
            ffi::easycon_native_edge_preprocess(
                &raw_view,
                method.into_raw(),
                &raw_limits,
                output,
                error,
            )
        }
    })
}

pub(crate) fn hsv_count(
    image: ImageView<'_>,
    roi: RelativeRect,
    range: HsvRange,
    limits: ImageLimits,
) -> Result<ColorResult, NativeError> {
    let raw_view = super::image::raw_view(image)?;
    let raw_range = range.into_raw();
    let raw_limits = super::image::raw_limits(limits)?;
    let mut output = ffi::ColorResult::default();
    let mut error = ffi::Error::default();
    // SAFETY: raw_view borrows live pixels and all other pointers are unique live values.
    let status = unsafe {
        ffi::easycon_native_hsv_count(
            &raw_view,
            roi.x,
            roi.y,
            roi.width,
            roi.height,
            &raw_range,
            &raw_limits,
            &mut output,
            &mut error,
        )
    };
    super::finish(status, error)?;

    let area = u64::from(roi.width)
        .checked_mul(u64::from(roi.height))
        .ok_or_else(|| NativeError::internal("HSV ROI area overflows"))?;
    if output.count > area {
        return Err(NativeError::internal("native HSV count exceeds ROI area"));
    }
    let bounding_box = match output.has_bbox {
        0 if output.count == 0
            && output.bbox_x == 0
            && output.bbox_y == 0
            && output.bbox_width == 0
            && output.bbox_height == 0 =>
        {
            None
        }
        1 if output.count > 0 => {
            let right = output
                .bbox_x
                .checked_add(output.bbox_width)
                .ok_or_else(|| NativeError::internal("native HSV bbox x overflows"))?;
            let bottom = output
                .bbox_y
                .checked_add(output.bbox_height)
                .ok_or_else(|| NativeError::internal("native HSV bbox y overflows"))?;
            if output.bbox_width == 0
                || output.bbox_height == 0
                || right > roi.width
                || bottom > roi.height
            {
                return Err(NativeError::internal("native HSV bbox is out of bounds"));
            }
            Some(RelativeRect {
                x: output.bbox_x,
                y: output.bbox_y,
                width: output.bbox_width,
                height: output.bbox_height,
            })
        }
        _ => {
            return Err(NativeError::internal(
                "native HSV bbox presence is inconsistent",
            ));
        }
    };
    Ok(ColorResult {
        count: output.count,
        bounding_box,
    })
}

fn coordinate(value: i32, maximum: u32, label: &str) -> Result<u32, NativeError> {
    let value = u32::try_from(value)
        .map_err(|_| NativeError::internal(format!("native {label} is negative")))?;
    if value > maximum {
        return Err(NativeError::internal(format!(
            "native {label} exceeds result bounds"
        )));
    }
    Ok(value)
}
