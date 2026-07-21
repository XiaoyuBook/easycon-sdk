//! Safe synchronous image calls over the private native boundary.

use crate::{NativeError, call};

pub const MAX_ENCODED_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_IMAGE_WIDTH: u32 = 16_384;
pub const MAX_IMAGE_HEIGHT: u32 = 16_384;
pub const MAX_IMAGE_PIXELS: u64 = 67_108_864;
pub const MAX_DECODED_BYTES: usize = 256 * 1024 * 1024;
pub const MAX_IMAGE_STRIDE: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelFormat {
    Bgr8,
    Bgra8,
    Gray8,
}

impl PixelFormat {
    #[must_use]
    pub const fn channels(self) -> usize {
        match self {
            Self::Bgr8 => 3,
            Self::Bgra8 => 4,
            Self::Gray8 => 1,
        }
    }

    pub(crate) const fn into_raw(self) -> u32 {
        match self {
            Self::Bgr8 => crate::ffi::PIXEL_FORMAT_BGR8,
            Self::Bgra8 => crate::ffi::PIXEL_FORMAT_BGRA8,
            Self::Gray8 => crate::ffi::PIXEL_FORMAT_GRAY8,
        }
    }

    pub(crate) fn from_raw(value: u32) -> Option<Self> {
        match value {
            crate::ffi::PIXEL_FORMAT_BGR8 => Some(Self::Bgr8),
            crate::ffi::PIXEL_FORMAT_BGRA8 => Some(Self::Bgra8),
            crate::ffi::PIXEL_FORMAT_GRAY8 => Some(Self::Gray8),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageLimits {
    pub max_encoded_bytes: usize,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u64,
    pub max_decoded_bytes: usize,
    pub max_stride: usize,
}

impl ImageLimits {
    pub fn new(
        max_encoded_bytes: usize,
        max_width: u32,
        max_height: u32,
        max_pixels: u64,
        max_decoded_bytes: usize,
        max_stride: usize,
    ) -> Result<Self, NativeError> {
        if max_encoded_bytes == 0
            || max_width == 0
            || max_height == 0
            || max_pixels == 0
            || max_decoded_bytes == 0
            || max_stride == 0
        {
            return Err(NativeError::invalid_argument(
                "image limits must be non-zero",
            ));
        }
        if max_encoded_bytes > MAX_ENCODED_BYTES
            || max_width > MAX_IMAGE_WIDTH
            || max_height > MAX_IMAGE_HEIGHT
            || max_pixels > MAX_IMAGE_PIXELS
            || max_decoded_bytes > MAX_DECODED_BYTES
            || max_stride > MAX_IMAGE_STRIDE
        {
            return Err(NativeError::out_of_range(
                "image limits exceed hard ceilings",
            ));
        }
        Ok(Self {
            max_encoded_bytes,
            max_width,
            max_height,
            max_pixels,
            max_decoded_bytes,
            max_stride,
        })
    }
}

#[derive(Clone, Copy)]
pub struct ImageView<'a> {
    pixels: &'a [u8],
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
}

impl<'a> ImageView<'a> {
    pub fn new(
        pixels: &'a [u8],
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        limits: ImageLimits,
    ) -> Result<Self, NativeError> {
        validate_layout(pixels.len(), width, height, stride, format, limits)?;
        Ok(Self {
            pixels,
            width,
            height,
            stride,
            format,
        })
    }

    pub(crate) const fn pixels(self) -> &'a [u8] {
        self.pixels
    }

    pub(crate) const fn width(self) -> u32 {
        self.width
    }

    pub(crate) const fn height(self) -> u32 {
        self.height
    }

    pub(crate) const fn stride(self) -> usize {
        self.stride
    }

    pub(crate) const fn format(self) -> PixelFormat {
        self.format
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnedImage {
    pixels: Vec<u8>,
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
}

impl OwnedImage {
    #[must_use]
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    #[must_use]
    pub const fn width(&self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(&self) -> u32 {
        self.height
    }

    #[must_use]
    pub const fn stride(&self) -> usize {
        self.stride
    }

    #[must_use]
    pub const fn format(&self) -> PixelFormat {
        self.format
    }

    pub(crate) fn from_parts(
        pixels: Vec<u8>,
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
    ) -> Self {
        Self {
            pixels,
            width,
            height,
            stride,
            format,
        }
    }
}

pub fn decode(encoded: &[u8], limits: ImageLimits) -> Result<OwnedImage, NativeError> {
    call::image::decode(encoded, limits)
}

pub fn encode_png(image: ImageView<'_>, limits: ImageLimits) -> Result<Vec<u8>, NativeError> {
    call::image::encode_png(image, limits)
}

pub fn convert(
    image: ImageView<'_>,
    output_format: PixelFormat,
    limits: ImageLimits,
) -> Result<OwnedImage, NativeError> {
    call::image::convert(image, output_format, limits)
}

pub fn crop(
    image: ImageView<'_>,
    x: u32,
    y: u32,
    width: u32,
    height: u32,
    limits: ImageLimits,
) -> Result<OwnedImage, NativeError> {
    call::image::crop(image, x, y, width, height, limits)
}

fn validate_layout(
    length: usize,
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    limits: ImageLimits,
) -> Result<(), NativeError> {
    if width == 0 || height == 0 {
        return Err(NativeError::invalid_argument(
            "image dimensions must be non-zero",
        ));
    }
    if width > limits.max_width || height > limits.max_height {
        return Err(NativeError::out_of_range("image dimensions exceed limits"));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| NativeError::overflow("image pixel count overflows"))?;
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(format.channels()))
        .ok_or_else(|| NativeError::overflow("image row bytes overflow"))?;
    let required = stride
        .checked_mul(usize::try_from(height).map_err(|_| NativeError::overflow("height"))?)
        .ok_or_else(|| NativeError::overflow("image buffer length overflows"))?;
    if pixels > limits.max_pixels
        || stride < row_bytes
        || stride > limits.max_stride
        || required > length
        || length > limits.max_decoded_bytes
    {
        return Err(NativeError::invalid_argument("image layout is invalid"));
    }
    Ok(())
}
