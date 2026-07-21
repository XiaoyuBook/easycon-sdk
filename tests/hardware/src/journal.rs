use std::fs::File;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{Value, json};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

pub(crate) const JOURNAL_FILE_NAME: &str = "evidence.journal.jsonl";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum JournalEventKind {
    RunStarted,
    CommandDispatchStarted,
    IdentityAdmission,
    ProtocolConnectionTerminal,
    ActionGroupTerminal,
    OperationTerminal,
    CleanupTerminal,
    RunProjectionFinalized,
    ArtifactFinalizationStarted,
    OperatorObservation,
    InterruptRequested,
    CancellationTerminal,
    AmiiboWriteIntent,
    AmiiboChunkIntent,
    AmiiboChunkTerminal,
}

impl JournalEventKind {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::RunStarted => "run_started",
            Self::CommandDispatchStarted => "command_dispatch_started",
            Self::IdentityAdmission => "identity_admission",
            Self::ProtocolConnectionTerminal => "protocol_connection_terminal",
            Self::ActionGroupTerminal => "action_group_terminal",
            Self::OperationTerminal => "operation_terminal",
            Self::CleanupTerminal => "cleanup_terminal",
            Self::RunProjectionFinalized => "run_projection_finalized",
            Self::ArtifactFinalizationStarted => "artifact_finalization_started",
            Self::OperatorObservation => "operator_observation",
            Self::InterruptRequested => "interrupt_requested",
            Self::CancellationTerminal => "cancellation_terminal",
            Self::AmiiboWriteIntent => "amiibo_write_intent",
            Self::AmiiboChunkIntent => "amiibo_chunk_intent",
            Self::AmiiboChunkTerminal => "amiibo_chunk_terminal",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "run_started" => Some(Self::RunStarted),
            "command_dispatch_started" => Some(Self::CommandDispatchStarted),
            "identity_admission" => Some(Self::IdentityAdmission),
            "protocol_connection_terminal" => Some(Self::ProtocolConnectionTerminal),
            "action_group_terminal" => Some(Self::ActionGroupTerminal),
            "operation_terminal" => Some(Self::OperationTerminal),
            "cleanup_terminal" => Some(Self::CleanupTerminal),
            "run_projection_finalized" => Some(Self::RunProjectionFinalized),
            "artifact_finalization_started" => Some(Self::ArtifactFinalizationStarted),
            "operator_observation" => Some(Self::OperatorObservation),
            "interrupt_requested" => Some(Self::InterruptRequested),
            "cancellation_terminal" => Some(Self::CancellationTerminal),
            "amiibo_write_intent" => Some(Self::AmiiboWriteIntent),
            "amiibo_chunk_intent" => Some(Self::AmiiboChunkIntent),
            "amiibo_chunk_terminal" => Some(Self::AmiiboChunkTerminal),
            _ => None,
        }
    }
}

pub(crate) struct JournalStart {
    pub(crate) lease_id: String,
    pub(crate) command: String,
    pub(crate) process_id: u32,
    pub(crate) started_unix_ns: u64,
    pub(crate) normalized_arguments: Value,
    pub(crate) provenance: Value,
}

trait JournalObserver {
    fn after_write(&self, _sequence: u64) -> Result<(), String> {
        Ok(())
    }

    fn after_flush(&self, _sequence: u64) -> Result<(), String> {
        Ok(())
    }

    fn after_sync(&self, _sequence: u64) -> Result<(), String> {
        Ok(())
    }
}

struct NoopJournalObserver;

impl JournalObserver for NoopJournalObserver {}

static NOOP_JOURNAL_OBSERVER: NoopJournalObserver = NoopJournalObserver;

pub(crate) struct EvidenceJournal {
    path: PathBuf,
    file: File,
    expected: Vec<u8>,
    projection: Vec<Value>,
    origin: Instant,
    sealed: bool,
    poisoned: Option<String>,
}

