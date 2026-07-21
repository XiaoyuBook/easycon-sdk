#![cfg(windows)]
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ID_128, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct HighResolutionFileIdentity {
    pub volume_serial_number: u64,
    pub file_id: u128,
}

pub fn high_resolution_file_identity(file: &File) -> Result<HighResolutionFileIdentity, io::Error> {
    query_file_identity(file)
}

#[allow(unsafe_code)]
fn query_file_identity(file: &File) -> Result<HighResolutionFileIdentity, io::Error> {
    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    // SAFETY: `file` owns a live Windows handle for the duration of this call, `info` is a
    // correctly sized writable FILE_ID_INFO buffer, and the API does not retain either pointer.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            size_of::<FILE_ID_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(HighResolutionFileIdentity {
        volume_serial_number: info.VolumeSerialNumber,
        file_id: u128::from_le_bytes(info.FileId.Identifier),
    })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn identity_is_bound_to_the_borrowed_file_object() {
        let directory = std::env::temp_dir().join(format!(
            "easycon-hardware-file-id-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        fs::create_dir(&directory).expect("create identity test directory");
        let original = directory.join("original");
        let linked = directory.join("linked");
        let distinct = directory.join("distinct");
        fs::write(&original, b"same bytes").expect("write original");
        fs::hard_link(&original, &linked).expect("create hard link");
        fs::write(&distinct, b"same bytes").expect("write distinct object");

        let original_id =
            high_resolution_file_identity(&File::open(&original).expect("open original handle"))
                .expect("read original identity");
        let linked_id =
            high_resolution_file_identity(&File::open(&linked).expect("open linked handle"))
                .expect("read linked identity");
        let distinct_id =
            high_resolution_file_identity(&File::open(&distinct).expect("open distinct handle"))
                .expect("read distinct identity");

        assert_eq!(original_id, linked_id);
        assert_ne!(original_id, distinct_id);
        fs::remove_dir_all(directory).expect("remove identity test directory");
    }
}
