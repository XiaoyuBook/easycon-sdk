use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::ffi::OsStrExt;
#[cfg(windows)]
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

use crate::artifact::{
    CheckpointRunEvidence, CompletedRunEvidence, RunDirectoryStatus, checkpoint_run_evidence,
    read_checkpoint_input_file,
};
use crate::provenance::sha256_bytes;
use easycon_hardware_qualification::option_value;

use super::{RunControl, pre_harness_cancellation_result};

const MAX_ROOT_ENTRIES: usize = 4_096;
const MAX_ATTESTATION_BYTES: u64 = 1_048_576;
const MAX_ATTESTATION_CLAIMS: usize = 256;
const EVIDENCE_CLASSES: [&str; 5] = ["Observed", "Attested", "Unverified", "Failed", "NotRun"];

#[derive(Clone, Debug)]
pub(crate) struct CheckpointInputs {
    runs_root: PathBuf,
    attestations_root: Option<PathBuf>,
}

impl CheckpointInputs {
    pub(crate) fn parse(arguments: &[String], output_dir: &Path) -> Result<Self, String> {
        validate_argument_shape(arguments)?;
        let runs_root = option_value(arguments, "--runs-root")?
            .ok_or_else(|| "checkpoint requires --runs-root PATH".to_owned())?;
        let output = option_value(arguments, "--output-dir")?
            .ok_or_else(|| "checkpoint requires --output-dir PATH".to_owned())?;
        if Path::new(output) != output_dir {
            return Err(
                "checkpoint output path did not match artifact reservation input".to_owned(),
            );
        }
        let runs_root = canonical_input_root(Path::new(runs_root), "runs root")?;
        let attestations_root = option_value(arguments, "--attestations-root")?
            .map(|value| canonical_input_root(Path::new(value), "attestations root"))
            .transpose()?;
        let output = canonical_output_path(output_dir)?;
        if paths_overlap(&runs_root, &output)
            || attestations_root
                .as_ref()
                .is_some_and(|root| paths_overlap(root, &output))
        {
            return Err("checkpoint input and output paths must be disjoint".to_owned());
        }
        if attestations_root
            .as_ref()
            .is_some_and(|root| paths_overlap(root, &runs_root))
        {
            return Err("checkpoint input roots must be disjoint".to_owned());
        }
        root_entries(&runs_root)?;
        if let Some(root) = &attestations_root {
            root_entries(root)?;
        }
        Ok(Self {
            runs_root,
            attestations_root,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EvidenceClass {
    Observed,
    Attested,
    Unverified,
    Failed,
    NotRun,
}

impl EvidenceClass {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "Observed",
            Self::Attested => "Attested",
            Self::Unverified => "Unverified",
            Self::Failed => "Failed",
            Self::NotRun => "NotRun",
        }
    }
}

struct EvidenceRecord {
    class: EvidenceClass,
    source_kind: &'static str,
    source_id: String,
    value: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DocumentOutcome {
    Passed,
    QualificationFailed,
    ExecutionFailed,
    NotRun,
    Unverified,
    Cancelled,
}

impl DocumentOutcome {
    fn parse(document: &Value) -> Option<Self> {
        if document["schema_version"].as_u64() != Some(2) {
            return None;
        }
        Self::parse_fields(document)
    }

    fn parse_projection(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.len() != 4
            || ![
                "execution_status",
                "qualification_status",
                "exit_code",
                "provenance_trusted",
            ]
            .iter()
            .all(|field| object.contains_key(*field))
            || value["provenance_trusted"].as_bool().is_none()
        {
            return None;
        }
        Self::parse_fields(value)
    }

    fn parse_fields(value: &Value) -> Option<Self> {
        match (
            value["execution_status"].as_str(),
            value["qualification_status"].as_str(),
            value["exit_code"].as_i64(),
        ) {
            (Some("completed"), Some("passed"), Some(0)) => Some(Self::Passed),
            (Some("completed"), Some("failed"), Some(1)) => Some(Self::QualificationFailed),
            (Some("failed"), Some("failed"), Some(1)) => Some(Self::ExecutionFailed),
            (Some("completed"), Some("not_run"), Some(2)) => Some(Self::NotRun),
            (Some("completed"), Some("unverified"), Some(2)) => Some(Self::Unverified),
            (Some("cancelled"), Some("unverified"), Some(130)) => Some(Self::Cancelled),
            _ => None,
        }
    }

    const fn evidence_class(self) -> EvidenceClass {
        match self {
            Self::Passed => EvidenceClass::Observed,
            Self::QualificationFailed | Self::ExecutionFailed => EvidenceClass::Failed,
            Self::NotRun => EvidenceClass::NotRun,
            Self::Unverified | Self::Cancelled => EvidenceClass::Unverified,
        }
    }
}

pub(crate) fn run(
    inputs: &CheckpointInputs,
    control: &mut RunControl<'_>,
) -> Result<Value, String> {
    match build_checkpoint(inputs, || control.checkpoint("checkpoint_scan_interrupt")) {
        Ok(checkpoint) => Ok(json!({
            "command": "checkpoint",
            "checkpoint": checkpoint,
            "resources": {},
        })),
        Err(CheckpointBuildError::Interrupted(failure)) => {
            Ok(pre_harness_cancellation_result("checkpoint", failure))
        }
        Err(CheckpointBuildError::Failed(error)) => Ok(json!({
            "command": "checkpoint",
            "execution_error": {
                "stage": "checkpoint_build",
                "message": error,
            },
            "resources": {},
        })),
    }
}

enum CheckpointBuildError {
    Failed(String),
    Interrupted(super::CommandFailure),
}

fn build_checkpoint(
    inputs: &CheckpointInputs,
    mut checkpoint: impl FnMut() -> Result<(), super::CommandFailure>,
) -> Result<Value, CheckpointBuildError> {
    checkpoint().map_err(CheckpointBuildError::Interrupted)?;
    let mut records = scan_runs(&inputs.runs_root, &mut checkpoint)?;
    if let Some(root) = &inputs.attestations_root {
        records.extend(scan_attestations(root, &mut checkpoint)?);
    }
    checkpoint().map_err(CheckpointBuildError::Interrupted)?;

    records.sort_by(|left, right| {
        (left.class, left.source_kind, left.source_id.as_str()).cmp(&(
            right.class,
            right.source_kind,
            right.source_id.as_str(),
        ))
    });
    let mut source_keys = BTreeSet::new();
    for record in &records {
        if !source_keys.insert(record.source_id.clone()) {
            return Err(CheckpointBuildError::Failed(
                "checkpoint source IDs are not unique".to_owned(),
            ));
        }
    }

    let mut evidence = Map::new();
    for class in EVIDENCE_CLASSES {
        evidence.insert(class.to_owned(), Value::Array(Vec::new()));
    }
    for record in &records {
        evidence
            .get_mut(record.class.as_str())
            .and_then(Value::as_array_mut)
            .expect("fixed evidence class array")
            .push(record.value.clone());
    }
    let summary = summary_json(&records);
    Ok(json!({
        "schema_version": 1,
        "kind": "easycon_phase2b_hardware_unverified_checkpoint",
        "status": "Hardware Unverified",
        "open_items": ["O-01", "O-02", "O-04"],
        "support_matrix_rows_created": false,
        "evidence": Value::Object(evidence),
        "summary": summary,
    }))
}

fn scan_runs(
    root: &Path,
    checkpoint: &mut impl FnMut() -> Result<(), super::CommandFailure>,
) -> Result<Vec<EvidenceRecord>, CheckpointBuildError> {
    let entries = root_entries(root).map_err(CheckpointBuildError::Failed)?;
    let mut records = Vec::with_capacity(entries.len());
    let mut lease_ids = BTreeSet::new();
    for (name, path) in entries {
        checkpoint().map_err(CheckpointBuildError::Interrupted)?;
        let source_id = safe_source_id(&name);
        let metadata = fs::symlink_metadata(&path);
        let is_directory = metadata
            .as_ref()
            .is_ok_and(|metadata| metadata.is_dir() && !metadata_is_reparse(metadata));
        if !is_directory {
            records.push(failed_input_record(
                "run_directory",
                source_id,
                "run_entry_not_directory",
                name_snapshot_sha256(&name),
                false,
            ));
            continue;
        }

        let evidence = checkpoint_run_evidence(&path);
        let record = classify_run(source_id, evidence, &mut lease_ids)?;
        records.push(record);
    }
    Ok(records)
}

fn classify_run(
    source_id: String,
    evidence: CheckpointRunEvidence,
    lease_ids: &mut BTreeSet<String>,
) -> Result<EvidenceRecord, CheckpointBuildError> {
    let disk_status = evidence.inspection.status.as_str();
    let common = json!({
        "evidence_class": Value::Null,
        "source_kind": "run_directory",
        "source_id": source_id,
        "disk_status": disk_status,
        "disk_reason": evidence.inspection.reason,
        "run_snapshot_sha256": evidence.snapshot_sha256,
        "snapshot_complete": evidence.snapshot_complete,
    });
    match evidence.inspection.status {
        RunDirectoryStatus::Vacant => {
            Ok(with_class(EvidenceClass::NotRun, common, "run_directory"))
        }
        RunDirectoryStatus::Incomplete => Ok(with_class(
            EvidenceClass::Unverified,
            common,
            "run_directory",
        )),
        RunDirectoryStatus::Polluted => {
            Ok(with_class(EvidenceClass::Failed, common, "run_directory"))
        }
        RunDirectoryStatus::Completed => {
            let Some(completed) = evidence.completed else {
                return Ok(failed_completed_record(
                    common,
                    "completed_evidence_missing",
                ));
            };
            if !lease_ids.insert(completed.lease_id.clone()) {
                return Err(CheckpointBuildError::Failed(
                    "completed run lease IDs are not unique".to_owned(),
                ));
            }
            classify_completed_run(common, completed)
        }
    }
}

fn classify_completed_run(
    mut common: Value,
    completed: CompletedRunEvidence,
) -> Result<EvidenceRecord, CheckpointBuildError> {
    let document = &completed.document;
    let outcome = DocumentOutcome::parse(document);
    let provenance_trusted = document["provenance"]["trusted"].as_bool();
    let identity_valid = document["command"].as_str() == Some(completed.command.as_str())
        && document["run"]["lease_id"].as_str() == Some(completed.lease_id.as_str())
        && provenance_trusted.is_some();
    common["command"] = json!(completed.command);
    common["lease_id"] = json!(completed.lease_id);
    common["primary"] = json!({
        "relative_path": completed.primary_relative_path,
        "bytes": completed.document_bytes,
        "sha256": completed.document_sha256,
    });
    common["journal"] = json!({
        "relative_path": crate::journal::JOURNAL_FILE_NAME,
        "bytes": completed.journal_bytes,
        "sha256": completed.journal_sha256,
    });
    common["manifest"] = json!({
        "relative_path": crate::artifact::MANIFEST_FILE_NAME,
        "bytes": completed.manifest_bytes,
        "sha256": completed.manifest_sha256,
        "members": completed.manifest_members,
    });
    common["completion"] = json!({
        "relative_path": crate::artifact::COMPLETION_FILE_NAME,
        "bytes": completed.completion_bytes,
        "sha256": completed.completion_sha256,
    });
    if let Some(outcome) = outcome {
        common["document_outcome"] = json!({
            "execution_status": document["execution_status"],
            "qualification_status": document["qualification_status"],
            "exit_code": document["exit_code"],
            "provenance_trusted": provenance_trusted,
        });
        if outcome == DocumentOutcome::Passed && provenance_trusted != Some(true) {
            return Ok(failed_completed_record(
                common,
                "passed_provenance_untrusted",
            ));
        }
    }
    if outcome.is_none() || !identity_valid {
        return Ok(failed_completed_record(
            common,
            "completed_document_invalid",
        ));
    }
    let outcome = outcome.expect("validated document outcome");
    Ok(with_class(
        outcome.evidence_class(),
        common,
        "run_directory",
    ))
}

fn scan_attestations(
    root: &Path,
    checkpoint: &mut impl FnMut() -> Result<(), super::CommandFailure>,
) -> Result<Vec<EvidenceRecord>, CheckpointBuildError> {
    let entries = root_entries(root).map_err(CheckpointBuildError::Failed)?;
    let mut records = Vec::with_capacity(entries.len());
    let mut attestation_ids = BTreeSet::new();
    for (name, path) in entries {
        checkpoint().map_err(CheckpointBuildError::Interrupted)?;
        let source_id = safe_source_id(&name);
        let metadata = fs::symlink_metadata(&path);
        let is_regular_json = metadata
            .as_ref()
            .is_ok_and(|metadata| metadata.is_file() && !metadata_is_reparse(metadata))
            && path.extension().and_then(OsStr::to_str) == Some("json");
        if !is_regular_json {
            records.push(failed_input_record(
                "handoff_attestation",
                source_id,
                "attestation_not_regular_json",
                name_snapshot_sha256(&name),
                false,
            ));
            continue;
        }
        let bytes = match read_checkpoint_input_file(&path, MAX_ATTESTATION_BYTES) {
            Ok(bytes) => bytes,
            Err(_) => {
                records.push(failed_input_record(
                    "handoff_attestation",
                    source_id,
                    "attestation_unreadable_or_too_large",
                    name_snapshot_sha256(&name),
                    false,
                ));
                continue;
            }
        };
        let attestation_sha256 = sha256_bytes(&bytes);
        let Some(parsed) = parse_attestation(&bytes) else {
            records.push(failed_input_record(
                "handoff_attestation",
                source_id,
                "attestation_schema_invalid",
                attestation_sha256,
                true,
            ));
            continue;
        };
        let attestation_id = parsed["attestation_id"]
            .as_str()
            .expect("validated attestation ID")
            .to_owned();
        if !attestation_ids.insert(attestation_id.clone()) {
            return Err(CheckpointBuildError::Failed(
                "attestation IDs are not unique".to_owned(),
            ));
        }
        let value = json!({
            "evidence_class": EvidenceClass::Attested.as_str(),
            "source_kind": "handoff_attestation",
            "source_id": source_id,
            "attestation_id": attestation_id,
            "attestation_sha256": attestation_sha256,
            "handoff_sha256": parsed["handoff_sha256"],
            "claim_ids": parsed["claims"],
        });
        records.push(EvidenceRecord {
            class: EvidenceClass::Attested,
            source_kind: "handoff_attestation",
            source_id: value["source_id"].as_str().expect("source ID").to_owned(),
            value,
        });
    }
    Ok(records)
}

fn parse_attestation(bytes: &[u8]) -> Option<Value> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object()?;
    let required = [
        "schema_version",
        "kind",
        "attestation_id",
        "handoff_sha256",
        "claims",
    ];
    if object.len() != required.len()
        || required.iter().any(|field| !object.contains_key(*field))
        || value["schema_version"].as_u64() != Some(1)
        || value["kind"] != "easycon_hardware_handoff_attestation"
        || !value["attestation_id"].as_str().is_some_and(safe_token)
        || !value["handoff_sha256"].as_str().is_some_and(valid_sha256)
    {
        return None;
    }
    let claims = value["claims"].as_array()?;
    if claims.len() > MAX_ATTESTATION_CLAIMS {
        return None;
    }
    let mut unique = BTreeSet::new();
    if claims.iter().any(|claim| {
        claim
            .as_str()
            .filter(|claim| safe_token(claim))
            .is_none_or(|claim| !unique.insert(claim))
    }) {
        return None;
    }
    Some(value)
}

fn with_class(class: EvidenceClass, mut value: Value, source_kind: &'static str) -> EvidenceRecord {
    value["evidence_class"] = json!(class.as_str());
    EvidenceRecord {
        class,
        source_kind,
        source_id: value["source_id"]
            .as_str()
            .expect("source record has an ID")
            .to_owned(),
        value,
    }
}

fn failed_completed_record(mut common: Value, reason: &'static str) -> EvidenceRecord {
    common["validation_reason"] = json!(reason);
    with_class(EvidenceClass::Failed, common, "run_directory")
}

fn failed_input_record(
    source_kind: &'static str,
    source_id: String,
    reason: &'static str,
    snapshot_sha256: String,
    snapshot_complete: bool,
) -> EvidenceRecord {
    let value = json!({
        "evidence_class": EvidenceClass::Failed.as_str(),
        "source_kind": source_kind,
        "source_id": source_id,
        "snapshot_sha256": snapshot_sha256,
        "snapshot_complete": snapshot_complete,
        "validation_reason": reason,
    });
    EvidenceRecord {
        class: EvidenceClass::Failed,
        source_kind,
        source_id: value["source_id"].as_str().expect("source ID").to_owned(),
        value,
    }
}

fn summary_json(records: &[EvidenceRecord]) -> Value {
    let class_counts = EVIDENCE_CLASSES
        .into_iter()
        .map(|class| {
            (
                class.to_owned(),
                json!(
                    records
                        .iter()
                        .filter(|record| record.class.as_str() == class)
                        .count()
                ),
            )
        })
        .collect::<Map<_, _>>();
    let disk_count = |status: &str| {
        records
            .iter()
            .filter(|record| {
                record.source_kind == "run_directory" && record.value["disk_status"] == status
            })
            .count()
    };
    json!({
        "total_input_count": records.len(),
        "class_counts": Value::Object(class_counts),
        "disk_counts": {
            "completed": disk_count("completed"),
            "incomplete": disk_count("incomplete"),
            "polluted": disk_count("polluted"),
            "vacant": disk_count("vacant"),
        },
        "attestation_input_count": records.iter().filter(|record| record.source_kind == "handoff_attestation").count(),
    })
}

fn projection_record_is_valid(class: EvidenceClass, value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(source_kind) = value["source_kind"].as_str() else {
        return false;
    };
    match source_kind {
        "handoff_attestation" if class == EvidenceClass::Attested => {
            let Some(claims) = value["claim_ids"].as_array() else {
                return false;
            };
            let mut unique = BTreeSet::new();
            object.len() == 7
                && [
                    "evidence_class",
                    "source_kind",
                    "source_id",
                    "attestation_id",
                    "attestation_sha256",
                    "handoff_sha256",
                    "claim_ids",
                ]
                .iter()
                .all(|field| object.contains_key(*field))
                && value["attestation_id"].as_str().is_some_and(safe_token)
                && value["attestation_sha256"]
                    .as_str()
                    .is_some_and(valid_sha256)
                && value["handoff_sha256"].as_str().is_some_and(valid_sha256)
                && claims.len() <= MAX_ATTESTATION_CLAIMS
                && claims.iter().all(|claim| {
                    claim
                        .as_str()
                        .is_some_and(|claim| safe_token(claim) && unique.insert(claim))
                })
        }
        "handoff_attestation" => {
            class == EvidenceClass::Failed
                && failed_input_projection_is_valid(
                    object,
                    value,
                    &[
                        "attestation_not_regular_json",
                        "attestation_unreadable_or_too_large",
                        "attestation_schema_invalid",
                    ],
                )
        }
        "run_directory" => run_projection_is_valid(class, object, value),
        _ => false,
    }
}

fn failed_input_projection_is_valid(
    object: &Map<String, Value>,
    value: &Value,
    reasons: &[&str],
) -> bool {
    object.len() == 6
        && [
            "evidence_class",
            "source_kind",
            "source_id",
            "snapshot_sha256",
            "snapshot_complete",
            "validation_reason",
        ]
        .iter()
        .all(|field| object.contains_key(*field))
        && value["snapshot_sha256"].as_str().is_some_and(valid_sha256)
        && value["snapshot_complete"]
            .as_bool()
            .is_some_and(|complete| {
                complete == (value["validation_reason"] == "attestation_schema_invalid")
            })
        && value["validation_reason"]
            .as_str()
            .is_some_and(|reason| reasons.contains(&reason))
}

fn run_projection_is_valid(
    class: EvidenceClass,
    object: &Map<String, Value>,
    value: &Value,
) -> bool {
    let Some(disk_status) = value.get("disk_status").and_then(Value::as_str) else {
        return class == EvidenceClass::Failed
            && failed_input_projection_is_valid(object, value, &["run_entry_not_directory"]);
    };
    let common_valid = [
        "evidence_class",
        "source_kind",
        "source_id",
        "disk_status",
        "disk_reason",
        "run_snapshot_sha256",
        "snapshot_complete",
    ]
    .iter()
    .all(|field| object.contains_key(*field))
        && value["disk_reason"].as_str().is_some_and(safe_token)
        && value["run_snapshot_sha256"]
            .as_str()
            .is_some_and(valid_sha256)
        && value["snapshot_complete"].as_bool().is_some();
    if !common_valid {
        return false;
    }
    match disk_status {
        "vacant" => class == EvidenceClass::NotRun && object.len() == 7,
        "incomplete" => class == EvidenceClass::Unverified && object.len() == 7,
        "polluted" => class == EvidenceClass::Failed && object.len() == 7,
        "completed" => completed_projection_is_valid(class, object, value),
        _ => false,
    }
}

fn completed_projection_is_valid(
    class: EvidenceClass,
    object: &Map<String, Value>,
    value: &Value,
) -> bool {
    let validation_reason = value.get("validation_reason").and_then(Value::as_str);
    if validation_reason == Some("completed_evidence_missing") {
        return class == EvidenceClass::Failed
            && object.len() == 8
            && value["snapshot_complete"] == false;
    }
    let Some(command) = value["command"].as_str() else {
        return false;
    };
    let evidence_valid = crate::artifact::ArtifactReservation::validate_command(command).is_ok()
        && value["lease_id"].as_str().is_some_and(valid_lease_id)
        && value["snapshot_complete"] == true
        && artifact_projection_is_valid(&value["primary"], &format!("{command}.json"))
        && artifact_projection_is_valid(&value["journal"], crate::journal::JOURNAL_FILE_NAME)
        && manifest_projection_is_valid(&value["manifest"])
        && artifact_projection_is_valid(
            &value["completion"],
            crate::artifact::COMPLETION_FILE_NAME,
        );
    if !evidence_valid {
        return false;
    }
    let projected_outcome = value
        .get("document_outcome")
        .and_then(DocumentOutcome::parse_projection);
    let expected_len =
        13 + usize::from(projected_outcome.is_some()) + usize::from(validation_reason.is_some());
    if object.len() != expected_len {
        return false;
    }
    match validation_reason {
        None => projected_outcome.is_some_and(|outcome| {
            class == outcome.evidence_class()
                && (outcome != DocumentOutcome::Passed
                    || value["document_outcome"]["provenance_trusted"] == true)
        }),
        Some("passed_provenance_untrusted") => {
            class == EvidenceClass::Failed
                && projected_outcome == Some(DocumentOutcome::Passed)
                && value["document_outcome"]["provenance_trusted"] == false
        }
        Some("completed_document_invalid") => class == EvidenceClass::Failed,
        _ => false,
    }
}

fn artifact_projection_is_valid(value: &Value, expected_relative_path: &str) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == 3
            && object.contains_key("relative_path")
            && object.contains_key("bytes")
            && object.contains_key("sha256")
            && value["relative_path"] == expected_relative_path
            && value["bytes"].as_u64().is_some()
            && value["sha256"].as_str().is_some_and(valid_sha256)
    })
}