impl EvidenceJournal {
    pub(crate) fn create(path: PathBuf, start: JournalStart) -> Result<Self, String> {
        let file = open_journal(&path)?;
        let mut journal = Self {
            path,
            file,
            expected: Vec::new(),
            projection: Vec::new(),
            origin: Instant::now(),
            sealed: false,
            poisoned: None,
        };
        journal.append(
            JournalEventKind::RunStarted,
            json!({
                "lease_id": start.lease_id,
                "command": start.command,
                "process_id": start.process_id,
                "started_unix_ns": start.started_unix_ns,
                "normalized_arguments": start.normalized_arguments,
                "provenance": start.provenance,
            }),
        )?;
        Ok(journal)
    }

    pub(crate) fn append(&mut self, kind: JournalEventKind, payload: Value) -> Result<(), String> {
        self.append_observed(kind, payload, &NOOP_JOURNAL_OBSERVER)
    }

    fn append_observed(
        &mut self,
        kind: JournalEventKind,
        payload: Value,
        observer: &dyn JournalObserver,
    ) -> Result<(), String> {
        if self.sealed {
            return Err("evidence journal is sealed".to_owned());
        }
        if let Some(error) = &self.poisoned {
            return Err(format!("evidence journal is poisoned: {error}"));
        }
        if !payload.is_object() {
            return Err("journal event payload must be an object".to_owned());
        }
        let sequence = u64::try_from(self.projection.len())
            .ok()
            .and_then(|value| value.checked_add(1))
            .ok_or_else(|| "journal sequence exhausted".to_owned())?;
        let elapsed_ns = u64::try_from(self.origin.elapsed().as_nanos())
            .map_err(|_| "journal monotonic elapsed does not fit u64 nanoseconds".to_owned())?;
        let event = json!({
            "schema_version": 1,
            "sequence": sequence,
            "event": kind.as_str(),
            "elapsed_ns": elapsed_ns,
            "payload": payload,
        });
        let mut line = serde_json::to_vec(&event).map_err(|error| error.to_string())?;
        line.push(b'\n');

        let result: Result<(), String> = (|| {
            self.file
                .write_all(&line)
                .map_err(|error| format!("cannot append journal: {error}"))?;
            observer.after_write(sequence)?;
            self.file
                .flush()
                .map_err(|error| format!("cannot flush journal: {error}"))?;
            observer.after_flush(sequence)?;
            self.file
                .sync_all()
                .map_err(|error| format!("cannot sync journal: {error}"))?;
            observer.after_sync(sequence)?;
            Ok(())
        })();
        if let Err(error) = result {
            self.poisoned = Some(error.clone());
            return Err(error);
        }

        self.expected.extend_from_slice(&line);
        self.projection.push(event);
        Ok(())
    }

    pub(crate) fn seal(&mut self, payload: Value) -> Result<(), String> {
        self.append(JournalEventKind::ArtifactFinalizationStarted, payload)?;
        self.verify()?;
        self.sealed = true;
        Ok(())
    }

    pub(crate) fn verify(&mut self) -> Result<(), String> {
        self.readback().map(|_| ())
    }

