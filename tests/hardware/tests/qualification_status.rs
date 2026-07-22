#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

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
    assert_eq!(document["exit_code"], 2);
    assert!(document.get("status").is_none());
    assert_eq!(document["result"]["write_performed"], false);
    assert_eq!(document["run"]["journal"], "evidence.journal.jsonl");
    let started = document["run"]["started_unix_ns"]
        .as_u64()
        .expect("start time");
    let ended = document["run"]["ended_unix_ns"].as_u64().expect("end time");
    assert!(ended >= started);
    assert_eq!(document["run"]["journal_projection"]["sealed"], true);

    let journal = fs::read_to_string(directory.0.join("evidence.journal.jsonl"))
        .expect("durable evidence journal");
    let events = journal
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("journal event"))
        .collect::<Vec<_>>();
    assert_eq!(
        events
            .iter()
            .map(|event| event["event"].as_str().expect("event kind"))
            .collect::<Vec<_>>(),
        [
            "run_started",
            "command_dispatch_started",
            "operation_terminal",
            "cleanup_terminal",
            "run_projection_finalized",
            "artifact_finalization_started",
        ]
    );
    assert_eq!(
        document["run"]["journal_projection"]["event_count"],
        events.len()
    );

    let manifest_bytes = fs::read(directory.0.join("manifest.json")).expect("manifest");
    let manifest: serde_json::Value =
        serde_json::from_slice(&manifest_bytes).expect("manifest JSON");
    let members = manifest["members"].as_array().expect("manifest members");
    for member in members {
        let relative = member["relative_path"].as_str().expect("relative member");
        assert!(!matches!(relative, "manifest.json" | "completion.json"));
        let bytes = fs::read(directory.0.join(relative)).expect("manifest member bytes");
        assert_eq!(member["bytes"].as_u64(), Some(bytes.len() as u64));
        assert_eq!(member["sha256"], sha256(&bytes));
    }
    let completion: serde_json::Value =
        serde_json::from_slice(&fs::read(directory.0.join("completion.json")).expect("completion"))
            .expect("completion JSON");
    assert_eq!(completion["manifest"]["relative_path"], "manifest.json");
    assert_eq!(completion["manifest"]["sha256"], sha256(&manifest_bytes));
    assert_eq!(document["auxiliary_artifacts"], serde_json::json!([]));
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:X}", Sha256::digest(bytes))
}