fn manifest_projection_is_valid(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(members) = value["members"].as_array() else {
        return false;
    };
    let mut prior_relative_path = None;
    object.len() == 4
        && object.contains_key("relative_path")
        && object.contains_key("bytes")
        && object.contains_key("sha256")
        && object.contains_key("members")
        && value["relative_path"] == crate::artifact::MANIFEST_FILE_NAME
        && value["bytes"].as_u64().is_some()
        && value["sha256"].as_str().is_some_and(valid_sha256)
        && !members.is_empty()
        && members.iter().all(|member| {
            let Some(member_object) = member.as_object() else {
                return false;
            };
            let Some(relative_path) = member["relative_path"].as_str() else {
                return false;
            };
            let sorted = prior_relative_path
                .as_ref()
                .is_none_or(|prior| prior < &relative_path);
            prior_relative_path = Some(relative_path);
            member_object.len() == 5
                && ["role", "relative_path", "bytes", "sha256", "same_file_as"]
                    .iter()
                    .all(|field| member_object.contains_key(*field))
                && member["role"].as_str().is_some_and(safe_token)
                && safe_token(relative_path)
                && member["bytes"].as_u64().is_some()
                && member["sha256"].as_str().is_some_and(valid_sha256)
                && (member["same_file_as"].is_null()
                    || member["same_file_as"].as_str().is_some_and(safe_token))
                && sorted
        })
}

