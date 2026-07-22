use std::ptr::NonNull;

use crate::capture::{CaptureProfile, CaptureRead};
use crate::{NativeError, NativeErrorKind, ffi};

pub(crate) enum RawDestroyOutcome {
    Consumed {
        diagnostic: Option<NativeError>,
    },
    Unconsumed {
        raw: NonNull<ffi::Capture>,
        diagnostic: NativeError,
    },
}

pub(crate) fn create(
    backend: u32,
    source: &str,
    options: ffi::CaptureOptions,
) -> Result<(NonNull<ffi::Capture>, NonNull<ffi::CaptureInterrupt>), NativeError> {
    let source_length = u64::try_from(source.len())
        .map_err(|_| NativeError::overflow("capture source length does not fit u64"))?;
    let mut capture = std::ptr::null_mut();
    let mut interrupt = std::ptr::null_mut();
    let mut error = ffi::Error::default();
    // SAFETY: source remains readable for source_length bytes; options and all unique out pointers
    // remain valid for the synchronous call, and native zeroes both owner outputs on failure.
    let status = unsafe {
        ffi::easycon_native_capture_create(
            backend,
            source.as_ptr(),
            source_length,
            &options,
            &mut capture,
            &mut interrupt,
            &mut error,
        )
    };
    let result = super::finish(status, error);
    let owners = (NonNull::new(capture), NonNull::new(interrupt));
    match (result, owners) {
        (Ok(()), (Some(capture), Some(interrupt))) => Ok((capture, interrupt)),
        (Ok(()), owners) => {
            cleanup_partial_create(owners);
            Err(NativeError::internal(
                "native capture create returned incomplete owners",
            ))
        }
        (Err(error), owners) => {
            cleanup_partial_create(owners);
            Err(error)
        }
    }
}

pub(crate) fn open(capture: NonNull<ffi::Capture>) -> Result<CaptureProfile, NativeError> {
    let mut profile = ffi::CaptureProfile::default();
    let mut error = ffi::Error::default();
    // SAFETY: capture is a live unique owner used only through &mut CaptureHandle; both outputs are
    // initialized and uniquely borrowed for this synchronous call.
    let status =
        unsafe { ffi::easycon_native_capture_open(capture.as_ptr(), &mut profile, &mut error) };
    super::finish(status, error)?;
    CaptureProfile::from_raw(profile)
}

pub(crate) fn read(capture: NonNull<ffi::Capture>) -> Result<CaptureRead, NativeError> {
    let mut image = ffi::Image::default();
    let mut error = ffi::Error::default();
    // SAFETY: capture has unique mutable Rust access; image and error are initialized unique outs,
    // and any image allocation is owned by image until copy_owned releases it.
    let status =
        unsafe { ffi::easycon_native_capture_read(capture.as_ptr(), &mut image, &mut error) };
    match super::finish(status, error) {
        Ok(()) => crate::call::image::copy_owned(image).map(CaptureRead::Frame),
        Err(error) if error.kind() == NativeErrorKind::NoFrame => {
            debug_assert!(image.data.is_null() && image.length == 0);
            crate::call::image::release_owned(image);
            Ok(CaptureRead::End)
        }
        Err(error) => {
            debug_assert!(image.data.is_null() && image.length == 0);
            crate::call::image::release_owned(image);
            Err(error)
        }
    }
}

pub(crate) fn close(capture: NonNull<ffi::Capture>) -> Result<(), NativeError> {
    let mut error = ffi::Error::default();
    // SAFETY: capture is a live unique owner used only through &mut CaptureHandle and error is a
    // valid unique out pointer for the synchronous call.
    let status = unsafe { ffi::easycon_native_capture_close(capture.as_ptr(), &mut error) };
    super::finish(status, error)
}

pub(crate) fn destroy(capture: NonNull<ffi::Capture>) -> RawDestroyOutcome {
    let mut raw = capture.as_ptr();
    let mut error = ffi::Error::default();
    // SAFETY: raw starts as the sole native owner; native either clears it to acknowledge
    // consumption or leaves it live, and error remains a valid unique output.
    let status = unsafe { ffi::easycon_native_capture_destroy(&mut raw, &mut error) };
    let diagnostic = super::finish(status, error).err();
    match NonNull::new(raw) {
        None => RawDestroyOutcome::Consumed { diagnostic },
        Some(raw) => RawDestroyOutcome::Unconsumed {
            raw,
            diagnostic: diagnostic.unwrap_or_else(|| {
                NativeError::internal(
                    "native capture destroy returned success without consumption acknowledgement",
                )
            }),
        },
    }
}

pub(crate) fn request_stop(interrupt: NonNull<ffi::CaptureInterrupt>) -> Result<(), NativeError> {
    let mut error = ffi::Error::default();
    // SAFETY: the bridge contract permits concurrent calls through this live atomic-only token;
    // error is a thread-local unique output for this synchronous invocation.
    let status =
        unsafe { ffi::easycon_native_capture_interrupt_request(interrupt.as_ptr(), &mut error) };
    super::finish(status, error)
}

