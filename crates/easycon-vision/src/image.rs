use std::fmt;
use std::sync::Arc;

use easycon_native_sys::NativeErrorKind;
use easycon_native_sys::codec as native;
use easycon_runtime::CancellationToken;

use crate::{NativeBridgeError, NativePool, VisionError};

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

    pub(crate) const fn into_native(self) -> native::PixelFormat {
        match self {
            Self::Bgr8 => native::PixelFormat::Bgr8,
            Self::Bgra8 => native::PixelFormat::Bgra8,
            Self::Gray8 => native::PixelFormat::Gray8,
        }
    }

    const fn from_native(value: native::PixelFormat) -> Self {
        match value {
            native::PixelFormat::Bgr8 => Self::Bgr8,
            native::PixelFormat::Bgra8 => Self::Bgra8,
            native::PixelFormat::Gray8 => Self::Gray8,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VisionLimits {
    image: native::ImageLimits,
}

impl VisionLimits {
    pub fn try_for_images(
        max_encoded_bytes: usize,
        max_width: u32,
        max_height: u32,
        max_pixels: u64,
        max_decoded_bytes: usize,
        max_stride: usize,
    ) -> Result<Self, ImageError> {
        native::ImageLimits::new(
            max_encoded_bytes,
            max_width,
            max_height,
            max_pixels,
            max_decoded_bytes,
            max_stride,
        )
        .map(|image| Self { image })
        .map_err(ImageError::from_native)
    }

    #[must_use]
    pub const fn max_encoded_bytes(self) -> usize {
        self.image.max_encoded_bytes
    }

    #[must_use]
    pub const fn max_width(self) -> u32 {
        self.image.max_width
    }

    #[must_use]
    pub const fn max_height(self) -> u32 {
        self.image.max_height
    }

    #[must_use]
    pub const fn max_pixels(self) -> u64 {
        self.image.max_pixels
    }

    #[must_use]
    pub const fn max_decoded_bytes(self) -> usize {
        self.image.max_decoded_bytes
    }

    #[must_use]
    pub const fn max_stride(self) -> usize {
        self.image.max_stride
    }

    pub(crate) const fn native_limits(self) -> native::ImageLimits {
        self.image
    }
}

impl Default for VisionLimits {
    fn default() -> Self {
        Self::try_for_images(
            native::MAX_ENCODED_BYTES,
            native::MAX_IMAGE_WIDTH,
            native::MAX_IMAGE_HEIGHT,
            native::MAX_IMAGE_PIXELS,
            native::MAX_DECODED_BYTES,
            native::MAX_IMAGE_STRIDE,
        )
        .expect("compiled image ceilings form valid default limits")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageErrorKind {
    InvalidArgument,
    OutOfRange,
    Overflow,
    ResourceExhausted,
    InvalidImage,
    Native,
    Internal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImageError {
    kind: ImageErrorKind,
    message: String,
    native: Option<NativeBridgeError>,
}

impl ImageError {
    pub(crate) fn new(kind: ImageErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            native: None,
        }
    }

    pub(crate) fn from_native(error: NativeBridgeError) -> Self {
        let kind = match error.kind() {
            NativeErrorKind::InvalidArgument => ImageErrorKind::InvalidArgument,
            NativeErrorKind::OutOfRange => ImageErrorKind::OutOfRange,
            NativeErrorKind::Overflow => ImageErrorKind::Overflow,
            NativeErrorKind::ResourceExhausted => ImageErrorKind::ResourceExhausted,
            NativeErrorKind::InvalidImage => ImageErrorKind::InvalidImage,
            NativeErrorKind::Internal => ImageErrorKind::Internal,
            _ => ImageErrorKind::Native,
        };
        Self {
            kind,
            message: error.message().to_owned(),
            native: Some(error),
        }
    }

    #[must_use]
    pub const fn kind(&self) -> ImageErrorKind {
        self.kind
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn into_native(self) -> Option<NativeBridgeError> {
        self.native
    }
}

impl fmt::Display for ImageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for ImageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.native
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Roi {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Roi {
    pub fn new(x: u32, y: u32, width: u32, height: u32) -> Result<Self, ImageError> {
        if width == 0 || height == 0 {
            return Err(ImageError::new(
                ImageErrorKind::InvalidArgument,
                "ROI dimensions must be non-zero",
            ));
        }
        x.checked_add(width)
            .and_then(|_| y.checked_add(height))
            .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "ROI bounds overflow"))?;
        Ok(Self {
            x,
            y,
            width,
            height,
        })
    }

    #[must_use]
    pub const fn x(self) -> u32 {
        self.x
    }

    #[must_use]
    pub const fn y(self) -> u32 {
        self.y
    }

    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Image {
    pixels: Arc<[u8]>,
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
}

impl Image {
    pub fn new(
        pixels: Arc<[u8]>,
        width: u32,
        height: u32,
        stride: usize,
        format: PixelFormat,
        limits: &VisionLimits,
    ) -> Result<Self, ImageError> {
        validate_layout(pixels.len(), width, height, stride, format, limits)?;
        Ok(Self {
            pixels,
            width,
            height,
            stride,
            format,
        })
    }

    pub(crate) fn decode_direct(encoded: &[u8], limits: &VisionLimits) -> Result<Self, ImageError> {
        if encoded.is_empty() {
            return Err(ImageError::new(
                ImageErrorKind::InvalidArgument,
                "encoded image is empty",
            ));
        }
        if encoded.len() > limits.max_encoded_bytes() {
            return Err(ImageError::new(
                ImageErrorKind::OutOfRange,
                "encoded image exceeds limits",
            ));
        }
        let output = native::decode(encoded, limits.image).map_err(ImageError::from_native)?;
        Self::from_native(output, limits)
    }

    pub(crate) fn encode_png_direct(&self, limits: &VisionLimits) -> Result<Vec<u8>, ImageError> {
        native::encode_png(self.native_view(limits)?, limits.image).map_err(ImageError::from_native)
    }

    pub(crate) fn convert_direct(
        &self,
        output_format: PixelFormat,
        limits: &VisionLimits,
    ) -> Result<Self, ImageError> {
        let output = native::convert(
            self.native_view(limits)?,
            output_format.into_native(),
            limits.image,
        )
        .map_err(ImageError::from_native)?;
        Self::from_native(output, limits)
    }

    pub(crate) fn crop_direct(&self, roi: Roi, limits: &VisionLimits) -> Result<Self, ImageError> {
        let right = roi
            .x
            .checked_add(roi.width)
            .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "ROI right overflows"))?;
        let bottom = roi
            .y
            .checked_add(roi.height)
            .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "ROI bottom overflows"))?;
        if right > self.width || bottom > self.height {
            return Err(ImageError::new(
                ImageErrorKind::OutOfRange,
                "ROI is outside the image",
            ));
        }
        let output = native::crop(
            self.native_view(limits)?,
            roi.x,
            roi.y,
            roi.width,
            roi.height,
            limits.image,
        )
        .map_err(ImageError::from_native)?;
        Self::from_native(output, limits)
    }

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

    #[must_use]
    pub fn row(&self, index: u32) -> Option<&[u8]> {
        if index >= self.height {
            return None;
        }
        let row_bytes = usize::try_from(self.width)
            .ok()?
            .checked_mul(self.format.channels())?;
        let start = usize::try_from(index).ok()?.checked_mul(self.stride)?;
        self.pixels.get(start..start.checked_add(row_bytes)?)
    }

    pub fn rows(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        (0..self.height).map(|index| {
            self.row(index)
                .expect("validated image keeps every row in bounds")
        })
    }

    pub(crate) fn native_view(
        &self,
        limits: &VisionLimits,
    ) -> Result<native::ImageView<'_>, ImageError> {
        native::ImageView::new(
            &self.pixels,
            self.width,
            self.height,
            self.stride,
            self.format.into_native(),
            limits.image,
        )
        .map_err(ImageError::from_native)
    }

    pub(crate) fn from_native(
        output: native::OwnedImage,
        limits: &VisionLimits,
    ) -> Result<Self, ImageError> {
        Self::new(
            Arc::from(output.pixels()),
            output.width(),
            output.height(),
            output.stride(),
            PixelFormat::from_native(output.format()),
            limits,
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Frame {
    image: Image,
    sequence: u64,
    timestamp_ns: u64,
}

impl Frame {
    pub fn new(image: Image, sequence: u64, timestamp_ns: u64) -> Result<Self, ImageError> {
        if sequence == 0 {
            return Err(ImageError::new(
                ImageErrorKind::InvalidArgument,
                "frame sequence must be non-zero",
            ));
        }
        Ok(Self {
            image,
            sequence,
            timestamp_ns,
        })
    }

    #[must_use]
    pub const fn image(&self) -> &Image {
        &self.image
    }

    #[must_use]
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub const fn timestamp_ns(&self) -> u64 {
        self.timestamp_ns
    }

    pub fn crop(
        &self,
        pool: &NativePool,
        cancellation: &CancellationToken,
        roi: Roi,
        limits: &VisionLimits,
    ) -> Result<Image, VisionError> {
        pool.crop(&self.image, roi, limits, cancellation)
    }
}

fn validate_layout(
    length: usize,
    width: u32,
    height: u32,
    stride: usize,
    format: PixelFormat,
    limits: &VisionLimits,
) -> Result<(), ImageError> {
    if width == 0 || height == 0 {
        return Err(ImageError::new(
            ImageErrorKind::InvalidArgument,
            "image dimensions must be non-zero",
        ));
    }
    if width > limits.max_width() || height > limits.max_height() {
        return Err(ImageError::new(
            ImageErrorKind::OutOfRange,
            "image dimensions exceed limits",
        ));
    }
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "pixel count overflows"))?;
    let row_bytes = usize::try_from(width)
        .ok()
        .and_then(|value| value.checked_mul(format.channels()))
        .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "row bytes overflow"))?;
    let required = stride
        .checked_mul(
            usize::try_from(height)
                .map_err(|_| ImageError::new(ImageErrorKind::Overflow, "height overflows"))?,
        )
        .ok_or_else(|| ImageError::new(ImageErrorKind::Overflow, "buffer length overflows"))?;
    if pixels > limits.max_pixels()
        || stride > limits.max_stride()
        || length > limits.max_decoded_bytes()
    {
        return Err(ImageError::new(
            ImageErrorKind::OutOfRange,
            "image layout exceeds limits",
        ));
    }
    if stride < row_bytes || required > length {
        return Err(ImageError::new(
            ImageErrorKind::InvalidArgument,
            "image buffer is shorter than its layout",
        ));
    }
    Ok(())
}
