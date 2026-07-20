#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

struct TestFile(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "easycon-hardware-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create unique test directory");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Drop for TestFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn existing_result_artifact_is_rejected_without_overwrite() {
    let directory = TestDirectory::new("existing-result");
    let output = directory.path().join("unknown.json");
    let sentinel = b"original qualification evidence\n";
    fs::write(&output, sentinel).expect("seed existing evidence");

    let result = Command::new(env!("CARGO_BIN_EXE_easycon-hardware-qualification"))
        .args(["unknown", "--output-dir"])
        .arg(directory.path())
        .output()
        .expect("run non-hardware command");

    assert!(!result.status.success());
    assert_eq!(
        fs::read(&output).expect("read original evidence"),
        sentinel,
        "a repeated run must not truncate an existing raw artifact"
    );
}

#[test]
fn one_run_finalizes_once_and_repeated_run_preserves_bytes() {
    let directory = TestDirectory::new("finalize-once");
    let binary = env!("CARGO_BIN_EXE_easycon-hardware-qualification");

    let first = Command::new(binary)
        .args(["unknown", "--output-dir"])
        .arg(directory.path())
        .output()
        .expect("run first non-hardware command");
    assert!(!first.status.success());

    let output = directory.path().join("unknown.json");
    let marker = directory.path().join(".unknown.in-progress.json");
    let temporary = directory.path().join("unknown.json.tmp");
    let original = fs::read(&output).expect("first run final artifact");
    assert!(!original.is_empty());
    assert!(!marker.exists());
    assert!(!temporary.exists());

    let second = Command::new(binary)
        .args(["unknown", "--output-dir"])
        .arg(directory.path())
        .output()
        .expect("run repeated non-hardware command");
    assert!(!second.status.success());
    assert!(String::from_utf8_lossy(&second.stderr).contains("refusing to overwrite"));
    assert_eq!(
        fs::read(output).expect("preserved final artifact"),
        original
    );
}

#[test]
fn unsafe_command_name_cannot_escape_artifact_directory() {
    let directory = TestDirectory::new("unsafe-command");
    let escape_stem = format!(
        "{}-escape",
        directory
            .path()
            .file_name()
            .expect("test directory name")
            .to_string_lossy()
    );
    let escaped = TestFile(
        directory
            .path()
            .parent()
            .expect("test directory parent")
            .join(format!("{escape_stem}.json")),
    );
    assert!(!escaped.0.exists());

    let result = Command::new(env!("CARGO_BIN_EXE_easycon-hardware-qualification"))
        .args([format!("../{escape_stem}"), "--output-dir".to_owned()])
        .arg(directory.path())
        .output()
        .expect("run unsafe non-hardware command");

    assert!(!result.status.success());
    assert!(!escaped.0.exists());
}
