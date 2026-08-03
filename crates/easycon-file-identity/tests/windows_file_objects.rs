#![cfg(windows)]
#![forbid(unsafe_code)]

use std::fs::{self, File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use easycon_file_identity::{same_file_object, validate_file_object};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};

const ROOT_DIR_FILE_BORROWED_HANDLES: &str = "F0-ROOT-DIR-FILE-BORROWED-HANDLES";

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "easycon-file-identity-integration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after epoch")
                .as_nanos()
        ));
        fs::create_dir(&path).expect("create identity integration directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn guarded_share_mode() -> u32 {
    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
}

fn open_directory(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .share_mode(guarded_share_mode())
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .unwrap_or_else(|error| panic!("open nofollow directory {}: {error}", path.display()))
}

fn open_regular_file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .share_mode(guarded_share_mode())
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .unwrap_or_else(|error| panic!("open nofollow regular file {}: {error}", path.display()))
}

fn volume_or_share_root(path: &Path) -> PathBuf {
    let mut root = PathBuf::new();
    for component in path.components() {
        root.push(component.as_os_str());
        if matches!(component, Component::RootDir) {
            return root;
        }
    }
    panic!(
        "temporary path has no volume or share root: {}",
        path.display()
    );
}

fn assert_true_false_error(retained: &File, current: &File, distinct: &File, unsupported: &File) {
    validate_file_object(retained).expect("validate retained file object");
    validate_file_object(current).expect("validate current file object");
    validate_file_object(distinct).expect("validate distinct file object");
    assert!(same_file_object(retained, current).expect("compare same object"));
    assert!(!same_file_object(retained, distinct).expect("compare distinct objects"));
    assert!(same_file_object(retained, unsupported).is_err());
}

#[test]
fn root_directory_and_regular_file_use_borrowed_nofollow_handles() {
    assert_eq!(
        ROOT_DIR_FILE_BORROWED_HANDLES,
        "F0-ROOT-DIR-FILE-BORROWED-HANDLES"
    );
    let directory = TestDirectory::new();
    let ordinary_directory = directory.0.join("ordinary-directory");
    let distinct_directory = directory.0.join("distinct-directory");
    fs::create_dir(&ordinary_directory).expect("create ordinary directory");
    fs::create_dir(&distinct_directory).expect("create distinct directory");
    let regular_file = directory.0.join("regular-file");
    let linked_file = directory.0.join("linked-file");
    let distinct_file = directory.0.join("distinct-file");
    fs::write(&regular_file, b"same bytes").expect("write regular file");
    fs::hard_link(&regular_file, &linked_file).expect("create file hard link");
    fs::write(&distinct_file, b"same bytes").expect("write distinct file");
    let unsupported = File::open("NUL").expect("open unsupported Windows device handle");

    let volume_or_share_root = volume_or_share_root(&directory.0);
    let root_retained = open_directory(&volume_or_share_root);
    let root_current = open_directory(&volume_or_share_root);
    let root_distinct = open_directory(&ordinary_directory);
    assert_true_false_error(&root_retained, &root_current, &root_distinct, &unsupported);

    let directory_retained = open_directory(&ordinary_directory);
    let directory_current = open_directory(&ordinary_directory);
    let directory_distinct = open_directory(&distinct_directory);
    assert_true_false_error(
        &directory_retained,
        &directory_current,
        &directory_distinct,
        &unsupported,
    );

    let file_retained = open_regular_file(&regular_file);
    let file_current = open_regular_file(&linked_file);
    let file_distinct = open_regular_file(&distinct_file);
    assert_true_false_error(&file_retained, &file_current, &file_distinct, &unsupported);
    assert!(validate_file_object(&unsupported).is_err());
}
