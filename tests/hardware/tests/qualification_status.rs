#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "easycon-hardware-status-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create unique test directory");
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn unauthorized_amiibo_is_not_run_and_exits_two() {
    let directory = TestDirectory::new("amiibo-not-run");
    let result = Command::new(env!("CARGO_BIN_EXE_easycon-hardware-qualification"))
        .args(["amiibo", "--output-dir"])
        .arg(&directory.0)
        .output()
        .expect("run non-writing Amiibo command");

    assert_eq!(result.status.code(), Some(2));
    let document: serde_json::Value = serde_json::from_slice(
        &fs::read(directory.0.join("amiibo.json")).expect("Amiibo result artifact"),
    )
    .expect("parse Amiibo result artifact");
    assert_eq!(document["execution_status"], "completed");
    assert_eq!(document["qualification_status"], "not_run");
    assert_eq!(document["result"]["write_performed"], false);
}