fn valid_lease_id(value: &str) -> bool {
    let mut fields = value.split('-');
    let process = fields.next().unwrap_or_default();
    let timestamp = fields.next().unwrap_or_default();
    fields.next().is_none()
        && process.len() == 8
        && timestamp.len() == 32
        && process.bytes().all(|byte| byte.is_ascii_hexdigit())
        && timestamp.bytes().all(|byte| byte.is_ascii_hexdigit())
}

pub(crate) fn projection_is_valid(checkpoint: &Value) -> bool {
    let Some(object) = checkpoint.as_object() else {
        return false;
    };
    let required = [
        "schema_version",
        "kind",
        "status",
        "open_items",
        "support_matrix_rows_created",
        "evidence",
        "summary",
    ];
    if object.len() != required.len()
        || required.iter().any(|field| !object.contains_key(*field))
        || checkpoint["schema_version"].as_u64() != Some(1)
        || checkpoint["kind"] != "easycon_phase2b_hardware_unverified_checkpoint"
        || checkpoint["status"] != "Hardware Unverified"
        || checkpoint["open_items"] != json!(["O-01", "O-02", "O-04"])
        || checkpoint["support_matrix_rows_created"] != false
    {
        return false;
    }
    let Some(evidence) = checkpoint["evidence"].as_object() else {
        return false;
    };
    if evidence.len() != EVIDENCE_CLASSES.len()
        || EVIDENCE_CLASSES
            .iter()
            .any(|class| !evidence.contains_key(*class))
    {
        return false;
    }
    let mut records = Vec::new();
    let mut source_keys = BTreeSet::new();
    let mut lease_ids = BTreeSet::new();
    let mut attestation_ids = BTreeSet::new();
    for class in EVIDENCE_CLASSES {
        let Some(values) = evidence[class].as_array() else {
            return false;
        };
        let mut prior = None;
        for value in values {
            if value["evidence_class"] != class
                || !value["source_kind"]
                    .as_str()
                    .is_some_and(|kind| matches!(kind, "run_directory" | "handoff_attestation"))
                || !value["source_id"].as_str().is_some_and(safe_token)
            {
                return false;
            }
            let key = format!(
                "{}:{}",
                value["source_kind"].as_str().expect("checked source kind"),
                value["source_id"].as_str().expect("checked source ID")
            );
            if prior.as_ref().is_some_and(|prior| prior >= &key) {
                return false;
            }
            prior = Some(key.clone());
            let class = match class {
                "Observed" => EvidenceClass::Observed,
                "Attested" => EvidenceClass::Attested,
                "Unverified" => EvidenceClass::Unverified,
                "Failed" => EvidenceClass::Failed,
                "NotRun" => EvidenceClass::NotRun,
                _ => return false,
            };
            if !projection_record_is_valid(class, value)
                || !source_keys.insert(
                    value["source_id"]
                        .as_str()
                        .expect("checked source ID")
                        .to_owned(),
                )
                || value
                    .get("lease_id")
                    .and_then(Value::as_str)
                    .is_some_and(|lease_id| !lease_ids.insert(lease_id.to_owned()))
                || value
                    .get("attestation_id")
                    .and_then(Value::as_str)
                    .is_some_and(|attestation_id| {
                        !attestation_ids.insert(attestation_id.to_owned())
                    })
            {
                return false;
            }
            records.push(EvidenceRecord {
                class,
                source_kind: if value["source_kind"] == "run_directory" {
                    "run_directory"
                } else {
                    "handoff_attestation"
                },
                source_id: value["source_id"]
                    .as_str()
                    .expect("checked source ID")
                    .to_owned(),
                value: value.clone(),
            });
        }
    }
    checkpoint["summary"] == summary_json(&records)
}

