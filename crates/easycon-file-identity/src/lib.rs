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
struct HighResolutionFileIdentity {
    volume_serial_number: u64,
    file_id: u128,
}

#[derive(Clone, Copy, Debug)]
struct RawFileIdInfo {
    volume_serial_number: u64,
    identifier: Option<[u8; 16]>,
}

trait FileIdentityQuery {
    fn query(&mut self, file: &File) -> io::Result<RawFileIdInfo>;

    fn compare(
        &mut self,
        left: HighResolutionFileIdentity,
        right: HighResolutionFileIdentity,
    ) -> bool {
        left == right
    }
}

struct SystemFileIdentityQuery;

impl FileIdentityQuery for SystemFileIdentityQuery {
    fn query(&mut self, file: &File) -> io::Result<RawFileIdInfo> {
        query_file_id_info(file)
    }
}

/// Validates that a borrowed, still-live file object has a usable high-resolution identity.
pub fn validate_file_object(file: &File) -> io::Result<()> {
    validate_file_object_with(file, &mut SystemFileIdentityQuery)
}

/// Compares two borrowed, still-live file objects within one call.
pub fn same_file_object(left: &File, right: &File) -> io::Result<bool> {
    same_file_object_with(left, right, &mut SystemFileIdentityQuery)
}

fn validate_file_object_with(file: &File, query: &mut impl FileIdentityQuery) -> io::Result<()> {
    validated_identity_with(file, query).map(|_| ())
}

fn same_file_object_with(
    left: &File,
    right: &File,
    query: &mut impl FileIdentityQuery,
) -> io::Result<bool> {
    let left_identity = validated_identity_with(left, query)?;
    let right_identity = validated_identity_with(right, query)?;
    Ok(query.compare(left_identity, right_identity))
}

fn validated_identity_with(
    file: &File,
    query: &mut impl FileIdentityQuery,
) -> io::Result<HighResolutionFileIdentity> {
    let RawFileIdInfo {
        volume_serial_number,
        identifier,
    } = query.query(file)?;
    let identifier =
        identifier.ok_or_else(|| invalid_identity("FILE_ID_INFO result is incomplete"))?;
    if volume_serial_number == 0 {
        return Err(invalid_identity(
            "FILE_ID_INFO volume serial number is zero",
        ));
    }
    if identifier_is_all_zero(&identifier) {
        return Err(invalid_identity("FILE_ID_INFO identifier is all zero"));
    }
    Ok(HighResolutionFileIdentity {
        volume_serial_number,
        file_id: u128::from_le_bytes(identifier),
    })
}

fn identifier_is_all_zero(identifier: &[u8; 16]) -> bool {
    identifier.iter().all(|byte| *byte == 0)
}

