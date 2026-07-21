use crate::codec::{ImageLimits, ImageView, OwnedImage, PixelFormat};
use crate::{NativeError, ffi};

pub(crate) fn decode(encoded: &[u8], limits: ImageLimits) -> Result<OwnedImage, NativeError> {
    let encoded_length = u64::try_from(encoded.len())
        .map_err(|_| NativeError::overflow("encoded image length does not fit u64"))?;
    let limits = raw_limits(limits)?;
    let mut output = ImageOwner::default();
    let mut error = ffi::Error::default();
    // SAFETY: the encoded slice and both unique out values remain alive for this synchronous call.
    let status = unsafe {
        ffi::easycon_native_image_decode(
            encoded.as_ptr(),
            encoded_length,
            &limits,
            &mut output.raw,
            &mut error,
        )
    };
    super::finish(status, error)?;
    output.copy_tight()
}

pub(crate) fn encode_png(
    image: ImageView<'_>,
    limits: ImageLimits,
) -> Result<Vec<u8>, NativeError> {
    let raw_view = raw_view(image)?;
    let limits = raw_limits(limits)?;
    let mut output = BufferOwner::default();
    let mut error = ffi::Error::default();
    // SAFETY: raw_view borrows image pixels and all pointers remain valid until the call returns.
    let status = unsafe {
        ffi::easycon_native_image_encode_png(&raw_view, &limits, &mut output.raw, &mut error)
    };
    super::finish(status, error)?;
    output.copy()
}

pub(crate) fn convert(
    image: ImageView<'_>,
    output_format: PixelFormat,
    limits: ImageLimits,
) -> Result<OwnedImage, NativeError> {
    let raw_view = raw_view(image)?;
    let limits = raw_limits(limits)?;
    call_image(|output, error| {
        // SAFETY: raw_view borrows image pixels and both out pointers are unique for the call.
        unsafe {
            ffi::easycon_native_image_convert(
                &raw_view,
                output_format.into_raw(),
                &limits,
                output,
                error,
            )
        }
    })
}

pub(crate) fn crop(
    image: ImageView<'_>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    limits: ImageLimits,
) -> Result<OwnedImage, NativeError> {
    let raw_view = raw_view(image)?;
    let limits = raw_limits(limits)?;
    call_image(|output, error| {
        // SAFETY: raw_view borrows image pixels and both out pointers are unique for the call.
        unsafe {
            ffi::easycon_native_image_crop(&raw_view, x, y, width, height, &limits, output, error)
        }
    })
}

pub(super) fn call_image(
    call: impl FnOnce(*mut ffi::Image, *mut ffi::Error) -> i32,
) -> Result<OwnedImage, NativeError> {
    let mut output = ImageOwner::default();
    let mut error = ffi::Error::default();
    let status = call(&mut output.raw, &mut error);
    super::finish(status, error)?;
    output.copy_tight()
}

pub(super) fn raw_view(image: ImageView<'_>) -> Result<ffi::ImageView, NativeError> {
    Ok(ffi::ImageView {
        data: image.pixels().as_ptr(),
        length: u64::try_from(image.pixels().len())
            .map_err(|_| NativeError::overflow("image length does not fit u64"))?,
        width: image.width(),
        height: image.height(),
        stride: u64::try_from(image.stride())
            .map_err(|_| NativeError::overflow("image stride does not fit u64"))?,
        pixel_format: image.format().into_raw(),
    })
}

pub(super) fn raw_limits(limits: ImageLimits) -> Result<ffi::ImageLimits, NativeError> {
    Ok(ffi::ImageLimits {
        max_encoded_bytes: u64::try_from(limits.max_encoded_bytes)
            .map_err(|_| NativeError::overflow("encoded limit does not fit u64"))?,
        max_width: limits.max_width,
        max_height: limits.max_height,
        max_pixels: limits.max_pixels,
        max_decoded_bytes: u64::try_from(limits.max_decoded_bytes)
            .map_err(|_| NativeError::overflow("decoded limit does not fit u64"))?,
        max_stride: u64::try_from(limits.max_stride)
            .map_err(|_| NativeError::overflow("stride limit does not fit u64"))?,
    })
}

#[derive(Default)]
struct ImageOwner {
    raw: ffi::Image,
}

impl ImageOwner {
    fn copy_tight(&self) -> Result<OwnedImage, NativeError> {
        let format = PixelFormat::from_raw(self.raw.pixel_format)
            .ok_or_else(|| NativeError::internal("native image returned an unknown format"))?;
        let stride = usize::try_from(self.raw.stride)
            .map_err(|_| NativeError::internal("native image stride does not fit usize"))?;
        let length = usize::try_from(self.raw.length)
            .map_err(|_| NativeError::internal("native image length does not fit usize"))?;
        let row_bytes = usize::try_from(self.raw.width)
            .ok()
            .and_then(|width| width.checked_mul(format.channels()))
            .ok_or_else(|| NativeError::internal("native image row bytes overflow"))?;
        let required = stride
            .checked_mul(
                usize::try_from(self.raw.height)
                    .map_err(|_| NativeError::internal("native image height does not fit usize"))?,
            )
            .ok_or_else(|| NativeError::internal("native image required length overflows"))?;
        if self.raw.data.is_null()
            || self.raw.width == 0
            || self.raw.height == 0
            || stride != row_bytes
            || length != required
        {
            return Err(NativeError::internal(
                "native image returned an invalid tight layout",
            ));
        }
        // SAFETY: a successful native image owns exactly length readable bytes until this guard drops.
        let pixels = unsafe { std::slice::from_raw_parts(self.raw.data, length) }.to_vec();
        Ok(OwnedImage::from_parts(
            pixels,
            self.raw.width,
            self.raw.height,
            stride,
            format,
        ))
    }
}

impl Drop for ImageOwner {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns any native image allocation.
        unsafe { ffi::easycon_native_image_release(&mut self.raw) };
    }
}

#[derive(Default)]
struct BufferOwner {
    raw: ffi::Buffer,
}

impl BufferOwner {
    fn copy(&self) -> Result<Vec<u8>, NativeError> {
        let length = usize::try_from(self.raw.length)
            .map_err(|_| NativeError::internal("native buffer length does not fit usize"))?;
        if self.raw.data.is_null() || length == 0 {
            return Err(NativeError::internal("native returned an empty buffer"));
        }
        // SAFETY: a successful native buffer owns exactly length readable bytes until this guard drops.
        Ok(unsafe { std::slice::from_raw_parts(self.raw.data, length) }.to_vec())
    }
}

impl Drop for BufferOwner {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns any native buffer allocation.
        unsafe { ffi::easycon_native_buffer_release(&mut self.raw) };
    }
}