fn root_entries(root: &Path) -> Result<Vec<(OsString, PathBuf)>, String> {
    let mut entries = fs::read_dir(root)
        .map_err(|_| "cannot read checkpoint input root".to_owned())?
        .map(|entry| {
            entry
                .map(|entry| (entry.file_name(), entry.path()))
                .map_err(|_| "cannot read checkpoint input entry".to_owned())
        })
        .collect::<Result<Vec<_>, _>>()?;
    if entries.len() > MAX_ROOT_ENTRIES {
        return Err("checkpoint input root exceeds its entry limit".to_owned());
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(entries)
}

fn validate_argument_shape(arguments: &[String]) -> Result<(), String> {
    if arguments.first().map(String::as_str) != Some("checkpoint") {
        return Err("checkpoint arguments have the wrong command".to_owned());
    }
    let mut index = 1;
    let mut seen = BTreeSet::new();
    while index < arguments.len() {
        let option = arguments[index].as_str();
        if !matches!(
            option,
            "--runs-root" | "--attestations-root" | "--output-dir"
        ) {
            return Err(format!("unsupported checkpoint option: {option}"));
        }
        if !seen.insert(option) {
            return Err(format!("option {option} may be supplied only once"));
        }
        if arguments.get(index + 1).is_none() {
            return Err(format!("missing value for {option}"));
        }
        index += 2;
    }
    Ok(())
}

fn canonical_input_root(path: &Path, label: &str) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path).map_err(|_| format!("{label} is not readable"))?;
    if !metadata.is_dir() || metadata_is_reparse(&metadata) {
        return Err(format!("{label} must be a non-reparse directory"));
    }
    fs::canonicalize(path).map_err(|_| format!("cannot canonicalize {label}"))
}

