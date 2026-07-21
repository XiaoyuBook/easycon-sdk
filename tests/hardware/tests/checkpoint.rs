#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "easycon-hardware-checkpoint-{label}-{}-{nonce}",
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
fn empty_synthetic_runs_root_publishes_hardware_unverified_checkpoint() {
    let directory = TestDirectory::new("empty-runs");
    let runs = directory.0.join("runs");
    let output = directory.0.join("output");
    fs::create_dir(&runs).expect("create synthetic runs root");

    let result = Command::new(env!("CARGO_BIN_EXE_easycon-hardware-qualification"))
        .arg("checkpoint")
        .arg("--runs-root")
        .arg(&runs)
        .arg("--output-dir")
        .arg(&output)
        .output()
        .expect("run software-only checkpoint command");

    assert_eq!(result.status.code(), Some(2));
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let document = read_json(&output.join("checkpoint.json"));
    assert_eq!(document["schema_version"], 2);
    assert_eq!(document["execution_status"], "completed");
    assert_eq!(document["qualification_status"], "unverified");
    assert_eq!(document["exit_code"], 2);
    assert_eq!(document["result"]["command"], "checkpoint");
    assert_eq!(document["result"]["resources"], json!({}));
    assert!(document["result"].get("identity_admission").is_none());
    assert!(document["result"].get("operator_observations").is_none());

    let checkpoint = &document["result"]["checkpoint"];
    assert_eq!(checkpoint["status"], "Hardware Unverified");
    assert_eq!(checkpoint["support_matrix_rows_created"], false);
    assert_eq!(checkpoint["open_items"], json!(["O-01", "O-02", "O-04"]));
    for class in ["Observed", "Attested", "Unverified", "Failed", "NotRun"] {
        assert_eq!(checkpoint["evidence"][class], json!([]));
        assert_eq!(checkpoint["summary"]["class_counts"][class], 0);
    }
    assert_eq!(checkpoint["summary"]["total_input_count"], 0);

    let journal =
        fs::read_to_string(output.join("evidence.journal.jsonl")).expect("checkpoint journal");
    assert!(journal.contains("<redacted-path>"));
    assert!(!journal.contains(&directory.0.to_string_lossy().into_owned()));

    verify_completed_transaction(&output, "checkpoint");
}

#[test]
fn duplicate_source_id_publishes_failed_transaction_without_cleanup_claims() {
    let directory = TestDirectory::new("duplicate-source");
    let runs = directory.0.join("runs");
    let attestations = directory.0.join("attestations");
    let output = directory.0.join("output");
    fs::create_dir(&runs).expect("runs root");
    fs::create_dir(&attestations).expect("attestations root");
    fs::create_dir(runs.join("same.json")).expect("vacant synthetic run");
    fs::write(
        attestations.join("same.json"),
        serde_json::to_vec(&json!({
            "schema_version": 1,
            "kind": "easycon_hardware_handoff_attestation",
            "attestation_id": "duplicate-source-attestation",
            "handoff_sha256": "A".repeat(64),
            "claims": ["synthetic.claim"],
        }))
        .expect("attestation JSON"),
    )
    .expect("attestation file");

    let result = Command::new(env!("CARGO_BIN_EXE_easycon-hardware-qualification"))
        .arg("checkpoint")
        .arg("--runs-root")
        .arg(&runs)
        .arg("--attestations-root")
        .arg(&attestations)
        .arg("--output-dir")
        .arg(&output)
        .output()
        .expect("run duplicate-source checkpoint command");

    assert_eq!(result.status.code(), Some(1));
    let document = read_json(&output.join("checkpoint.json"));
    assert_eq!(document["execution_status"], "failed");
    assert_eq!(document["qualification_status"], "failed");
    assert_eq!(document["exit_code"], 1);
    assert_eq!(
        document["result"]["execution_error"]["stage"],
        "checkpoint_build"
    );
    assert_eq!(document["result"]["resources"], json!({}));
    assert!(document["result"].get("cleanup").is_none());
    verify_completed_transaction(&output, "checkpoint");
}

fn verify_completed_transaction(directory: &Path, command: &str) {
    let manifest_bytes = fs::read(directory.join("manifest.json")).expect("manifest bytes");
    let manifest: Value = serde_json::from_slice(&manifest_bytes).expect("manifest JSON");
    assert_eq!(manifest["kind"], "easycon_hardware_evidence_manifest");
    assert_eq!(manifest["command"], command);
    let members = manifest["members"].as_array().expect("manifest members");
    assert!(!members.is_empty());
    for member in members {
        let relative = member["relative_path"].as_str().expect("relative path");
        assert!(!matches!(relative, "manifest.json" | "completion.json"));
        let bytes = fs::read(directory.join(relative)).expect("manifest member bytes");
        assert_eq!(member["bytes"].as_u64(), u64::try_from(bytes.len()).ok());
        assert_eq!(member["sha256"], sha256(&bytes));
    }

    let completion = read_json(&directory.join("completion.json"));
    assert_eq!(completion["kind"], "easycon_hardware_evidence_completion");
    assert_eq!(completion["lease_id"], manifest["lease_id"]);
    assert_eq!(completion["manifest"]["relative_path"], "manifest.json");
    assert_eq!(completion["manifest"]["sha256"], sha256(&manifest_bytes));
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).expect("JSON bytes")).expect("valid JSON")
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:X}", Sha256::digest(bytes))
}