    pub(crate) fn readback(&mut self) -> Result<Vec<u8>, String> {
        if let Some(error) = &self.poisoned {
            return Err(format!("evidence journal is poisoned: {error}"));
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot seek journal {}: {error}", self.path.display()))?;
        let mut actual = Vec::new();
        self.file
            .read_to_end(&mut actual)
            .map_err(|error| format!("cannot read journal {}: {error}", self.path.display()))?;
        if actual != self.expected {
            return Err(format!(
                "retained evidence journal {} bytes changed",
                self.path.display()
            ));
        }
        parse_journal(&actual)?;
        self.file
            .seek(SeekFrom::End(0))
            .map_err(|error| format!("cannot seek journal end {}: {error}", self.path.display()))?;
        Ok(actual)
    }

    pub(crate) fn projection_json(&self) -> Value {
        json!({
            "event_count": self.projection.len(),
            "last_sequence": self.projection.last().and_then(|event| event["sequence"].as_u64()),
            "last_event": self.projection.last().and_then(|event| event["event"].as_str()),
            "sealed": self.sealed,
        })
    }

    pub(crate) const fn is_sealed(&self) -> bool {
        self.sealed
    }

    #[cfg(test)]
    pub(crate) fn expected_bytes(&self) -> &[u8] {
        &self.expected
    }
}

pub(crate) fn parse_journal(bytes: &[u8]) -> Result<Vec<Value>, String> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err("journal has an empty or partial final line".to_owned());
    }
    let mut events = Vec::new();
    let mut previous_elapsed = 0_u64;
    for (index, line) in bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .enumerate()
    {
        if line.is_empty() {
            return Err("journal contains an empty line".to_owned());
        }
        let event: Value = serde_json::from_slice(line)
            .map_err(|_| "journal line is not valid JSON".to_owned())?;
        let object = event
            .as_object()
            .ok_or_else(|| "journal line is not an object".to_owned())?;
        let required = [
            "schema_version",
            "sequence",
            "event",
            "elapsed_ns",
            "payload",
        ];
        if object.len() != required.len()
            || required.iter().any(|field| !object.contains_key(*field))
        {
            return Err("journal line schema is not exact".to_owned());
        }
        if event["schema_version"].as_u64() != Some(1) {
            return Err("journal schema version is unsupported".to_owned());
        }
        let expected_sequence =
            u64::try_from(index + 1).map_err(|_| "journal sequence does not fit u64".to_owned())?;
        if event["sequence"].as_u64() != Some(expected_sequence) {
            return Err("journal sequence is not contiguous".to_owned());
        }
        let kind = event["event"]
            .as_str()
            .and_then(JournalEventKind::parse)
            .ok_or_else(|| "journal event kind is unknown".to_owned())?;
        if index == 0 && kind != JournalEventKind::RunStarted {
            return Err("journal does not start with run_started".to_owned());
        }
        if index != 0 && kind == JournalEventKind::RunStarted {
            return Err("journal contains repeated run_started".to_owned());
        }
        let elapsed = event["elapsed_ns"]
            .as_u64()
            .ok_or_else(|| "journal elapsed_ns is invalid".to_owned())?;
        if elapsed < previous_elapsed {
            return Err("journal elapsed_ns moved backwards".to_owned());
        }
        previous_elapsed = elapsed;
        if !event["payload"].is_object() {
            return Err("journal payload is not an object".to_owned());
        }
        events.push(event);
    }
    Ok(events)
}

#[cfg(windows)]
fn open_journal(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ);
    options
        .open(path)
        .map_err(|error| format!("cannot create evidence journal: {error}"))
}