fn invalid_identity(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[allow(unsafe_code)]
fn query_file_id_info(file: &File) -> io::Result<RawFileIdInfo> {
    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    let buffer_size =
        u32::try_from(size_of::<FILE_ID_INFO>()).expect("FILE_ID_INFO size fits in u32");
    // SAFETY: `file.as_raw_handle()` is borrowed from a live `File` for this one call;
    // `FileIdInfo` selects the exact `FILE_ID_INFO` layout; `info` is an aligned, writable
    // buffer of exactly `buffer_size` bytes; and the API retains neither the handle nor pointer.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            std::ptr::from_mut(&mut info).cast(),
            buffer_size,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(RawFileIdInfo {
        volume_serial_number: info.VolumeSerialNumber,
        identifier: Some(info.FileId.Identifier),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::fs;
    use std::path::PathBuf;
    use std::rc::Rc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    const NO_DETACHED_PUBLIC_AUTHORITY: &str = "F0-NO-DETACHED-PUBLIC-AUTHORITY";
    const QUERY_QUALITY_FAIL_CLOSED: &str = "F0-QUERY-QUALITY-FAIL-CLOSED";
    const TWO_LIVE_HANDLE_DROP_TRACE: &str = "F0-TWO-LIVE-HANDLE-DROP-TRACE";

    struct StubQuery {
        responses: VecDeque<io::Result<RawFileIdInfo>>,
        query_count: usize,
        compare_count: usize,
    }

    impl FileIdentityQuery for StubQuery {
        fn query(&mut self, _file: &File) -> io::Result<RawFileIdInfo> {
            self.query_count += 1;
            self.responses.pop_front().expect("stub query response")
        }

        fn compare(
            &mut self,
            left: HighResolutionFileIdentity,
            right: HighResolutionFileIdentity,
        ) -> bool {
            self.compare_count += 1;
            left == right
        }
    }

    fn stub(responses: impl IntoIterator<Item = io::Result<RawFileIdInfo>>) -> StubQuery {
        StubQuery {
            responses: responses.into_iter().collect(),
            query_count: 0,
            compare_count: 0,
        }
    }

    fn valid_raw_identity(volume_serial_number: u64, identifier: [u8; 16]) -> RawFileIdInfo {
        RawFileIdInfo {
            volume_serial_number,
            identifier: Some(identifier),
        }
    }

    fn rejected_quality_results() -> [io::Result<RawFileIdInfo>; 5] {
        [
            Err(io::Error::from_raw_os_error(1)),
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "FileIdInfo unsupported",
            )),
            Ok(valid_raw_identity(0, [1; 16])),
            Ok(valid_raw_identity(1, [0; 16])),
            Ok(RawFileIdInfo {
                volume_serial_number: 1,
                identifier: None,
            }),
        ]
    }

    fn inert_file() -> File {
        File::open("NUL").expect("open inert Windows device handle")
    }

    #[test]
    fn public_authority_accepts_only_borrowed_files_and_opaque_results() {
        assert_eq!(
            NO_DETACHED_PUBLIC_AUTHORITY,
            "F0-NO-DETACHED-PUBLIC-AUTHORITY"
        );
        let _: fn(&File) -> io::Result<()> = validate_file_object;
        let _: fn(&File, &File) -> io::Result<bool> = same_file_object;
    }

    #[test]
    fn query_quality_failures_reach_both_safe_api_semantics_without_a_fallback() {
        assert_eq!(QUERY_QUALITY_FAIL_CLOSED, "F0-QUERY-QUALITY-FAIL-CLOSED");
        let left = inert_file();
        let right = inert_file();
        let valid = valid_raw_identity(1, [1; 16]);

        for response in rejected_quality_results() {
            let mut query = stub([response]);
            assert!(validate_file_object_with(&left, &mut query).is_err());
            assert_eq!(query.query_count, 1);
            assert_eq!(query.compare_count, 0);
        }
        for response in rejected_quality_results() {
            let mut query = stub([response]);
            assert!(same_file_object_with(&left, &right, &mut query).is_err());
            assert_eq!(query.query_count, 1);
            assert_eq!(query.compare_count, 0);
        }
        for response in rejected_quality_results() {
            let mut query = stub([Ok(valid), response]);
            assert!(same_file_object_with(&left, &right, &mut query).is_err());
            assert_eq!(query.query_count, 2);
            assert_eq!(query.compare_count, 0);
        }

        let mut low_half_only = [0; 16];
        low_half_only[0] = 1;
        let mut query = stub([Ok(valid_raw_identity(1, low_half_only))]);
        assert!(validate_file_object_with(&left, &mut query).is_ok());
        assert_eq!(query.query_count, 1);
        assert_eq!(query.compare_count, 0);
    }

    #[test]
    fn comparison_uses_volume_and_all_128_identifier_bits() {
        let file = inert_file();
        let mut low_bit = [0; 16];
        low_bit[0] = 1;
        let mut other_low_bit = low_bit;
        other_low_bit[1] = 1;
        let mut high_bit = low_bit;
        high_bit[15] = 1;
        let cases = [
            (
                valid_raw_identity(7, low_bit),
                valid_raw_identity(7, low_bit),
                true,
            ),
            (
                valid_raw_identity(7, low_bit),
                valid_raw_identity(8, low_bit),
                false,
            ),
            (
                valid_raw_identity(7, low_bit),
                valid_raw_identity(7, other_low_bit),
                false,
            ),
            (
                valid_raw_identity(7, low_bit),
                valid_raw_identity(7, high_bit),
                false,
            ),
        ];

        for (left, right, expected) in cases {
            let mut query = stub([Ok(left), Ok(right)]);
            assert_eq!(
                same_file_object_with(&file, &file, &mut query).expect("compare identities"),
                expected
            );
            assert_eq!(query.query_count, 2);
            assert_eq!(query.compare_count, 1);
        }
    }

    #[test]
    fn same_reference_is_queried_twice_before_comparison() {
        let file = inert_file();
        let identity = valid_raw_identity(7, [1; 16]);
        let mut query = stub([Ok(identity), Ok(identity)]);
        assert!(same_file_object_with(&file, &file, &mut query).expect("compare same reference"));
        assert_eq!(query.query_count, 2);
        assert_eq!(query.compare_count, 1);
    }

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "easycon-file-identity-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after epoch")
                    .as_nanos()
            ));
            fs::create_dir(&path).expect("create identity test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct TrackedFile {
        file: File,
        drop_event: &'static str,
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl Drop for TrackedFile {
        fn drop(&mut self) {
            self.events.borrow_mut().push(self.drop_event);
        }
    }

    struct RecordingSystemQuery<'a> {
        left: &'a File,
        right: &'a File,
        events: Rc<RefCell<Vec<&'static str>>>,
    }

    impl FileIdentityQuery for RecordingSystemQuery<'_> {
        fn query(&mut self, file: &File) -> io::Result<RawFileIdInfo> {
            let event = if std::ptr::eq(file, self.left) {
                "left-query"
            } else {
                assert!(std::ptr::eq(file, self.right));
                "right-query"
            };
            self.events.borrow_mut().push(event);
            query_file_id_info(file)
        }

        fn compare(
            &mut self,
            left: HighResolutionFileIdentity,
            right: HighResolutionFileIdentity,
        ) -> bool {
            self.events.borrow_mut().push("compare");
            left == right
        }
    }

    fn assert_two_live_handle_trace(use_hard_link: bool, expected: bool) {
        let directory = TestDirectory::new(if use_hard_link { "same" } else { "distinct" });
        let retained_path = directory.0.join("retained");
        let current_path = directory.0.join("current");
        fs::write(&retained_path, b"same bytes").expect("write retained object");
        if use_hard_link {
            fs::hard_link(&retained_path, &current_path).expect("link current object");
        } else {
            fs::write(&current_path, b"same bytes").expect("write distinct current object");
        }

        let events = Rc::new(RefCell::new(Vec::new()));
        let retained = TrackedFile {
            file: File::open(&retained_path).expect("open retained handle"),
            drop_event: "drop-retained",
            events: Rc::clone(&events),
        };
        let current = TrackedFile {
            file: File::open(&current_path).expect("open current handle"),
            drop_event: "drop-current",
            events: Rc::clone(&events),
        };
        let mut query = RecordingSystemQuery {
            left: &retained.file,
            right: &current.file,
            events: Rc::clone(&events),
        };

        let result = same_file_object_with(&retained.file, &current.file, &mut query)
            .expect("compare live handles");
        assert_eq!(result, expected);
        events.borrow_mut().push("return");
        drop(query);
        drop(current);
        drop(retained);
        assert_eq!(
            events.borrow().as_slice(),
            [
                "left-query",
                "right-query",
                "compare",
                "return",
                "drop-current",
                "drop-retained",
            ]
        );
    }

    #[test]
    fn two_live_handle_trace_covers_same_and_distinct_objects() {
        assert_eq!(TWO_LIVE_HANDLE_DROP_TRACE, "F0-TWO-LIVE-HANDLE-DROP-TRACE");
        assert_two_live_handle_trace(true, true);
        assert_two_live_handle_trace(false, false);
    }
}