fn canonical_output_path(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        let metadata = fs::symlink_metadata(path)
            .map_err(|_| "checkpoint output directory is not readable".to_owned())?;
        if !metadata.is_dir() || metadata_is_reparse(&metadata) {
            return Err("checkpoint output must be a non-reparse directory".to_owned());
        }
        return fs::canonicalize(path)
            .map_err(|_| "cannot canonicalize checkpoint output".to_owned());
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent)
        .map_err(|_| "checkpoint output parent must already exist".to_owned())?;
    let name = path
        .file_name()
        .ok_or_else(|| "checkpoint output must have a final directory name".to_owned())?;
    Ok(parent.join(name))
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

fn metadata_is_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        false
    }
}

fn safe_source_id(name: &OsStr) -> String {
    name.to_str().filter(|name| safe_token(name)).map_or_else(
        || format!("unsafe-{}", &name_snapshot_sha256(name)[..16]),
        ToOwned::to_owned,
    )
}

fn safe_token(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && !matches!(value, "." | "..")
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_lowercase())
}

fn name_snapshot_sha256(name: &OsStr) -> String {
    let mut bytes = b"easycon-checkpoint-source-name-v1\0".to_vec();
    bytes.extend_from_slice(&checkpoint_os_name_bytes(name));
    sha256_bytes(&bytes)
}