pub(crate) fn destroy_interrupt(interrupt: NonNull<ffi::CaptureInterrupt>) {
    let mut raw = interrupt.as_ptr();
    let mut error = ffi::Error::default();
    // SAFETY: Arc<InterruptOwner> makes this Drop the sole remaining native token owner; both
    // mutable outputs are valid and the bridge contract makes token destruction infallible.
    let status = unsafe { ffi::easycon_native_capture_interrupt_destroy(&mut raw, &mut error) };
    let result = super::finish(status, error);
    debug_assert!(
        result.is_ok(),
        "capture interrupt destroy must be infallible"
    );
    debug_assert!(
        raw.is_null(),
        "capture interrupt destroy must consume its owner"
    );
}

fn cleanup_partial_create(
    owners: (
        Option<NonNull<ffi::Capture>>,
        Option<NonNull<ffi::CaptureInterrupt>>,
    ),
) {
    if let Some(capture) = owners.0 {
        let _ = destroy(capture);
    }
    if let Some(interrupt) = owners.1 {
        destroy_interrupt(interrupt);
    }
}

pub(crate) fn discover(backend: u32) -> Result<Vec<(String, String)>, NativeError> {
    let mut raw = std::ptr::null_mut();
    let mut error = ffi::Error::default();
    // SAFETY: raw and error are initialized unique outputs and native zeroes the owner on failure.
    let status =
        unsafe { ffi::easycon_native_capture_discovery_create(backend, &mut raw, &mut error) };
    let result = super::finish(status, error);
    let Some(raw) = NonNull::new(raw) else {
        return match result {
            Ok(()) => Err(NativeError::internal(
                "native capture discovery returned a null owner",
            )),
            Err(error) => Err(error),
        };
    };
    if let Err(error) = result {
        destroy_discovery(raw);
        return Err(error);
    }
    let owner = DiscoveryOwner { raw };
    let mut count = 0_u32;
    let mut error = ffi::Error::default();
    // SAFETY: owner keeps the live discovery handle valid and both outputs are uniquely borrowed.
    let status = unsafe {
        ffi::easycon_native_capture_discovery_count(owner.raw.as_ptr(), &mut count, &mut error)
    };
    super::finish(status, error)?;
    if count > 64 {
        return Err(NativeError::internal(
            "native capture discovery exceeded its fixed count",
        ));
    }
    let mut descriptors = Vec::with_capacity(count as usize);
    for index in 0..count {
        let mut source = BufferOwner::default();
        let mut name = BufferOwner::default();
        let mut error = ffi::Error::default();
        // SAFETY: owner remains live; both buffer owners and error are initialized unique outputs.
        let status = unsafe {
            ffi::easycon_native_capture_discovery_get(
                owner.raw.as_ptr(),
                index,
                &mut source.raw,
                &mut name.raw,
                &mut error,
            )
        };
        super::finish(status, error)?;
        descriptors.push((
            source.copy_utf8("source ID")?,
            name.copy_utf8("display name")?,
        ));
    }
    Ok(descriptors)
}

struct DiscoveryOwner {
    raw: NonNull<ffi::CaptureDiscovery>,
}

impl Drop for DiscoveryOwner {
    fn drop(&mut self) {
        destroy_discovery(self.raw);
    }
}

fn destroy_discovery(discovery: NonNull<ffi::CaptureDiscovery>) {
    let mut raw = discovery.as_ptr();
    let mut error = ffi::Error::default();
    // SAFETY: the guard uniquely owns discovery and both mutable outputs remain valid for the call.
    let status = unsafe { ffi::easycon_native_capture_discovery_destroy(&mut raw, &mut error) };
    let result = super::finish(status, error);
    debug_assert!(
        result.is_ok(),
        "capture discovery destroy must be infallible"
    );
    debug_assert!(
        raw.is_null(),
        "capture discovery destroy must consume its owner"
    );
}

#[derive(Default)]
struct BufferOwner {
    raw: ffi::Buffer,
}

impl BufferOwner {
    fn copy_utf8(&self, label: &str) -> Result<String, NativeError> {
        let length = usize::try_from(self.raw.length)
            .map_err(|_| NativeError::internal(format!("capture {label} length overflows")))?;
        if self.raw.data.is_null() || length == 0 {
            return Err(NativeError::internal(format!(
                "native capture returned an empty {label}"
            )));
        }
        // SAFETY: the discovery get call returned one readable bridge allocation of exactly length
        // bytes and this guard keeps it live until the copied UTF-8 String is complete.
        let bytes = unsafe { std::slice::from_raw_parts(self.raw.data, length) };
        String::from_utf8(bytes.to_vec()).map_err(|_| {
            NativeError::internal(format!("native capture returned an invalid UTF-8 {label}"))
        })
    }
}

impl Drop for BufferOwner {
    fn drop(&mut self) {
        // SAFETY: this guard uniquely owns any bridge buffer returned by discovery get.
        unsafe { ffi::easycon_native_buffer_release(&mut self.raw) };
    }
}
