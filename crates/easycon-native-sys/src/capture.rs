//! Safe ownership for the private native capture bridge.

use std::cell::Cell;
use std::fmt;
use std::marker::PhantomData;
use std::ptr::NonNull;
use std::sync::Arc;

use crate::call::capture::RawDestroyOutcome;
use crate::codec::{ImageLimits, OwnedImage, PixelFormat};
use crate::{NativeError, call, ffi};

pub const MAX_CAPTURE_SOURCE_BYTES: usize = 32_768;
pub const MAX_CAPTURE_TIMEOUT_NS: u64 = 60_000_000_000;
pub const MAX_CAPTURE_FRAMES: u32 = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureBackend {
    File,
    DirectShow,
    MediaFoundation,
}

impl CaptureBackend {
    const fn into_raw(self) -> u32 {
        match self {
            Self::File => ffi::CAPTURE_BACKEND_FILE,
            Self::DirectShow => ffi::CAPTURE_BACKEND_DIRECTSHOW,
            Self::MediaFoundation => ffi::CAPTURE_BACKEND_MEDIA_FOUNDATION,
        }
    }

    fn from_raw(value: u32) -> Option<Self> {
        match value {
            ffi::CAPTURE_BACKEND_FILE => Some(Self::File),
            ffi::CAPTURE_BACKEND_DIRECTSHOW => Some(Self::DirectShow),
            ffi::CAPTURE_BACKEND_MEDIA_FOUNDATION => Some(Self::MediaFoundation),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureDescriptor {
    backend: CaptureBackend,
    source_id: String,
    display_name: String,
}

impl CaptureDescriptor {
    #[must_use]
    pub const fn backend(&self) -> CaptureBackend {
        self.backend
    }

    #[must_use]
    pub fn source_id(&self) -> &str {
        &self.source_id
    }

    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
}

pub fn discover(backend: CaptureBackend) -> Result<Vec<CaptureDescriptor>, NativeError> {
    if backend == CaptureBackend::File {
        return Err(NativeError::invalid_argument(
            "file capture does not support device discovery",
        ));
    }
    call::capture::discover(backend.into_raw())?
        .into_iter()
        .map(|(source_id, display_name)| {
            if source_id.is_empty()
                || source_id.len() > 4096
                || source_id.as_bytes().contains(&0)
                || display_name.is_empty()
                || display_name.len() > 1024
                || display_name.as_bytes().contains(&0)
            {
                return Err(NativeError::internal(
                    "native capture returned an invalid bounded descriptor",
                ));
            }
            Ok(CaptureDescriptor {
                backend,
                source_id,
                display_name,
            })
        })
        .collect()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CaptureOptions {
    image_limits: ImageLimits,
    open_timeout_ns: u64,
    read_timeout_ns: u64,
    max_frames: u32,
}

impl CaptureOptions {
    pub fn new(
        image_limits: ImageLimits,
        open_timeout_ns: u64,
        read_timeout_ns: u64,
        max_frames: u32,
    ) -> Result<Self, NativeError> {
        if open_timeout_ns == 0
            || open_timeout_ns > MAX_CAPTURE_TIMEOUT_NS
            || read_timeout_ns == 0
            || read_timeout_ns > MAX_CAPTURE_TIMEOUT_NS
            || max_frames == 0
            || max_frames > MAX_CAPTURE_FRAMES
        {
            return Err(NativeError::out_of_range(
                "capture options exceed fixed bounds",
            ));
        }
        Ok(Self {
            image_limits,
            open_timeout_ns,
            read_timeout_ns,
            max_frames,
        })
    }

    fn into_raw(self) -> Result<ffi::CaptureOptions, NativeError> {
        Ok(ffi::CaptureOptions {
            image_limits: ffi::ImageLimits {
                max_encoded_bytes: u64::try_from(self.image_limits.max_encoded_bytes)
                    .map_err(|_| NativeError::overflow("encoded limit does not fit u64"))?,
                max_width: self.image_limits.max_width,
                max_height: self.image_limits.max_height,
                max_pixels: self.image_limits.max_pixels,
                max_decoded_bytes: u64::try_from(self.image_limits.max_decoded_bytes)
                    .map_err(|_| NativeError::overflow("decoded limit does not fit u64"))?,
                max_stride: u64::try_from(self.image_limits.max_stride)
                    .map_err(|_| NativeError::overflow("stride limit does not fit u64"))?,
            },
            open_timeout_ns: self.open_timeout_ns,
            read_timeout_ns: self.read_timeout_ns,
            max_frames: self.max_frames,
            reserved: 0,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureProfile {
    backend: CaptureBackend,
    width: u32,
    height: u32,
    stride: usize,
    pixel_format: PixelFormat,
    frame_interval_ns: Option<u64>,
}

impl CaptureProfile {
    pub(crate) fn from_raw(raw: ffi::CaptureProfile) -> Result<Self, NativeError> {
        let backend = CaptureBackend::from_raw(raw.backend)
            .ok_or_else(|| NativeError::internal("native capture returned an unknown backend"))?;
        let pixel_format = PixelFormat::from_raw(raw.pixel_format).ok_or_else(|| {
            NativeError::internal("native capture returned an unknown pixel format")
        })?;
        let stride = usize::try_from(raw.stride)
            .map_err(|_| NativeError::internal("native capture stride does not fit usize"))?;
        let row_bytes = usize::try_from(raw.width)
            .ok()
            .and_then(|width| width.checked_mul(pixel_format.channels()))
            .ok_or_else(|| NativeError::internal("native capture row bytes overflow"))?;
        if raw.width == 0 || raw.height == 0 || stride != row_bytes {
            return Err(NativeError::internal(
                "native capture returned an invalid tight profile",
            ));
        }
        Ok(Self {
            backend,
            width: raw.width,
            height: raw.height,
            stride,
            pixel_format,
            frame_interval_ns: (raw.frame_interval_ns != 0).then_some(raw.frame_interval_ns),
        })
    }

    #[must_use]
    pub const fn backend(&self) -> CaptureBackend {
        self.backend
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
    pub const fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    #[must_use]
    pub const fn frame_interval_ns(&self) -> Option<u64> {
        self.frame_interval_ns
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum CaptureRead {
    Frame(OwnedImage),
    End,
}

/// Unique capture owner. It may move to one read worker, but it is not shareable.
///
/// ```compile_fail
/// fn assert_sync<T: Sync>() {}
/// assert_sync::<easycon_native_sys::capture::CaptureHandle>();
/// ```
pub struct CaptureHandle {
    raw: Option<NonNull<ffi::Capture>>,
    not_sync: PhantomData<Cell<()>>,
}

// SAFETY: the bridge owns no native thread and every operation requires `&mut self`; moving the
// sole owner between Rust workers cannot create concurrent open/read/close/destroy access.
#[allow(unsafe_code)]
unsafe impl Send for CaptureHandle {}

impl fmt::Debug for CaptureHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureHandle")
            .field("armed", &self.raw.is_some())
            .finish()
    }
}

impl CaptureHandle {
    pub fn create(
        backend: CaptureBackend,
        source: &str,
        options: CaptureOptions,
    ) -> Result<(Self, CaptureInterrupt), NativeError> {
        if source.is_empty()
            || source.len() > MAX_CAPTURE_SOURCE_BYTES
            || source.as_bytes().contains(&0)
        {
            return Err(NativeError::invalid_argument(
                "capture source must be bounded NUL-free UTF-8",
            ));
        }
        let (capture, interrupt) =
            call::capture::create(backend.into_raw(), source, options.into_raw()?)?;
        Ok((
            Self {
                raw: Some(capture),
                not_sync: PhantomData,
            },
            CaptureInterrupt {
                owner: Arc::new(InterruptOwner { raw: interrupt }),
            },
        ))
    }

    pub fn open(&mut self) -> Result<CaptureProfile, NativeError> {
        call::capture::open(self.armed()?)
    }

    pub fn read(&mut self) -> Result<CaptureRead, NativeError> {
        call::capture::read(self.armed()?)
    }

    pub fn close(&mut self) -> Result<(), NativeError> {
        call::capture::close(self.armed()?)
    }

    #[must_use]
    pub fn destroy(mut self) -> CaptureDestroyOutcome {
        let raw = self.raw.take().expect("CaptureHandle owner is armed");
        match call::capture::destroy(raw) {
            RawDestroyOutcome::Consumed { diagnostic } => {
                CaptureDestroyOutcome::Consumed { diagnostic }
            }
            RawDestroyOutcome::Unconsumed { raw, diagnostic } => {
                CaptureDestroyOutcome::Unconsumed {
                    handle: Self {
                        raw: Some(raw),
                        not_sync: PhantomData,
                    },
                    diagnostic,
                }
            }
        }
    }

    fn armed(&self) -> Result<NonNull<ffi::Capture>, NativeError> {
        self.raw
            .ok_or_else(|| NativeError::internal("capture owner is not armed"))
    }
}

impl Drop for CaptureHandle {
    fn drop(&mut self) {
        let Some(raw) = self.raw.take() else {
            return;
        };
        let outcome = call::capture::destroy(raw);
        debug_assert!(
            matches!(outcome, RawDestroyOutcome::Consumed { .. }),
            "capture fallback destroy did not consume its owner"
        );
    }
}

#[derive(Debug)]
pub enum CaptureDestroyOutcome {
    Consumed {
        diagnostic: Option<NativeError>,
    },
    Unconsumed {
        handle: CaptureHandle,
        diagnostic: NativeError,
    },
}

#[derive(Clone)]
pub struct CaptureInterrupt {
    owner: Arc<InterruptOwner>,
}

struct InterruptOwner {
    raw: NonNull<ffi::CaptureInterrupt>,
}

// SAFETY: the opaque token exposes only an atomic stop store; C++ holds a ref-counted control
// separate from the capture owner, and destruction happens once after the final Rust Arc drops.
#[allow(unsafe_code)]
unsafe impl Send for InterruptOwner {}
// SAFETY: request_stop is explicitly concurrent and touches only the bridge atomic control.
#[allow(unsafe_code)]
unsafe impl Sync for InterruptOwner {}

impl CaptureInterrupt {
    pub fn request_stop(&self) -> Result<(), NativeError> {
        call::capture::request_stop(self.owner.raw)
    }
}

impl fmt::Debug for CaptureInterrupt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureInterrupt")
            .field("strong_count", &Arc::strong_count(&self.owner))
            .finish()
    }
}

impl Drop for InterruptOwner {
    fn drop(&mut self) {
        call::capture::destroy_interrupt(self.raw);
    }
}

#[cfg(test)]
mod tests {
    use super::{CaptureHandle, CaptureInterrupt};

    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn capture_thread_capabilities_are_explicit() {
        assert_send::<CaptureHandle>();
        assert_send_sync::<CaptureInterrupt>();
    }
}