#[cfg(windows)]
fn checkpoint_os_name_bytes(name: &OsStr) -> Vec<u8> {
    name.encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>()
}

#[cfg(unix)]
fn checkpoint_os_name_bytes(name: &OsStr) -> Vec<u8> {
    name.as_bytes().to_vec()
}

#[cfg(not(any(windows, unix)))]
fn checkpoint_os_name_bytes(name: &OsStr) -> Vec<u8> {
    name.to_string_lossy().as_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::artifact::{ArtifactReservation, RunStartMetadata};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "easycon-checkpoint-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system time")
                    .as_nanos()
            ));
            fs::create_dir(&path).expect("test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn inputs(runs_root: PathBuf, attestations_root: Option<PathBuf>) -> CheckpointInputs {
        CheckpointInputs {
            runs_root,
            attestations_root,
        }
    }

    fn completed_run(directory: &Path, outcome: DocumentOutcome, trusted: bool) {
        fs::create_dir(directory).expect("run directory");
        let mut reservation = ArtifactReservation::begin_journaled(
            "handshake",
            directory,
            RunStartMetadata {
                normalized_arguments: json!(["handshake"]),
                provenance: json!({"trusted": trusted}),
            },
        )
        .expect("reservation");
        let lease_id = reservation.lease_id().to_owned();
        reservation
            .seal_journal(json!({"primary_artifact": "handshake.json"}))
            .expect("seal journal");
        let (execution, qualification, exit_code) = match outcome {
            DocumentOutcome::Passed => ("completed", "passed", 0),
            DocumentOutcome::QualificationFailed => ("completed", "failed", 1),
            DocumentOutcome::ExecutionFailed => ("failed", "failed", 1),
            DocumentOutcome::NotRun => ("completed", "not_run", 2),
            DocumentOutcome::Unverified => ("completed", "unverified", 2),
            DocumentOutcome::Cancelled => ("cancelled", "unverified", 130),
        };
        reservation
            .commit(&json!({
                "schema_version": 2,
                "command": "handshake",
                "execution_status": execution,
                "qualification_status": qualification,
                "exit_code": exit_code,
                "checks": [],
                "provenance": {"trusted": trusted},
                "run": {"lease_id": lease_id},
                "auxiliary_artifacts": [],
            }))
            .expect("commit run");
    }

    #[test]
    fn input_output_overlap_is_rejected_before_output_creation() {
        let root = TestDirectory::new("overlap");
        let output = root.0.join("checkpoint-output");
        let arguments = vec![
            "checkpoint".to_owned(),
            "--runs-root".to_owned(),
            root.0.to_string_lossy().into_owned(),
            "--output-dir".to_owned(),
            output.to_string_lossy().into_owned(),
        ];

        assert!(CheckpointInputs::parse(&arguments, &output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn initial_entry_limit_is_rejected_before_output_creation() {
        let root = TestDirectory::new("entry-limit");
        let runs = root.0.join("runs");
        let output = root.0.join("checkpoint-output");
        fs::create_dir(&runs).expect("runs root");
        for index in 0..=MAX_ROOT_ENTRIES {
            fs::write(runs.join(format!("entry-{index:04}")), []).expect("synthetic entry");
        }
        let arguments = vec![
            "checkpoint".to_owned(),
            "--runs-root".to_owned(),
            runs.to_string_lossy().into_owned(),
            "--output-dir".to_owned(),
            output.to_string_lossy().into_owned(),
        ];

        assert!(CheckpointInputs::parse(&arguments, &output).is_err());
        assert!(!output.exists());
    }

    #[test]
    fn disk_outcomes_and_attestation_map_to_five_explicit_classes() {
        let root = TestDirectory::new("classes");
        let runs = root.0.join("runs");
        let attestations = root.0.join("attestations");
        fs::create_dir(&runs).expect("runs root");
        fs::create_dir(&attestations).expect("attestations root");
        for (name, outcome) in [
            ("observed", DocumentOutcome::Passed),
            ("unverified", DocumentOutcome::Unverified),
            ("failed", DocumentOutcome::QualificationFailed),
            ("execution-failed", DocumentOutcome::ExecutionFailed),
            ("not-run", DocumentOutcome::NotRun),
            ("cancelled", DocumentOutcome::Cancelled),
        ] {
            completed_run(&runs.join(name), outcome, true);
        }
        let polluted = runs.join("polluted");
        completed_run(&polluted, DocumentOutcome::Passed, true);
        fs::write(polluted.join("unexpected.bin"), b"pollution").expect("polluted run entry");
        let incomplete = runs.join("incomplete");
        fs::create_dir(&incomplete).expect("incomplete directory");
        let _reservation = ArtifactReservation::begin_journaled(
            "handshake",
            &incomplete,
            RunStartMetadata {
                normalized_arguments: json!(["handshake"]),
                provenance: json!({"trusted": true}),
            },
        )
        .expect("incomplete reservation");
        fs::create_dir(runs.join("vacant")).expect("vacant directory");
        fs::write(
            attestations.join("handoff.json"),
            serde_json::to_vec(&json!({
                "schema_version": 1,
                "kind": "easycon_hardware_handoff_attestation",
                "attestation_id": "handoff-com8-20260720",
                "handoff_sha256": "A".repeat(64),
                "claims": ["com8.stable-identity-observed"],
            }))
            .expect("attestation JSON"),
        )
        .expect("attestation file");

        let checkpoint = build_checkpoint(&inputs(runs, Some(attestations)), || Ok(()))
            .unwrap_or_else(|_| panic!("checkpoint build failed"));
        let repeated = build_checkpoint(
            &inputs(root.0.join("runs"), Some(root.0.join("attestations"))),
            || Ok(()),
        )
        .unwrap_or_else(|_| panic!("repeated checkpoint build failed"));

        assert!(projection_is_valid(&checkpoint));
        assert_eq!(checkpoint, repeated);
        assert_eq!(
            checkpoint["evidence"]["Observed"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            checkpoint["evidence"]["Attested"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            checkpoint["evidence"]["Unverified"]
                .as_array()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            checkpoint["evidence"]["Failed"].as_array().map(Vec::len),
            Some(3)
        );
        assert_eq!(
            checkpoint["evidence"]["NotRun"].as_array().map(Vec::len),
            Some(2)
        );
        assert_eq!(checkpoint["status"], "Hardware Unverified");
        assert_eq!(checkpoint["support_matrix_rows_created"], false);

        let mut missing_hash = checkpoint.clone();
        missing_hash["evidence"]["Observed"][0]
            .as_object_mut()
            .expect("observed record")
            .remove("run_snapshot_sha256");
        assert!(!projection_is_valid(&missing_hash));
    }

    #[test]
    fn untrusted_pass_and_malformed_attestation_fail_closed_without_payload_leak() {
        let root = TestDirectory::new("fail-closed");
        let runs = root.0.join("runs");
        let attestations = root.0.join("attestations");
        fs::create_dir(&runs).expect("runs root");
        fs::create_dir(&attestations).expect("attestations root");
        completed_run(&runs.join("untrusted-pass"), DocumentOutcome::Passed, false);
        fs::write(
            attestations.join("malformed.json"),
            br#"{"machine_path":"C:\\secret\\device.log","claim":"passed"}"#,
        )
        .expect("malformed attestation");
        fs::write(
            attestations.join("oversized.json"),
            vec![b'x'; usize::try_from(MAX_ATTESTATION_BYTES).expect("limit") + 1],
        )
        .expect("oversized attestation");

        let checkpoint = build_checkpoint(&inputs(runs, Some(attestations)), || Ok(()))
            .unwrap_or_else(|_| panic!("checkpoint build failed"));
        let serialized = serde_json::to_string(&checkpoint).expect("checkpoint JSON");

        assert_eq!(
            checkpoint["evidence"]["Failed"].as_array().map(Vec::len),
            Some(3)
        );
        assert!(!serialized.contains("secret"));
        assert!(!serialized.contains("machine_path"));
        assert!(!serialized.contains("claim\":\"passed"));
        let malformed = checkpoint["evidence"]["Failed"]
            .as_array()
            .expect("failed records")
            .iter()
            .find(|record| record["source_id"] == "malformed.json")
            .expect("malformed attestation record");
        assert_eq!(malformed["snapshot_complete"], true);
        let oversized = checkpoint["evidence"]["Failed"]
            .as_array()
            .expect("failed records")
            .iter()
            .find(|record| record["source_id"] == "oversized.json")
            .expect("oversized attestation record");
        assert_eq!(oversized["snapshot_complete"], false);
        let untrusted = checkpoint["evidence"]["Failed"]
            .as_array()
            .expect("failed records")
            .iter()
            .find(|record| record["source_id"] == "untrusted-pass")
            .expect("untrusted pass record");
        assert_eq!(untrusted["document_outcome"]["provenance_trusted"], false);
        assert!(
            untrusted["primary"]["sha256"]
                .as_str()
                .is_some_and(valid_sha256)
        );
    }
}