#[cfg(not(windows))]
fn open_journal(_path: &Path) -> Result<File, String> {
    Err("retained evidence journal requires Windows".to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "easycon-hardware-journal-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock")
                    .as_nanos()
            ));
            std::fs::create_dir(&path).expect("test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn start() -> JournalStart {
        JournalStart {
            lease_id: "lease-test".to_owned(),
            command: "handshake".to_owned(),
            process_id: 7,
            started_unix_ns: 11,
            normalized_arguments: json!({"port": "COM8"}),
            provenance: json!({"trusted": true}),
        }
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FailPoint {
        Write,
        Flush,
        Sync,
    }

    struct FailingObserver {
        point: FailPoint,
        calls: Mutex<Vec<FailPoint>>,
    }

    impl JournalObserver for FailingObserver {
        fn after_write(&self, _sequence: u64) -> Result<(), String> {
            self.calls.lock().expect("calls").push(FailPoint::Write);
            (self.point != FailPoint::Write)
                .then_some(())
                .ok_or_else(|| "injected journal write boundary".to_owned())
        }

        fn after_flush(&self, _sequence: u64) -> Result<(), String> {
            self.calls.lock().expect("calls").push(FailPoint::Flush);
            (self.point != FailPoint::Flush)
                .then_some(())
                .ok_or_else(|| "injected journal flush boundary".to_owned())
        }

        fn after_sync(&self, _sequence: u64) -> Result<(), String> {
            self.calls.lock().expect("calls").push(FailPoint::Sync);
            (self.point != FailPoint::Sync)
                .then_some(())
                .ok_or_else(|| "injected journal sync boundary".to_owned())
        }
    }

    #[test]
    fn durable_append_updates_projection_only_after_sync() {
        for point in [FailPoint::Write, FailPoint::Flush, FailPoint::Sync] {
            let directory = TestDirectory::new(&format!("fail-{point:?}"));
            let mut journal = EvidenceJournal::create(directory.0.join(JOURNAL_FILE_NAME), start())
                .expect("initial journal");
            let before = journal.projection_json();
            let observer = FailingObserver {
                point,
                calls: Mutex::new(Vec::new()),
            };

            assert!(
                journal
                    .append_observed(
                        JournalEventKind::CommandDispatchStarted,
                        json!({"command": "handshake"}),
                        &observer,
                    )
                    .is_err()
            );
            assert_eq!(journal.projection_json(), before);
            assert!(
                journal
                    .append(JournalEventKind::CleanupTerminal, json!({}))
                    .is_err()
            );
        }
    }

    #[test]
    fn retained_journal_handle_blocks_delete_and_external_write() {
        let directory = TestDirectory::new("retained-handle");
        let path = directory.0.join(JOURNAL_FILE_NAME);
        let journal = EvidenceJournal::create(path.clone(), start()).expect("journal");

        assert!(std::fs::remove_file(&path).is_err());
        assert!(std::fs::write(&path, b"replacement").is_err());
        drop(journal);
        std::fs::write(&path, b"control replacement").expect("control mutation after close");
    }

    #[test]
    fn parser_rejects_partial_duplicate_unknown_and_backwards_entries() {
        let directory = TestDirectory::new("parser");
        let mut journal =
            EvidenceJournal::create(directory.0.join(JOURNAL_FILE_NAME), start()).expect("journal");
        journal
            .append(
                JournalEventKind::CommandDispatchStarted,
                json!({"command": "handshake"}),
            )
            .expect("second event");
        let valid = journal.expected_bytes().to_vec();
        assert_eq!(parse_journal(&valid).expect("valid").len(), 2);

        assert!(parse_journal(&valid[..valid.len() - 1]).is_err());

        let mut duplicate: Vec<Value> = parse_journal(&valid).expect("events");
        duplicate[1]["sequence"] = json!(1);
        assert!(parse_journal(&render(&duplicate)).is_err());

        let mut unknown: Vec<Value> = parse_journal(&valid).expect("events");
        unknown[1]["event"] = json!("invented");
        assert!(parse_journal(&render(&unknown)).is_err());

        let mut backwards: Vec<Value> = parse_journal(&valid).expect("events");
        backwards[0]["elapsed_ns"] = json!(10);
        backwards[1]["elapsed_ns"] = json!(9);
        assert!(parse_journal(&render(&backwards)).is_err());
    }

    #[test]
    fn sealed_journal_is_exact_and_rejects_further_events() {
        let directory = TestDirectory::new("sealed");
        let mut journal =
            EvidenceJournal::create(directory.0.join(JOURNAL_FILE_NAME), start()).expect("journal");
        journal
            .seal(json!({"primary": "handshake.json"}))
            .expect("seal");
        assert!(journal.is_sealed());
        assert!(journal.verify().is_ok());
        assert!(
            journal
                .append(JournalEventKind::CleanupTerminal, json!({}))
                .is_err()
        );
        assert_eq!(
            journal.projection_json()["last_event"],
            "artifact_finalization_started"
        );
    }

    fn render(events: &[Value]) -> Vec<u8> {
        let mut bytes = Vec::new();
        for event in events {
            bytes.extend(serde_json::to_vec(event).expect("event"));
            bytes.push(b'\n');
        }
        bytes
    }
}
