use std::collections::BTreeSet;
use std::ffi::OsString;
#[cfg(windows)]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(windows)]
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

use crate::journal::{EvidenceJournal, JOURNAL_FILE_NAME, JournalEventKind, JournalStart};
use crate::provenance::sha256_bytes;

pub(crate) const RESERVATION_FILE_NAME: &str = ".easycon-hardware-run.reservation.json";
pub(crate) const SEQUENCE_TIMINGS_FILE_NAME: &str = "sequence-timings.csv";
pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.json";
pub(crate) const COMPLETION_FILE_NAME: &str = "completion.json";

pub(crate) struct RunStartMetadata {
    pub(crate) normalized_arguments: Value,
    pub(crate) provenance: Value,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum AuxiliaryKind {
    SequenceTimingsCsv,
}

impl AuxiliaryKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::SequenceTimingsCsv => "sequence_timings_csv",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum OwnedFileRole {
    Reservation,
    Primary,
    Auxiliary(AuxiliaryKind),
    Manifest,
    Completion,
}

trait ArtifactObserver {
    fn after_file_synced(&self, _role: OwnedFileRole, _path: &Path) {}

    fn after_pre_publish_validation(&self, _directory: &Path) {}

    fn after_hard_link_created(&self, _role: OwnedFileRole, _staging: &Path, _final_path: &Path) {}

    fn after_auxiliary_published(&self, _kind: AuxiliaryKind, _directory: &Path) {}

    fn after_all_published(&self, _directory: &Path) {}

    fn after_manifest_published(&self, _directory: &Path) {}

    fn after_completion_published(&self, _directory: &Path) {}
}

struct NoopArtifactObserver;

impl ArtifactObserver for NoopArtifactObserver {}

static NOOP_ARTIFACT_OBSERVER: NoopArtifactObserver = NoopArtifactObserver;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalGuardOpenSpec {
    read: bool,
    write: bool,
    share_mode: u32,
    custom_flags: u32,
}

trait FinalGuardOpener {
    fn open(&self, path: &Path, spec: FinalGuardOpenSpec) -> Result<File, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct FinalGuardMetadata {
    file_attributes: u32,
    is_file: bool,
}

trait FinalGuardMetadataProvider {
    fn metadata(&self, guard: &File, path: &Path) -> Result<FinalGuardMetadata, String>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct HighResolutionFileIdentity {
    volume_serial_number: u64,
    file_id: u128,
}

trait FileIdentityProvider {
    fn high_resolution_identity(
        &self,
        file: &File,
        path: &Path,
    ) -> Result<HighResolutionFileIdentity, String>;
}

struct SystemFinalGuardOpener;
struct SystemFinalGuardMetadataProvider;
struct SystemFileIdentityProvider;

static SYSTEM_FINAL_GUARD_OPENER: SystemFinalGuardOpener = SystemFinalGuardOpener;
static SYSTEM_FINAL_GUARD_METADATA_PROVIDER: SystemFinalGuardMetadataProvider =
    SystemFinalGuardMetadataProvider;
static SYSTEM_FILE_IDENTITY_PROVIDER: SystemFileIdentityProvider = SystemFileIdentityProvider;

#[derive(Clone, Debug)]
struct ArtifactPlan {
    final_name: String,
    staging_name: String,
}

impl ArtifactPlan {
    fn final_path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.final_name)
    }

    fn staging_path(&self, directory: &Path) -> PathBuf {
        directory.join(&self.staging_name)
    }
}

#[derive(Clone, Debug)]
struct AuxiliaryPlan {
    kind: AuxiliaryKind,
    artifact: ArtifactPlan,
}

struct OwnedFile {
    path: PathBuf,
    file: File,
    expected: Vec<u8>,
}

impl OwnedFile {
    fn create(
        path: PathBuf,
        expected: Vec<u8>,
        role: OwnedFileRole,
        observer: &dyn ArtifactObserver,
    ) -> Result<Self, String> {
        let mut file = open_owned_file(&path)?;
        file.write_all(&expected)
            .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
        file.flush()
            .map_err(|error| format!("cannot flush {}: {error}", path.display()))?;
        file.sync_all()
            .map_err(|error| format!("cannot sync {}: {error}", path.display()))?;
        observer.after_file_synced(role, &path);

        let mut owned = Self {
            path,
            file,
            expected,
        };
        owned.verify()?;
        Ok(owned)
    }

    fn verify(&mut self) -> Result<(), String> {
        self.readback().map(|_| ())
    }

    fn readback(&mut self) -> Result<Vec<u8>, String> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot seek {}: {error}", self.path.display()))?;
        let mut actual = Vec::new();
        self.file
            .read_to_end(&mut actual)
            .map_err(|error| format!("cannot read {}: {error}", self.path.display()))?;
        if actual != self.expected {
            return Err(format!(
                "owned artifact {} no longer contains the expected bytes",
                self.path.display()
            ));
        }
        self.file
            .seek(SeekFrom::End(0))
            .map_err(|error| format!("cannot seek {}: {error}", self.path.display()))?;
        Ok(actual)
    }
}

struct PublishedFile {
    path: PathBuf,
    guard: File,
}

impl PublishedFile {
    fn verify(&mut self, expected: &[u8]) -> Result<(), String> {
        self.readback(expected).map(|_| ())
    }

    fn readback(&mut self, expected: &[u8]) -> Result<Vec<u8>, String> {
        self.guard
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot seek {}: {error}", self.path.display()))?;
        let mut actual = Vec::new();
        self.guard
            .read_to_end(&mut actual)
            .map_err(|error| format!("cannot read {}: {error}", self.path.display()))?;
        if actual != expected {
            return Err(format!(
                "published artifact {} does not contain the expected bytes",
                self.path.display()
            ));
        }
        self.guard
            .seek(SeekFrom::Start(0))
            .map_err(|error| format!("cannot seek {}: {error}", self.path.display()))?;
        Ok(actual)
    }
}

struct OwnedAuxiliary {
    plan: AuxiliaryPlan,
    staging: OwnedFile,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestMember {
    role: String,
    relative_path: String,
    bytes: u64,
    sha256: String,
    same_file_as: Option<String>,
}

impl ManifestMember {
    fn new(
        role: impl Into<String>,
        relative_path: impl Into<String>,
        bytes: &[u8],
        same_file_as: Option<String>,
    ) -> Result<Self, String> {
        Ok(Self {
            role: role.into(),
            relative_path: relative_path.into(),
            bytes: u64::try_from(bytes.len())
                .map_err(|_| "manifest member length does not fit u64".to_owned())?,
            sha256: sha256_bytes(bytes),
            same_file_as,
        })
    }

    fn to_json(&self) -> Value {
        json!({
            "role": self.role,
            "relative_path": self.relative_path,
            "bytes": self.bytes,
            "sha256": self.sha256,
            "same_file_as": self.same_file_as,
        })
    }
}

pub(crate) struct ArtifactReservation {
    directory: PathBuf,
    command: String,
    lease_id: String,
    created_unix_ns: u64,
    tombstone: OwnedFile,
    journal: Option<EvidenceJournal>,
    primary: ArtifactPlan,
    manifest: ArtifactPlan,
    completion: ArtifactPlan,
    planned_auxiliaries: Vec<AuxiliaryPlan>,
    owned_auxiliaries: Vec<OwnedAuxiliary>,
    poisoned: Option<String>,
}

impl ArtifactReservation {
    pub(crate) fn validate_command(command: &str) -> Result<(), String> {
        if command.is_empty()
            || !command
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || value == b'-')
        {
            return Err("command is not safe for an artifact file name".to_owned());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn begin(command: &str, directory: &Path) -> Result<Self, String> {
        Self::begin_observed(command, directory, &NOOP_ARTIFACT_OBSERVER)
    }

    fn begin_observed(
        command: &str,
        directory: &Path,
        observer: &dyn ArtifactObserver,
    ) -> Result<Self, String> {
        Self::validate_command(command)?;
        validate_directory_entries(directory, &BTreeSet::new())?;

        let created_unix_ns = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| format!("system clock is before Unix epoch: {error}"))?
                .as_nanos(),
        )
        .map_err(|_| "current Unix time does not fit u64 nanoseconds".to_owned())?;
        let process_id = std::process::id();
        let lease_id = format!("{process_id:08x}-{created_unix_ns:032x}");
        let primary = ArtifactPlan {
            final_name: format!("{command}.json"),
            staging_name: format!(".{command}.{lease_id}.json.staged"),
        };
        let manifest = ArtifactPlan {
            final_name: MANIFEST_FILE_NAME.to_owned(),
            staging_name: format!(".manifest.{lease_id}.json.staged"),
        };
        let completion = ArtifactPlan {
            final_name: COMPLETION_FILE_NAME.to_owned(),
            staging_name: format!(".completion.{lease_id}.json.staged"),
        };
        let planned_auxiliaries = if command == "sequence" {
            vec![AuxiliaryPlan {
                kind: AuxiliaryKind::SequenceTimingsCsv,
                artifact: ArtifactPlan {
                    final_name: SEQUENCE_TIMINGS_FILE_NAME.to_owned(),
                    staging_name: format!(".sequence.{lease_id}.sequence-timings.csv.staged"),
                },
            }]
        } else {
            Vec::new()
        };
        let auxiliary_document = planned_auxiliaries
            .iter()
            .map(|plan| {
                json!({
                    "kind": plan.kind.as_str(),
                    "final": plan.artifact.final_name,
                    "staging": plan.artifact.staging_name,
                })
            })
            .collect::<Vec<_>>();
        let reservation_document = json!({
            "schema_version": 1,
            "kind": "run_directory_reservation",
            "lease_id": lease_id,
            "command": command,
            "process_id": process_id,
            "created_unix_ns": created_unix_ns,
            "primary_artifact": {
                "final": primary.final_name,
                "staging": primary.staging_name,
            },
            "auxiliary_artifacts": auxiliary_document,
            "evidence_transaction": {
                "journal": JOURNAL_FILE_NAME,
                "manifest": {
                    "final": manifest.final_name,
                    "staging": manifest.staging_name,
                },
                "completion": {
                    "final": completion.final_name,
                    "staging": completion.staging_name,
                },
            },
        });
        let reservation_bytes =
            serde_json::to_vec_pretty(&reservation_document).map_err(|error| error.to_string())?;
        let mut tombstone = OwnedFile::create(
            directory.join(RESERVATION_FILE_NAME),
            reservation_bytes,
            OwnedFileRole::Reservation,
            observer,
        )?;

        let mut allowed = BTreeSet::new();
        allowed.insert(OsString::from(RESERVATION_FILE_NAME));
        validate_directory_entries(directory, &allowed)?;
        tombstone.verify()?;

        Ok(Self {
            directory: directory.to_path_buf(),
            command: command.to_owned(),
            lease_id,
            created_unix_ns,
            tombstone,
            journal: None,
            primary,
            manifest,
            completion,
            planned_auxiliaries,
            owned_auxiliaries: Vec::new(),
            poisoned: None,
        })
    }

    pub(crate) fn begin_journaled(
        command: &str,
        directory: &Path,
        metadata: RunStartMetadata,
    ) -> Result<Self, String> {
        let mut reservation = Self::begin_observed(command, directory, &NOOP_ARTIFACT_OBSERVER)?;
        let journal = EvidenceJournal::create(
            directory.join(JOURNAL_FILE_NAME),
            JournalStart {
                lease_id: reservation.lease_id.clone(),
                command: reservation.command.clone(),
                process_id: std::process::id(),
                started_unix_ns: reservation.created_unix_ns,
                normalized_arguments: metadata.normalized_arguments,
                provenance: metadata.provenance,
            },
        )?;
        reservation.journal = Some(journal);
        validate_directory_entries(directory, &reservation.pre_primary_entries())?;
        reservation.tombstone.verify()?;
        reservation
            .journal
            .as_mut()
            .expect("journal was installed")
            .verify()?;
        Ok(reservation)
    }

    pub(crate) fn record_event(
        &mut self,
        kind: JournalEventKind,
        payload: Value,
    ) -> Result<(), String> {
        self.journal
            .as_mut()
            .ok_or_else(|| "artifact reservation has no evidence journal".to_owned())?
            .append(kind, payload)
    }

    pub(crate) fn seal_journal(&mut self, payload: Value) -> Result<(), String> {
        self.journal
            .as_mut()
            .ok_or_else(|| "artifact reservation has no evidence journal".to_owned())?
            .seal(payload)
    }

    pub(crate) fn journal_projection(&self) -> Result<Value, String> {
        self.journal
            .as_ref()
            .map(EvidenceJournal::projection_json)
            .ok_or_else(|| "artifact reservation has no evidence journal".to_owned())
    }

    pub(crate) fn run_identity_json(&self, ended_unix_ns: u64) -> Value {
        json!({
            "lease_id": self.lease_id,
            "started_unix_ns": self.created_unix_ns,
            "ended_unix_ns": ended_unix_ns,
            "journal": JOURNAL_FILE_NAME,
        })
    }

    pub(crate) fn stage_auxiliary(
        &mut self,
        kind: AuxiliaryKind,
        bytes: Vec<u8>,
    ) -> Result<(), String> {
        self.stage_auxiliary_observed(kind, bytes, &NOOP_ARTIFACT_OBSERVER)
    }

    fn stage_auxiliary_observed(
        &mut self,
        kind: AuxiliaryKind,
        bytes: Vec<u8>,
        observer: &dyn ArtifactObserver,
    ) -> Result<(), String> {
        if let Some(error) = &self.poisoned {
            return Err(format!("artifact reservation is poisoned: {error}"));
        }
        if self
            .owned_auxiliaries
            .iter()
            .any(|owned| owned.plan.kind == kind)
        {
            return Err(format!("auxiliary {} is already staged", kind.as_str()));
        }
        let Some(plan) = self
            .planned_auxiliaries
            .iter()
            .find(|plan| plan.kind == kind)
            .cloned()
        else {
            return Err(format!(
                "command {} does not own auxiliary {}",
                self.command,
                kind.as_str()
            ));
        };
        let staging = match OwnedFile::create(
            plan.artifact.staging_path(&self.directory),
            bytes,
            OwnedFileRole::Auxiliary(kind),
            observer,
        ) {
            Ok(staging) => staging,
            Err(error) => {
                self.poisoned = Some(error.clone());
                return Err(error);
            }
        };
        self.owned_auxiliaries
            .push(OwnedAuxiliary { plan, staging });
        Ok(())
    }

    pub(crate) fn commit(self, document: &Value) -> Result<PathBuf, String> {
        self.commit_with_services(
            document,
            &NOOP_ARTIFACT_OBSERVER,
            &SYSTEM_FINAL_GUARD_OPENER,
            &SYSTEM_FINAL_GUARD_METADATA_PROVIDER,
            &SYSTEM_FILE_IDENTITY_PROVIDER,
        )
    }

    #[cfg(test)]
    fn commit_observed(
        self,
        document: &Value,
        observer: &dyn ArtifactObserver,
    ) -> Result<PathBuf, String> {
        self.commit_with_services(
            document,
            observer,
            &SYSTEM_FINAL_GUARD_OPENER,
            &SYSTEM_FINAL_GUARD_METADATA_PROVIDER,
            &SYSTEM_FILE_IDENTITY_PROVIDER,
        )
    }

    fn commit_with_services(
        mut self,
        document: &Value,
        observer: &dyn ArtifactObserver,
        guard_opener: &dyn FinalGuardOpener,
        guard_metadata_provider: &dyn FinalGuardMetadataProvider,
        identity_provider: &dyn FileIdentityProvider,
    ) -> Result<PathBuf, String> {
        if let Some(error) = &self.poisoned {
            return Err(format!("artifact reservation is poisoned: {error}"));
        }
        if let Some(journal) = &mut self.journal {
            if !journal.is_sealed() {
                return Err("evidence journal must be sealed before artifact commit".to_owned());
            }
            journal.verify()?;
        }
        self.tombstone.verify()?;
        validate_directory_entries(&self.directory, &self.pre_primary_entries())?;

        let primary_bytes =
            serde_json::to_vec_pretty(document).map_err(|error| error.to_string())?;
        let mut primary = OwnedFile::create(
            self.primary.staging_path(&self.directory),
            primary_bytes,
            OwnedFileRole::Primary,
            observer,
        )?;

        self.tombstone.verify()?;
        if let Some(journal) = &mut self.journal {
            journal.verify()?;
        }
        for auxiliary in &mut self.owned_auxiliaries {
            auxiliary.staging.verify()?;
        }
        primary.verify()?;
        validate_directory_entries(&self.directory, &self.pre_publish_entries())?;
        observer.after_pre_publish_validation(&self.directory);

        let mut published_auxiliaries = Vec::with_capacity(self.owned_auxiliaries.len());
        for auxiliary in &self.owned_auxiliaries {
            let final_path = auxiliary.plan.artifact.final_path(&self.directory);
            let published = publish_owned_artifact(
                &auxiliary.staging,
                &final_path,
                OwnedFileRole::Auxiliary(auxiliary.plan.kind),
                observer,
                guard_opener,
                guard_metadata_provider,
                identity_provider,
            )?;
            published_auxiliaries.push(published);
            observer.after_auxiliary_published(auxiliary.plan.kind, &self.directory);
        }
        let primary_final = self.primary.final_path(&self.directory);
        let mut published_primary = publish_owned_artifact(
            &primary,
            &primary_final,
            OwnedFileRole::Primary,
            observer,
            guard_opener,
            guard_metadata_provider,
            identity_provider,
        )?;
        observer.after_all_published(&self.directory);

        validate_directory_entries(&self.directory, &self.published_entries())?;
        self.tombstone.verify()?;
        if let Some(journal) = &mut self.journal {
            journal.verify()?;
        }
        for (auxiliary, published) in self
            .owned_auxiliaries
            .iter_mut()
            .zip(&mut published_auxiliaries)
        {
            auxiliary.staging.verify()?;
            published.verify(&auxiliary.staging.expected)?;
        }
        primary.verify()?;
        published_primary.verify(&primary.expected)?;
        if self.journal.is_none() {
            return Ok(PathBuf::from(&self.primary.final_name));
        }

        let members = collect_manifest_members(
            &mut self.tombstone,
            self.journal.as_mut().expect("journal presence was checked"),
            &self.primary,
            &mut primary,
            &mut published_primary,
            &mut self.owned_auxiliaries,
            &mut published_auxiliaries,
        )?;
        let manifest_document = manifest_document(&self.lease_id, &self.command, &members);
        let manifest_bytes =
            serde_json::to_vec_pretty(&manifest_document).map_err(|error| error.to_string())?;
        let mut manifest = OwnedFile::create(
            self.manifest.staging_path(&self.directory),
            manifest_bytes,
            OwnedFileRole::Manifest,
            observer,
        )?;
        validate_directory_entries(&self.directory, &self.manifest_staged_entries())?;
        let mut published_manifest = publish_owned_artifact(
            &manifest,
            &self.manifest.final_path(&self.directory),
            OwnedFileRole::Manifest,
            observer,
            guard_opener,
            guard_metadata_provider,
            identity_provider,
        )?;
        observer.after_manifest_published(&self.directory);
        validate_directory_entries(&self.directory, &self.manifest_published_entries())?;
        let manifest_disk_bytes = published_manifest.readback(&manifest.expected)?;
        let manifest_disk_document: Value = serde_json::from_slice(&manifest_disk_bytes)
            .map_err(|error| format!("cannot parse published manifest: {error}"))?;
        if manifest_disk_document != manifest_document {
            return Err("published manifest does not match the disk-derived document".to_owned());
        }

        let completion_document = json!({
            "schema_version": 1,
            "kind": "easycon_hardware_evidence_completion",
            "lease_id": self.lease_id,
            "manifest": {
                "relative_path": self.manifest.final_name,
                "bytes": u64::try_from(manifest_disk_bytes.len())
                    .map_err(|_| "manifest length does not fit u64".to_owned())?,
                "sha256": sha256_bytes(&manifest_disk_bytes),
            },
        });
        let completion_bytes =
            serde_json::to_vec_pretty(&completion_document).map_err(|error| error.to_string())?;
        let mut completion = OwnedFile::create(
            self.completion.staging_path(&self.directory),
            completion_bytes,
            OwnedFileRole::Completion,
            observer,
        )?;
        validate_directory_entries(&self.directory, &self.completion_staged_entries())?;
        let mut published_completion = publish_owned_artifact(
            &completion,
            &self.completion.final_path(&self.directory),
            OwnedFileRole::Completion,
            observer,
            guard_opener,
            guard_metadata_provider,
            identity_provider,
        )?;
        observer.after_completion_published(&self.directory);
        validate_directory_entries(&self.directory, &self.completed_entries())?;

        let final_members = collect_manifest_members(
            &mut self.tombstone,
            self.journal.as_mut().expect("journal presence was checked"),
            &self.primary,
            &mut primary,
            &mut published_primary,
            &mut self.owned_auxiliaries,
            &mut published_auxiliaries,
        )?;
        if final_members != members {
            return Err("manifest members changed before completion verification".to_owned());
        }
        manifest.verify()?;
        let manifest_final = published_manifest.readback(&manifest.expected)?;
        if manifest_final != manifest_disk_bytes {
            return Err("manifest bytes changed before completion verification".to_owned());
        }
        completion.verify()?;
        let completion_final = published_completion.readback(&completion.expected)?;
        let completion_final_document: Value = serde_json::from_slice(&completion_final)
            .map_err(|error| format!("cannot parse published completion: {error}"))?;
        if completion_final_document != completion_document {
            return Err("published completion does not match its owned document".to_owned());
        }
        let inspection = classify_run_directory(&self.directory);
        if inspection.status != RunDirectoryStatus::Completed {
            return Err(format!(
                "completed evidence transaction failed disk classification: {}",
                inspection.reason
            ));
        }
        Ok(PathBuf::from(&self.primary.final_name))
    }

    fn pre_primary_entries(&self) -> BTreeSet<OsString> {
        let mut entries = BTreeSet::new();
        entries.insert(OsString::from(RESERVATION_FILE_NAME));
        if self.journal.is_some() {
            entries.insert(OsString::from(JOURNAL_FILE_NAME));
        }
        for auxiliary in &self.owned_auxiliaries {
            entries.insert(OsString::from(&auxiliary.plan.artifact.staging_name));
        }
        entries
    }

    fn pre_publish_entries(&self) -> BTreeSet<OsString> {
        let mut entries = self.pre_primary_entries();
        entries.insert(OsString::from(&self.primary.staging_name));
        entries
    }

    fn published_entries(&self) -> BTreeSet<OsString> {
        let mut entries = self.pre_publish_entries();
        entries.insert(OsString::from(&self.primary.final_name));
        for auxiliary in &self.owned_auxiliaries {
            entries.insert(OsString::from(&auxiliary.plan.artifact.final_name));
        }
        entries
    }

    fn manifest_staged_entries(&self) -> BTreeSet<OsString> {
        let mut entries = self.published_entries();
        entries.insert(OsString::from(&self.manifest.staging_name));
        entries
    }

    fn manifest_published_entries(&self) -> BTreeSet<OsString> {
        let mut entries = self.manifest_staged_entries();
        entries.insert(OsString::from(&self.manifest.final_name));
        entries
    }

    fn completion_staged_entries(&self) -> BTreeSet<OsString> {
        let mut entries = self.manifest_published_entries();
        entries.insert(OsString::from(&self.completion.staging_name));
        entries
    }

    fn completed_entries(&self) -> BTreeSet<OsString> {
        let mut entries = self.completion_staged_entries();
        entries.insert(OsString::from(&self.completion.final_name));
        entries
    }
}

fn collect_manifest_members(
    tombstone: &mut OwnedFile,
    journal: &mut EvidenceJournal,
    primary_plan: &ArtifactPlan,
    primary: &mut OwnedFile,
    published_primary: &mut PublishedFile,
    auxiliaries: &mut [OwnedAuxiliary],
    published_auxiliaries: &mut [PublishedFile],
) -> Result<Vec<ManifestMember>, String> {
    if auxiliaries.len() != published_auxiliaries.len() {
        return Err("published auxiliary count does not match owned auxiliaries".to_owned());
    }
    let mut members = vec![
        ManifestMember::new(
            "reservation",
            RESERVATION_FILE_NAME,
            &tombstone.readback()?,
            None,
        )?,
        ManifestMember::new("journal", JOURNAL_FILE_NAME, &journal.readback()?, None)?,
        ManifestMember::new(
            "primary_staging",
            &primary_plan.staging_name,
            &primary.readback()?,
            None,
        )?,
        ManifestMember::new(
            "primary_final",
            &primary_plan.final_name,
            &published_primary.readback(&primary.expected)?,
            Some(primary_plan.staging_name.clone()),
        )?,
    ];
    for (auxiliary, published) in auxiliaries.iter_mut().zip(published_auxiliaries) {
        let kind = auxiliary.plan.kind.as_str();
        members.push(ManifestMember::new(
            format!("{kind}_staging"),
            &auxiliary.plan.artifact.staging_name,
            &auxiliary.staging.readback()?,
            None,
        )?);
        members.push(ManifestMember::new(
            format!("{kind}_final"),
            &auxiliary.plan.artifact.final_name,
            &published.readback(&auxiliary.staging.expected)?,
            Some(auxiliary.plan.artifact.staging_name.clone()),
        )?);
    }
    members.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(members)
}

fn manifest_document(lease_id: &str, command: &str, members: &[ManifestMember]) -> Value {
    json!({
        "schema_version": 1,
        "kind": "easycon_hardware_evidence_manifest",
        "lease_id": lease_id,
        "command": command,
        "members": members.iter().map(ManifestMember::to_json).collect::<Vec<_>>(),
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RunDirectoryStatus {
    Vacant,
    Incomplete,
    Polluted,
    Completed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RunDirectoryInspection {
    pub(crate) status: RunDirectoryStatus,
    pub(crate) reason: &'static str,
}

#[derive(Clone, Debug)]
struct DiskTransactionPlan {
    lease_id: String,
    command: String,
    primary: ArtifactPlan,
    auxiliaries: Vec<AuxiliaryPlan>,
    manifest: ArtifactPlan,
    completion: ArtifactPlan,
}

impl DiskTransactionPlan {
    fn from_reservation(document: &Value) -> Result<Self, String> {
        let object = document
            .as_object()
            .ok_or_else(|| "reservation is not an object".to_owned())?;
        let required = [
            "schema_version",
            "kind",
            "lease_id",
            "command",
            "process_id",
            "created_unix_ns",
            "primary_artifact",
            "auxiliary_artifacts",
            "evidence_transaction",
        ];
        if object.len() != required.len()
            || required.iter().any(|field| !object.contains_key(*field))
        {
            return Err("reservation schema is not exact".to_owned());
        }
        if document["schema_version"].as_u64() != Some(1)
            || document["kind"] != "run_directory_reservation"
        {
            return Err("reservation identity is invalid".to_owned());
        }
        let lease_id = nonempty_string(&document["lease_id"], "lease_id")?;
        let command = nonempty_string(&document["command"], "command")?;
        ArtifactReservation::validate_command(&command)?;
        if document["process_id"].as_u64().is_none()
            || document["created_unix_ns"].as_u64().is_none()
        {
            return Err("reservation process/time fields are invalid".to_owned());
        }
        let primary = parse_artifact_plan(&document["primary_artifact"])?;
        let auxiliary_values = document["auxiliary_artifacts"]
            .as_array()
            .ok_or_else(|| "reservation auxiliary plan is invalid".to_owned())?;
        let mut auxiliaries = Vec::with_capacity(auxiliary_values.len());
        for value in auxiliary_values {
            let object = value
                .as_object()
                .ok_or_else(|| "auxiliary plan is not an object".to_owned())?;
            if object.len() != 3
                || !object.contains_key("kind")
                || !object.contains_key("final")
                || !object.contains_key("staging")
            {
                return Err("auxiliary plan schema is not exact".to_owned());
            }
            let kind = match value["kind"].as_str() {
                Some("sequence_timings_csv") => AuxiliaryKind::SequenceTimingsCsv,
                _ => return Err("auxiliary plan kind is unknown".to_owned()),
            };
            auxiliaries.push(AuxiliaryPlan {
                kind,
                artifact: ArtifactPlan {
                    final_name: safe_relative_name(&value["final"], "auxiliary final")?,
                    staging_name: safe_relative_name(&value["staging"], "auxiliary staging")?,
                },
            });
        }
        let transaction = document["evidence_transaction"]
            .as_object()
            .ok_or_else(|| "evidence transaction plan is invalid".to_owned())?;
        if transaction.len() != 3
            || transaction.get("journal").and_then(Value::as_str) != Some(JOURNAL_FILE_NAME)
        {
            return Err("evidence transaction plan schema is invalid".to_owned());
        }
        let manifest = parse_artifact_plan(&document["evidence_transaction"]["manifest"])?;
        let completion = parse_artifact_plan(&document["evidence_transaction"]["completion"])?;
        if manifest.final_name != MANIFEST_FILE_NAME
            || completion.final_name != COMPLETION_FILE_NAME
        {
            return Err("evidence transaction final names are invalid".to_owned());
        }
        let plan = Self {
            lease_id,
            command,
            primary,
            auxiliaries,
            manifest,
            completion,
        };
        plan.validate_expected_names()?;
        plan.completed_entries()?;
        Ok(plan)
    }

    fn validate_expected_names(&self) -> Result<(), String> {
        let mut lease_parts = self.lease_id.split('-');
        let process = lease_parts.next().unwrap_or_default();
        let timestamp = lease_parts.next().unwrap_or_default();
        if lease_parts.next().is_some()
            || process.len() != 8
            || timestamp.len() != 32
            || !process.bytes().all(|byte| byte.is_ascii_hexdigit())
            || !timestamp.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("reservation lease_id is invalid".to_owned());
        }
        if self.primary.final_name != format!("{}.json", self.command)
            || self.primary.staging_name
                != format!(".{}.{}.json.staged", self.command, self.lease_id)
            || self.manifest.final_name != MANIFEST_FILE_NAME
            || self.manifest.staging_name != format!(".manifest.{}.json.staged", self.lease_id)
            || self.completion.final_name != COMPLETION_FILE_NAME
            || self.completion.staging_name != format!(".completion.{}.json.staged", self.lease_id)
        {
            return Err("reservation transaction names do not match lease".to_owned());
        }
        match (self.command.as_str(), self.auxiliaries.as_slice()) {
            ("sequence", [auxiliary])
                if auxiliary.kind == AuxiliaryKind::SequenceTimingsCsv
                    && auxiliary.artifact.final_name == SEQUENCE_TIMINGS_FILE_NAME
                    && auxiliary.artifact.staging_name
                        == format!(".sequence.{}.sequence-timings.csv.staged", self.lease_id) =>
            {
                Ok(())
            }
            ("sequence", _) => Err("sequence auxiliary plan is invalid".to_owned()),
            (_, []) => Ok(()),
            (_, _) => Err("non-sequence command declared an auxiliary".to_owned()),
        }
    }

    fn completed_entries(&self) -> Result<BTreeSet<OsString>, String> {
        let mut entries = BTreeSet::from([
            OsString::from(RESERVATION_FILE_NAME),
            OsString::from(JOURNAL_FILE_NAME),
            OsString::from(&self.primary.staging_name),
            OsString::from(&self.primary.final_name),
            OsString::from(&self.manifest.staging_name),
            OsString::from(&self.manifest.final_name),
            OsString::from(&self.completion.staging_name),
            OsString::from(&self.completion.final_name),
        ]);
        let expected = 8 + self.auxiliaries.len() * 2;
        for auxiliary in &self.auxiliaries {
            entries.insert(OsString::from(&auxiliary.artifact.staging_name));
            entries.insert(OsString::from(&auxiliary.artifact.final_name));
        }
        if entries.len() != expected {
            return Err("transaction plan contains duplicate relative names".to_owned());
        }
        Ok(entries)
    }

    fn manifest_specs(&self) -> Vec<(String, String, Option<String>)> {
        let mut specs = vec![
            (
                "reservation".to_owned(),
                RESERVATION_FILE_NAME.to_owned(),
                None,
            ),
            ("journal".to_owned(), JOURNAL_FILE_NAME.to_owned(), None),
            (
                "primary_staging".to_owned(),
                self.primary.staging_name.clone(),
                None,
            ),
            (
                "primary_final".to_owned(),
                self.primary.final_name.clone(),
                Some(self.primary.staging_name.clone()),
            ),
        ];
        for auxiliary in &self.auxiliaries {
            let kind = auxiliary.kind.as_str();
            specs.push((
                format!("{kind}_staging"),
                auxiliary.artifact.staging_name.clone(),
                None,
            ));
            specs.push((
                format!("{kind}_final"),
                auxiliary.artifact.final_name.clone(),
                Some(auxiliary.artifact.staging_name.clone()),
            ));
        }
        specs.sort_by(|left, right| left.1.cmp(&right.1));
        specs
    }
}

struct GuardedDiskFile {
    bytes: Vec<u8>,
    identity: HighResolutionFileIdentity,
}

pub(crate) fn classify_run_directory(directory: &Path) -> RunDirectoryInspection {
    match classify_run_directory_inner(directory) {
        Ok(status) => status,
        Err(_) => RunDirectoryInspection {
            status: RunDirectoryStatus::Polluted,
            reason: "completed_evidence_invalid",
        },
    }
}

fn classify_run_directory_inner(directory: &Path) -> Result<RunDirectoryInspection, String> {
    let actual = directory_entries(directory)?;
    if actual.is_empty() {
        return Ok(RunDirectoryInspection {
            status: RunDirectoryStatus::Vacant,
            reason: "directory_empty",
        });
    }
    if !actual.contains(&OsString::from(RESERVATION_FILE_NAME)) {
        return Ok(RunDirectoryInspection {
            status: RunDirectoryStatus::Polluted,
            reason: "reservation_missing",
        });
    }
    let reservation = read_guarded_disk_file(&directory.join(RESERVATION_FILE_NAME))?;
    let reservation_document: Value = serde_json::from_slice(&reservation.bytes)
        .map_err(|_| "reservation JSON is invalid".to_owned())?;
    let plan = DiskTransactionPlan::from_reservation(&reservation_document)?;
    let expected = plan.completed_entries()?;
    if actual.difference(&expected).next().is_some() {
        return Ok(RunDirectoryInspection {
            status: RunDirectoryStatus::Polluted,
            reason: "unexpected_entry",
        });
    }
    for artifact in std::iter::once(&plan.primary)
        .chain(plan.auxiliaries.iter().map(|plan| &plan.artifact))
        .chain([&plan.manifest, &plan.completion])
    {
        if actual.contains(&OsString::from(&artifact.final_name))
            && !actual.contains(&OsString::from(&artifact.staging_name))
        {
            return Ok(RunDirectoryInspection {
                status: RunDirectoryStatus::Polluted,
                reason: "orphan_final_link",
            });
        }
    }
    if !actual.contains(&OsString::from(&plan.completion.final_name)) {
        return Ok(RunDirectoryInspection {
            status: RunDirectoryStatus::Incomplete,
            reason: "completion_absent",
        });
    }
    if actual != expected {
        return Ok(RunDirectoryInspection {
            status: RunDirectoryStatus::Polluted,
            reason: "completed_entry_missing",
        });
    }

    let journal = read_guarded_disk_file(&directory.join(JOURNAL_FILE_NAME))?;
    let journal_events = crate::journal::parse_journal(&journal.bytes)?;
    if journal_events
        .last()
        .and_then(|event| event["event"].as_str())
        != Some(JournalEventKind::ArtifactFinalizationStarted.as_str())
    {
        return Err("completed journal is not sealed".to_owned());
    }

    let members = disk_manifest_members(directory, &plan)?;
    let expected_manifest = manifest_document(&plan.lease_id, &plan.command, &members);
    let manifest_file = read_guarded_disk_file(&directory.join(&plan.manifest.final_name))?;
    let manifest_staging = read_guarded_disk_file(&directory.join(&plan.manifest.staging_name))?;
    if manifest_staging.identity != manifest_file.identity
        || manifest_staging.bytes != manifest_file.bytes
    {
        return Err("manifest staging/final identity does not match".to_owned());
    }
    let manifest_document: Value = serde_json::from_slice(&manifest_file.bytes)
        .map_err(|_| "manifest JSON is invalid".to_owned())?;
    if manifest_document != expected_manifest {
        return Err("manifest does not match disk members".to_owned());
    }

    let completion_file = read_guarded_disk_file(&directory.join(&plan.completion.final_name))?;
    let completion_staging =
        read_guarded_disk_file(&directory.join(&plan.completion.staging_name))?;
    if completion_staging.identity != completion_file.identity
        || completion_staging.bytes != completion_file.bytes
    {
        return Err("completion staging/final identity does not match".to_owned());
    }
    let completion: Value = serde_json::from_slice(&completion_file.bytes)
        .map_err(|_| "completion JSON is invalid".to_owned())?;
    let expected_completion = json!({
        "schema_version": 1,
        "kind": "easycon_hardware_evidence_completion",
        "lease_id": plan.lease_id,
        "manifest": {
            "relative_path": plan.manifest.final_name,
            "bytes": u64::try_from(manifest_file.bytes.len())
                .map_err(|_| "manifest length does not fit u64".to_owned())?,
            "sha256": sha256_bytes(&manifest_file.bytes),
        },
    });
    if completion != expected_completion {
        return Err("completion does not match the manifest".to_owned());
    }

    Ok(RunDirectoryInspection {
        status: RunDirectoryStatus::Completed,
        reason: "manifest_verified",
    })
}

fn disk_manifest_members(
    directory: &Path,
    plan: &DiskTransactionPlan,
) -> Result<Vec<ManifestMember>, String> {
    let mut files = std::collections::BTreeMap::new();
    let mut members = Vec::new();
    for (role, relative_path, same_file_as) in plan.manifest_specs() {
        let file = read_guarded_disk_file(&directory.join(&relative_path))?;
        if let Some(source) = &same_file_as {
            let source_file = if let Some(source_file) = files.get(source) {
                source_file
            } else {
                let source_file = read_guarded_disk_file(&directory.join(source))?;
                files.insert(source.clone(), source_file);
                files.get(source).expect("source was inserted")
            };
            if source_file.identity != file.identity {
                return Err("manifest same_file_as identity does not match".to_owned());
            }
        }
        members.push(ManifestMember::new(
            role,
            relative_path.clone(),
            &file.bytes,
            same_file_as,
        )?);
        files.insert(relative_path, file);
    }
    members.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(members)
}

fn read_guarded_disk_file(path: &Path) -> Result<GuardedDiskFile, String> {
    let mut file = SYSTEM_FINAL_GUARD_OPENER.open(path, final_guard_open_spec())?;
    let metadata = SYSTEM_FINAL_GUARD_METADATA_PROVIDER.metadata(&file, path)?;
    validate_final_guard_metadata(metadata, path)?;
    let identity = SYSTEM_FILE_IDENTITY_PROVIDER.high_resolution_identity(&file, path)?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read guarded evidence file: {error}"))?;
    Ok(GuardedDiskFile { bytes, identity })
}

fn directory_entries(directory: &Path) -> Result<BTreeSet<OsString>, String> {
    fs::read_dir(directory)
        .map_err(|error| format!("cannot inspect run directory: {error}"))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|error| format!("cannot inspect run directory entry: {error}"))
        })
        .collect()
}

fn parse_artifact_plan(value: &Value) -> Result<ArtifactPlan, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "artifact plan is not an object".to_owned())?;
    if object.len() != 2 || !object.contains_key("final") || !object.contains_key("staging") {
        return Err("artifact plan schema is not exact".to_owned());
    }
    Ok(ArtifactPlan {
        final_name: safe_relative_name(&value["final"], "artifact final")?,
        staging_name: safe_relative_name(&value["staging"], "artifact staging")?,
    })
}

fn safe_relative_name(value: &Value, field: &str) -> Result<String, String> {
    let value = nonempty_string(value, field)?;
    let path = Path::new(&value);
    if path.components().count() != 1 || path.file_name().is_none() || value == "." || value == ".."
    {
        return Err(format!("{field} is not a single relative name"));
    }
    Ok(value)
}

fn nonempty_string(value: &Value, field: &str) -> Result<String, String> {
    value
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("{field} is empty or invalid"))
}

#[cfg(windows)]
fn open_owned_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ);
    options
        .open(path)
        .map_err(|error| format!("cannot create owned artifact {}: {error}", path.display()))
}

#[cfg(not(windows))]
fn open_owned_file(path: &Path) -> Result<File, String> {
    Err(format!(
        "cannot create owned artifact {}: retained artifact ownership requires Windows",
        path.display()
    ))
}

#[cfg(windows)]
const fn final_guard_open_spec() -> FinalGuardOpenSpec {
    FinalGuardOpenSpec {
        read: true,
        write: false,
        share_mode: FILE_SHARE_READ | FILE_SHARE_WRITE,
        custom_flags: FILE_FLAG_OPEN_REPARSE_POINT,
    }
}

#[cfg(not(windows))]
const fn final_guard_open_spec() -> FinalGuardOpenSpec {
    FinalGuardOpenSpec {
        read: true,
        write: false,
        share_mode: 0,
        custom_flags: 0,
    }
}

impl FinalGuardOpener for SystemFinalGuardOpener {
    fn open(&self, path: &Path, spec: FinalGuardOpenSpec) -> Result<File, String> {
        #[cfg(windows)]
        {
            let mut options = OpenOptions::new();
            options
                .read(spec.read)
                .write(spec.write)
                .share_mode(spec.share_mode)
                .custom_flags(spec.custom_flags);
            options
                .open(path)
                .map_err(|error| format!("cannot retain final guard {}: {error}", path.display()))
        }
        #[cfg(not(windows))]
        {
            let _ = spec;
            Err(format!(
                "cannot retain final guard {}: guarded publication requires Windows",
                path.display()
            ))
        }
    }
}

impl FinalGuardMetadataProvider for SystemFinalGuardMetadataProvider {
    fn metadata(&self, guard: &File, path: &Path) -> Result<FinalGuardMetadata, String> {
        #[cfg(windows)]
        {
            let metadata = guard.metadata().map_err(|error| {
                format!("cannot inspect final guard {}: {error}", path.display())
            })?;
            Ok(FinalGuardMetadata {
                file_attributes: metadata.file_attributes(),
                is_file: metadata.is_file(),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = guard;
            Err(format!(
                "cannot inspect final guard {}: guarded publication requires Windows",
                path.display()
            ))
        }
    }
}

fn validate_final_guard_metadata(metadata: FinalGuardMetadata, path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    if metadata.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(format!(
            "final artifact {} is a reparse point",
            path.display()
        ));
    }
    if !metadata.is_file {
        return Err(format!(
            "final artifact {} is not a regular file",
            path.display()
        ));
    }
    Ok(())
}

impl FileIdentityProvider for SystemFileIdentityProvider {
    fn high_resolution_identity(
        &self,
        file: &File,
        path: &Path,
    ) -> Result<HighResolutionFileIdentity, String> {
        #[cfg(windows)]
        {
            let identity =
                easycon_hardware_file_id::high_resolution_file_identity(file).map_err(|error| {
                    format!(
                        "cannot read high-resolution file identity for {}: {error}",
                        path.display()
                    )
                })?;
            Ok(HighResolutionFileIdentity {
                volume_serial_number: identity.volume_serial_number,
                file_id: identity.file_id,
            })
        }
        #[cfg(not(windows))]
        {
            let _ = file;
            Err(format!(
                "cannot read high-resolution file identity for {}: guarded publication requires Windows",
                path.display()
            ))
        }
    }
}

fn publish_owned_artifact(
    staging: &OwnedFile,
    final_path: &Path,
    role: OwnedFileRole,
    observer: &dyn ArtifactObserver,
    guard_opener: &dyn FinalGuardOpener,
    guard_metadata_provider: &dyn FinalGuardMetadataProvider,
    identity_provider: &dyn FileIdentityProvider,
) -> Result<PublishedFile, String> {
    fs::hard_link(&staging.path, final_path).map_err(|error| {
        format!(
            "cannot publish {} as {} without replacement: {error}",
            staging.path.display(),
            final_path.display()
        )
    })?;
    observer.after_hard_link_created(role, &staging.path, final_path);

    let guard = guard_opener.open(final_path, final_guard_open_spec())?;
    let guard_metadata = guard_metadata_provider.metadata(&guard, final_path)?;
    validate_final_guard_metadata(guard_metadata, final_path)?;
    let staging_identity =
        identity_provider.high_resolution_identity(&staging.file, &staging.path)?;
    let final_identity = identity_provider.high_resolution_identity(&guard, final_path)?;
    if staging_identity != final_identity {
        return Err(format!(
            "published artifact {} does not reference its owned staging object",
            final_path.display()
        ));
    }

    let mut published = PublishedFile {
        path: final_path.to_path_buf(),
        guard,
    };
    published.verify(&staging.expected)?;
    Ok(published)
}

fn validate_directory_entries(
    directory: &Path,
    expected: &BTreeSet<OsString>,
) -> Result<(), String> {
    let actual = fs::read_dir(directory)
        .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name())
                .map_err(|error| format!("cannot inspect {}: {error}", directory.display()))
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    if actual == *expected {
        return Ok(());
    }

    let missing = expected
        .difference(&actual)
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let unexpected = actual
        .difference(expected)
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    Err(format!(
        "run directory entries do not match the current reservation; missing={missing:?}; unexpected={unexpected:?}"
    ))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::{Arc, Barrier};

    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "easycon-hardware-artifact-{label}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after epoch")
                    .as_nanos()
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

    fn journal_metadata() -> RunStartMetadata {
        RunStartMetadata {
            normalized_arguments: json!(["handshake", "--port", "COM8"]),
            provenance: json!({"trusted": true}),
        }
    }

    #[test]
    fn journaled_reservation_requires_a_sealed_durable_prefix() {
        let incomplete_directory = TestDirectory::new("journal-unsealed");
        let reservation = ArtifactReservation::begin_journaled(
            "handshake",
            &incomplete_directory.0,
            journal_metadata(),
        )
        .expect("journaled reservation");
        assert!(reservation.commit(&json!({"status": "failed"})).is_err());
        assert!(incomplete_directory.0.join(JOURNAL_FILE_NAME).is_file());
        assert!(!incomplete_directory.0.join("handshake.json").exists());
        assert_eq!(
            classify_run_directory(&incomplete_directory.0),
            RunDirectoryInspection {
                status: RunDirectoryStatus::Incomplete,
                reason: "completion_absent",
            }
        );

        let directory = TestDirectory::new("journal-sealed");
        let mut reservation =
            ArtifactReservation::begin_journaled("handshake", &directory.0, journal_metadata())
                .expect("journaled reservation");
        reservation
            .record_event(
                JournalEventKind::CommandDispatchStarted,
                json!({"command": "handshake"}),
            )
            .expect("dispatch event");
        reservation
            .record_event(
                JournalEventKind::RunProjectionFinalized,
                json!({"status": "failed"}),
            )
            .expect("projection event");
        reservation
            .seal_journal(json!({"primary_artifact": "handshake.json"}))
            .expect("seal journal");
        let projection = reservation.journal_projection().expect("projection");

        reservation
            .commit(&json!({"status": "failed", "journal_projection": projection}))
            .expect("publish journaled result");

        let journal = fs::read(directory.0.join(JOURNAL_FILE_NAME)).expect("journal bytes");
        let events = crate::journal::parse_journal(&journal).expect("durable journal");
        assert_eq!(events.len(), 4);
        assert_eq!(events[0]["event"], "run_started");
        assert_eq!(events[3]["event"], "artifact_finalization_started");
        assert_eq!(
            classify_run_directory(&directory.0),
            RunDirectoryInspection {
                status: RunDirectoryStatus::Completed,
                reason: "manifest_verified",
            }
        );

        let manifest_bytes = fs::read(directory.0.join(MANIFEST_FILE_NAME)).expect("manifest");
        let manifest: Value = serde_json::from_slice(&manifest_bytes).expect("manifest JSON");
        let members = manifest["members"].as_array().expect("manifest members");
        assert!(members.iter().all(|member| {
            !matches!(
                member["relative_path"].as_str(),
                Some(MANIFEST_FILE_NAME | COMPLETION_FILE_NAME)
            )
        }));
        for member in members {
            let relative = member["relative_path"].as_str().expect("relative path");
            let bytes = fs::read(directory.0.join(relative)).expect("manifest member");
            assert_eq!(member["bytes"].as_u64(), Some(bytes.len() as u64));
            assert_eq!(member["sha256"], sha256_bytes(&bytes));
        }
        let completion: Value = serde_json::from_slice(
            &fs::read(directory.0.join(COMPLETION_FILE_NAME)).expect("completion"),
        )
        .expect("completion JSON");
        assert_eq!(
            completion["manifest"]["sha256"],
            sha256_bytes(&manifest_bytes)
        );

        fs::write(
            directory.0.join("handshake.json"),
            b"mutated after completion",
        )
        .expect("post-owner mutation");
        assert_eq!(
            classify_run_directory(&directory.0).status,
            RunDirectoryStatus::Polluted
        );
    }

    #[test]
    fn classifier_distinguishes_vacant_and_polluted_incomplete_directories() {
        let vacant = TestDirectory::new("classification-vacant");
        assert_eq!(
            classify_run_directory(&vacant.0).status,
            RunDirectoryStatus::Vacant
        );

        let polluted = TestDirectory::new("classification-polluted");
        let _reservation =
            ArtifactReservation::begin_journaled("handshake", &polluted.0, journal_metadata())
                .expect("journaled reservation");
        fs::write(polluted.0.join("intruder.bin"), b"not owned").expect("pollution");
        assert_eq!(
            classify_run_directory(&polluted.0),
            RunDirectoryInspection {
                status: RunDirectoryStatus::Polluted,
                reason: "unexpected_entry",
            }
        );
    }

    #[test]
    fn sequence_manifest_hashes_both_csv_links_from_disk() {
        let directory = TestDirectory::new("manifest-sequence");
        let csv = b"sequence,dispatch_ns\n1,10\n".to_vec();
        let mut reservation =
            ArtifactReservation::begin_journaled("sequence", &directory.0, journal_metadata())
                .expect("journaled sequence");
        reservation
            .stage_auxiliary(AuxiliaryKind::SequenceTimingsCsv, csv.clone())
            .expect("stage CSV");
        reservation
            .record_event(
                JournalEventKind::RunProjectionFinalized,
                json!({"status": "unverified"}),
            )
            .expect("projection");
        reservation
            .seal_journal(json!({"primary_artifact": "sequence.json"}))
            .expect("seal");
        reservation
            .commit(&json!({
                "status": "unverified",
                "auxiliary_artifacts": [{
                    "kind": "sequence_timings_csv",
                    "relative_path": SEQUENCE_TIMINGS_FILE_NAME,
                    "bytes": csv.len(),
                    "sha256": sha256_bytes(&csv),
                }],
            }))
            .expect("commit sequence transaction");

        let manifest: Value = serde_json::from_slice(
            &fs::read(directory.0.join(MANIFEST_FILE_NAME)).expect("manifest"),
        )
        .expect("manifest JSON");
        let csv_members = manifest["members"]
            .as_array()
            .expect("members")
            .iter()
            .filter(|member| {
                member["role"] == "sequence_timings_csv_staging"
                    || member["role"] == "sequence_timings_csv_final"
            })
            .collect::<Vec<_>>();
        assert_eq!(csv_members.len(), 2);
        assert!(
            csv_members
                .iter()
                .all(|member| member["sha256"] == sha256_bytes(&csv))
        );
        assert_eq!(
            classify_run_directory(&directory.0).status,
            RunDirectoryStatus::Completed
        );
    }

    #[derive(Clone, Copy)]
    enum TransactionCollision {
        Manifest,
        Completion,
    }

    struct TransactionCollisionObserver {
        point: TransactionCollision,
        bytes: Vec<u8>,
    }

    impl ArtifactObserver for TransactionCollisionObserver {
        fn after_all_published(&self, directory: &Path) {
            if matches!(self.point, TransactionCollision::Manifest) {
                fs::write(directory.join(MANIFEST_FILE_NAME), &self.bytes)
                    .expect("inject manifest collision");
            }
        }

        fn after_manifest_published(&self, directory: &Path) {
            if matches!(self.point, TransactionCollision::Completion) {
                fs::write(directory.join(COMPLETION_FILE_NAME), &self.bytes)
                    .expect("inject completion collision");
            }
        }
    }

    #[test]
    fn manifest_and_completion_collisions_preserve_injected_bytes() {
        for point in [
            TransactionCollision::Manifest,
            TransactionCollision::Completion,
        ] {
            let label = match point {
                TransactionCollision::Manifest => "manifest-collision",
                TransactionCollision::Completion => "completion-collision",
            };
            let final_name = match point {
                TransactionCollision::Manifest => MANIFEST_FILE_NAME,
                TransactionCollision::Completion => COMPLETION_FILE_NAME,
            };
            let directory = TestDirectory::new(label);
            let sentinel = format!("injected {label}").into_bytes();
            let observer = TransactionCollisionObserver {
                point,
                bytes: sentinel.clone(),
            };
            let mut reservation =
                ArtifactReservation::begin_journaled("handshake", &directory.0, journal_metadata())
                    .expect("journaled reservation");
            reservation
                .seal_journal(json!({"primary_artifact": "handshake.json"}))
                .expect("seal journal");

            assert!(
                reservation
                    .commit_observed(&json!({"status": "failed"}), &observer)
                    .is_err()
            );
            assert_eq!(
                fs::read(directory.0.join(final_name)).expect("preserved collision"),
                sentinel
            );
            assert!(
                !directory.0.join(COMPLETION_FILE_NAME).is_file()
                    || matches!(point, TransactionCollision::Completion)
            );
            assert_eq!(
                classify_run_directory(&directory.0).status,
                match point {
                    TransactionCollision::Manifest => RunDirectoryStatus::Polluted,
                    TransactionCollision::Completion => RunDirectoryStatus::Polluted,
                }
            );
        }
    }

    #[test]
    fn classifier_rejects_same_bytes_with_a_different_final_file_identity() {
        let directory = TestDirectory::new("classifier-file-identity");
        let mut reservation =
            ArtifactReservation::begin_journaled("handshake", &directory.0, journal_metadata())
                .expect("journaled reservation");
        reservation
            .seal_journal(json!({"primary_artifact": "handshake.json"}))
            .expect("seal journal");
        reservation
            .commit(&json!({"status": "failed"}))
            .expect("completed transaction");
        let reservation: Value = serde_json::from_slice(
            &fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("reservation"),
        )
        .expect("reservation JSON");
        let staging = reservation["primary_artifact"]["staging"]
            .as_str()
            .expect("staging");
        let staging_bytes = fs::read(directory.0.join(staging)).expect("staging bytes");

        fs::remove_file(directory.0.join("handshake.json")).expect("remove final link");
        fs::write(directory.0.join("handshake.json"), &staging_bytes)
            .expect("replace with equal bytes");
        assert_eq!(
            fs::read(directory.0.join("handshake.json")).expect("replacement"),
            staging_bytes
        );
        assert_eq!(
            classify_run_directory(&directory.0).status,
            RunDirectoryStatus::Polluted
        );
    }

    #[derive(Default)]
    struct MutationObserver {
        attempts: RefCell<Vec<(OwnedFileRole, bool, bool)>>,
    }

    impl ArtifactObserver for MutationObserver {
        fn after_file_synced(&self, role: OwnedFileRole, path: &Path) {
            let delete_blocked = fs::remove_file(path).is_err();
            let write_blocked = fs::write(path, b"replacement evidence").is_err();
            self.attempts
                .borrow_mut()
                .push((role, delete_blocked, write_blocked));
        }
    }

    #[test]
    fn manifest_and_completion_staging_remain_retained_through_verification() {
        let directory = TestDirectory::new("retained-transaction-files");
        let observer = MutationObserver::default();
        let mut reservation =
            ArtifactReservation::begin_journaled("handshake", &directory.0, journal_metadata())
                .expect("journaled reservation");
        reservation
            .seal_journal(json!({"primary_artifact": "handshake.json"}))
            .expect("seal journal");

        reservation
            .commit_observed(&json!({"status": "failed"}), &observer)
            .expect("transaction");

        let attempts = observer.attempts.borrow();
        assert_eq!(attempts.len(), 3);
        assert!(attempts.iter().all(|(_, delete, write)| *delete && *write));
        assert!(
            attempts
                .iter()
                .any(|(role, _, _)| *role == OwnedFileRole::Primary)
        );
        assert!(
            attempts
                .iter()
                .any(|(role, _, _)| *role == OwnedFileRole::Manifest)
        );
        assert!(
            attempts
                .iter()
                .any(|(role, _, _)| *role == OwnedFileRole::Completion)
        );
    }

    #[test]
    fn retained_handles_reject_reservation_primary_and_auxiliary_mutation() {
        let directory = TestDirectory::new("retained-handles");
        let observer = MutationObserver::default();
        let mut reservation =
            ArtifactReservation::begin_observed("sequence", &directory.0, &observer)
                .expect("reserve run directory");
        reservation
            .stage_auxiliary_observed(
                AuxiliaryKind::SequenceTimingsCsv,
                b"sequence,dispatch_ns\n1,10\n".to_vec(),
                &observer,
            )
            .expect("stage timings");

        reservation
            .commit_observed(&json!({"status": "failed"}), &observer)
            .expect("publish owned artifacts");

        let attempts = observer.attempts.borrow();
        assert_eq!(attempts.len(), 3);
        assert!(attempts.iter().all(|(_, delete, write)| *delete && *write));
        assert!(
            attempts
                .iter()
                .any(|(role, _, _)| *role == OwnedFileRole::Reservation)
        );
        assert!(
            attempts
                .iter()
                .any(|(role, _, _)| *role == OwnedFileRole::Primary)
        );
        assert!(attempts.iter().any(|(role, _, _)| {
            *role == OwnedFileRole::Auxiliary(AuxiliaryKind::SequenceTimingsCsv)
        }));
    }

    #[test]
    fn sequence_auxiliary_is_published_from_the_owned_staging_file() {
        let directory = TestDirectory::new("sequence-auxiliary");
        let bytes = b"sequence,dispatch_ns\n1,10\n".to_vec();
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        reservation
            .stage_auxiliary(AuxiliaryKind::SequenceTimingsCsv, bytes.clone())
            .expect("stage timings");

        reservation
            .commit(&json!({"status": "failed"}))
            .expect("publish artifact");

        assert_eq!(
            fs::read(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME)).expect("read final timings"),
            bytes
        );
        let tombstone: Value = serde_json::from_slice(
            &fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("read reservation"),
        )
        .expect("parse reservation");
        let staging = tombstone["auxiliary_artifacts"][0]["staging"]
            .as_str()
            .expect("staging name");
        assert_eq!(
            fs::read(directory.0.join(staging)).expect("read retained staging"),
            bytes
        );
    }

    #[test]
    fn injected_sequence_final_is_not_accepted_as_owned_auxiliary() {
        let directory = TestDirectory::new("injected-sequence-final");
        let reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        let injected = b"injected timing evidence\n";
        fs::write(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME), injected)
            .expect("inject timing file");

        let result = reservation.commit(&json!({"status": "failed"}));

        assert!(result.is_err());
        assert_eq!(
            fs::read(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME)).expect("read injected bytes"),
            injected
        );
        assert!(!directory.0.join("sequence.json").exists());
    }

    struct PublishPollutionObserver;

    impl ArtifactObserver for PublishPollutionObserver {
        fn after_all_published(&self, directory: &Path) {
            fs::write(directory.join("unexpected.sentinel"), b"external evidence")
                .expect("inject post-publication pollution");
        }
    }

    #[test]
    fn post_publication_pollution_fails_without_deleting_final_artifacts() {
        let directory = TestDirectory::new("post-publication-pollution");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");

        let result =
            reservation.commit_observed(&json!({"status": "failed"}), &PublishPollutionObserver);

        assert!(result.is_err());
        assert!(directory.0.join("unknown.json").exists());
        assert!(directory.0.join("unexpected.sentinel").exists());
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
    }

    struct AuxiliaryCollisionObserver;

    impl ArtifactObserver for AuxiliaryCollisionObserver {
        fn after_pre_publish_validation(&self, directory: &Path) {
            fs::write(
                directory.join(SEQUENCE_TIMINGS_FILE_NAME),
                b"raced auxiliary evidence",
            )
            .expect("inject auxiliary final");
        }
    }

    #[test]
    fn auxiliary_publish_collision_preserves_injected_bytes() {
        let directory = TestDirectory::new("auxiliary-publish-collision");
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        reservation
            .stage_auxiliary(
                AuxiliaryKind::SequenceTimingsCsv,
                b"owned timing evidence".to_vec(),
            )
            .expect("stage timings");

        let result =
            reservation.commit_observed(&json!({"status": "failed"}), &AuxiliaryCollisionObserver);

        assert!(result.is_err());
        assert_eq!(
            fs::read(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME)).expect("read injected final"),
            b"raced auxiliary evidence"
        );
        assert!(!directory.0.join("sequence.json").exists());
    }

    struct PrimaryCollisionObserver;

    impl ArtifactObserver for PrimaryCollisionObserver {
        fn after_auxiliary_published(&self, _kind: AuxiliaryKind, directory: &Path) {
            fs::write(directory.join("sequence.json"), b"raced primary evidence")
                .expect("inject primary final");
        }
    }

    #[test]
    fn primary_publish_collision_does_not_roll_back_auxiliary() {
        let directory = TestDirectory::new("primary-publish-collision");
        let timing_bytes = b"owned timing evidence".to_vec();
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        reservation
            .stage_auxiliary(AuxiliaryKind::SequenceTimingsCsv, timing_bytes.clone())
            .expect("stage timings");

        let result =
            reservation.commit_observed(&json!({"status": "failed"}), &PrimaryCollisionObserver);

        assert!(result.is_err());
        assert_eq!(
            fs::read(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME)).expect("read auxiliary final"),
            timing_bytes
        );
        assert_eq!(
            fs::read(directory.0.join("sequence.json")).expect("read injected primary"),
            b"raced primary evidence"
        );
    }

    #[test]
    fn preexisting_reservation_is_preserved_without_other_writes() {
        let directory = TestDirectory::new("preexisting-reservation");
        let sentinel = b"reservation owned by another invocation";
        fs::write(directory.0.join(RESERVATION_FILE_NAME), sentinel)
            .expect("seed reservation sentinel");

        let result = ArtifactReservation::begin("unknown", &directory.0);

        assert!(result.is_err());
        assert_eq!(
            fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("read sentinel"),
            sentinel
        );
        assert_eq!(
            fs::read_dir(&directory.0)
                .expect("inspect directory")
                .count(),
            1
        );
    }

    #[test]
    fn injected_planned_auxiliary_staging_cannot_create_an_owned_token() {
        let directory = TestDirectory::new("injected-auxiliary-staging");
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        let staging = reservation.planned_auxiliaries[0]
            .artifact
            .staging_path(&directory.0);
        let primary_staging = reservation.primary.staging_path(&directory.0);
        let injected = b"injected auxiliary staging";
        fs::write(&staging, injected).expect("inject planned staging");

        let result = reservation.stage_auxiliary(
            AuxiliaryKind::SequenceTimingsCsv,
            b"owned auxiliary bytes".to_vec(),
        );

        assert!(result.is_err());
        assert_eq!(fs::read(&staging).expect("read injected staging"), injected);
        fs::remove_file(staging).expect("remove injected staging");

        let commit = reservation.commit(&json!({"status": "failed"}));

        assert!(
            commit
                .expect_err("poisoned reservation must not commit")
                .contains("poisoned")
        );
        assert!(!primary_staging.exists());
        assert!(!directory.0.join(SEQUENCE_TIMINGS_FILE_NAME).exists());
        assert!(!directory.0.join("sequence.json").exists());
        assert_eq!(
            fs::read_dir(&directory.0)
                .expect("inspect poisoned directory")
                .count(),
            1
        );
    }

    struct ReservationPollutionObserver;

    impl ArtifactObserver for ReservationPollutionObserver {
        fn after_file_synced(&self, role: OwnedFileRole, path: &Path) {
            if role == OwnedFileRole::Reservation {
                fs::write(
                    path.parent()
                        .expect("reservation path has a parent")
                        .join("raced.sentinel"),
                    b"raced directory entry",
                )
                .expect("inject admission pollution");
            }
        }
    }

    #[test]
    fn admission_pollution_after_create_new_is_incomplete_and_retained() {
        let directory = TestDirectory::new("admission-pollution");

        let result = ArtifactReservation::begin_observed(
            "unknown",
            &directory.0,
            &ReservationPollutionObserver,
        );

        assert!(result.is_err());
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
        assert_eq!(
            fs::read(directory.0.join("raced.sentinel")).expect("read raced sentinel"),
            b"raced directory entry"
        );
    }

    #[test]
    fn reservation_schema_contains_only_relative_single_component_names() {
        let directory = TestDirectory::new("relative-schema");
        let _reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        let document: Value = serde_json::from_slice(
            &fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("read reservation"),
        )
        .expect("parse reservation");

        assert_eq!(document["schema_version"], 1);
        assert_eq!(document["kind"], "run_directory_reservation");
        assert_eq!(document["command"], "sequence");
        let names = [
            document["primary_artifact"]["final"]
                .as_str()
                .expect("primary final"),
            document["primary_artifact"]["staging"]
                .as_str()
                .expect("primary staging"),
            document["auxiliary_artifacts"][0]["final"]
                .as_str()
                .expect("auxiliary final"),
            document["auxiliary_artifacts"][0]["staging"]
                .as_str()
                .expect("auxiliary staging"),
            document["evidence_transaction"]["journal"]
                .as_str()
                .expect("journal"),
            document["evidence_transaction"]["manifest"]["final"]
                .as_str()
                .expect("manifest final"),
            document["evidence_transaction"]["manifest"]["staging"]
                .as_str()
                .expect("manifest staging"),
            document["evidence_transaction"]["completion"]["final"]
                .as_str()
                .expect("completion final"),
            document["evidence_transaction"]["completion"]["staging"]
                .as_str()
                .expect("completion staging"),
        ];
        for name in names {
            let path = Path::new(name);
            assert!(path.is_relative());
            assert_eq!(path.components().count(), 1);
        }
    }

    #[test]
    fn command_without_auxiliary_plan_cannot_stage_sequence_timings() {
        let directory = TestDirectory::new("unplanned-auxiliary");
        let mut reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");

        let result = reservation.stage_auxiliary(
            AuxiliaryKind::SequenceTimingsCsv,
            b"unplanned bytes".to_vec(),
        );

        assert!(result.is_err());
        assert_eq!(
            fs::read_dir(&directory.0)
                .expect("inspect directory")
                .count(),
            1
        );
    }

    #[test]
    fn concurrent_commands_have_exactly_one_directory_owner() {
        let directory = TestDirectory::new("concurrent-commands");
        let barrier = Arc::new(Barrier::new(2));
        let attempts = ["unknown", "amiibo"].map(|command| {
            let path = directory.0.clone();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                barrier.wait();
                ArtifactReservation::begin(command, &path).is_ok()
            })
        });

        let succeeded = attempts
            .into_iter()
            .map(|attempt| attempt.join().expect("reservation thread did not panic"))
            .filter(|succeeded| *succeeded)
            .count();

        assert_eq!(succeeded, 1);
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
    }

    #[test]
    fn successful_publication_does_not_change_reservation_bytes() {
        let directory = TestDirectory::new("immutable-reservation");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let before = fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("read reservation");

        reservation
            .commit(&json!({"status": "failed"}))
            .expect("publish artifact");

        assert_eq!(
            fs::read(directory.0.join(RESERVATION_FILE_NAME)).expect("read reservation again"),
            before
        );
    }

    #[derive(Default)]
    struct PublishedMutationObserver {
        attempts: RefCell<Option<(bool, bool)>>,
    }

    impl ArtifactObserver for PublishedMutationObserver {
        fn after_all_published(&self, directory: &Path) {
            let final_path = directory.join("unknown.json");
            let delete_blocked = fs::remove_file(&final_path).is_err();
            let write_blocked = fs::write(final_path, b"replacement final").is_err();
            self.attempts
                .borrow_mut()
                .replace((delete_blocked, write_blocked));
        }
    }

    #[test]
    fn retained_source_and_final_handles_protect_the_published_final_link() {
        let directory = TestDirectory::new("published-final-handle");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let observer = PublishedMutationObserver::default();

        reservation
            .commit_observed(&json!({"status": "failed"}), &observer)
            .expect("publish artifact");

        assert_eq!(*observer.attempts.borrow(), Some((true, true)));
        assert_eq!(
            serde_json::from_slice::<Value>(
                &fs::read(directory.0.join("unknown.json")).expect("read final")
            )
            .expect("parse final"),
            json!({"status": "failed"})
        );

        let final_path = directory.0.join("unknown.json");
        fs::remove_file(&final_path).expect("handles are closed after commit returns");
        fs::write(&final_path, b"post-commit replacement")
            .expect("post-commit mutation is not permanently blocked");
        assert_eq!(
            fs::read(final_path).expect("read post-commit replacement"),
            b"post-commit replacement"
        );
    }

    struct RemoveFinalAfterHardLinkObserver;

    impl ArtifactObserver for RemoveFinalAfterHardLinkObserver {
        fn after_hard_link_created(&self, role: OwnedFileRole, _staging: &Path, final_path: &Path) {
            if role == OwnedFileRole::Primary {
                fs::remove_file(final_path).expect("remove unguarded final link");
            }
        }
    }

    #[test]
    fn missing_final_between_hard_link_and_guard_is_incomplete() {
        let directory = TestDirectory::new("missing-before-final-guard");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let staging = reservation.primary.staging_path(&directory.0);

        let result = reservation.commit_observed(
            &json!({"status": "failed"}),
            &RemoveFinalAfterHardLinkObserver,
        );

        assert!(result.is_err());
        assert!(staging.exists());
        assert!(!directory.0.join("unknown.json").exists());
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
    }

    struct ReplaceFinalAfterHardLinkObserver {
        bytes: Vec<u8>,
    }

    impl ArtifactObserver for ReplaceFinalAfterHardLinkObserver {
        fn after_hard_link_created(&self, role: OwnedFileRole, _staging: &Path, final_path: &Path) {
            if role == OwnedFileRole::Primary {
                fs::remove_file(final_path).expect("remove unguarded final link");
                fs::write(final_path, &self.bytes).expect("replace final with exact bytes");
            }
        }
    }

    #[test]
    fn exact_byte_replacement_before_guard_fails_file_identity() {
        let directory = TestDirectory::new("identity-replacement");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let document = json!({"status": "failed"});
        let expected = serde_json::to_vec_pretty(&document).expect("serialize expected document");

        let result = reservation.commit_observed(
            &document,
            &ReplaceFinalAfterHardLinkObserver {
                bytes: expected.clone(),
            },
        );

        assert!(
            result
                .expect_err("replacement must not publish")
                .contains("does not reference")
        );
        assert_eq!(
            fs::read(directory.0.join("unknown.json")).expect("read replacement"),
            expected
        );
    }

    struct JunctionRetargetFixture {
        root: PathBuf,
        owned_directory: PathBuf,
        alternate_directory: PathBuf,
        run_directory: PathBuf,
        retired_junction: PathBuf,
        replacement_junction: PathBuf,
    }

    impl JunctionRetargetFixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!(
                "easycon-hardware-artifact-junction-retarget-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock after epoch")
                    .as_nanos()
            ));
            let owned_directory = root.join("owned");
            let alternate_directory = root.join("alternate");
            let run_directory = root.join("run");
            let retired_junction = root.join("retired-run");
            let replacement_junction = root.join("replacement-run");
            fs::create_dir(&root).expect("create junction fixture root");
            fs::create_dir(&owned_directory).expect("create owned target directory");
            fs::create_dir(&alternate_directory).expect("create alternate target directory");
            junction::create(&owned_directory, &run_directory)
                .expect("create run-directory junction");
            Self {
                root,
                owned_directory,
                alternate_directory,
                run_directory,
                retired_junction,
                replacement_junction,
            }
        }
    }

    impl Drop for JunctionRetargetFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.run_directory);
            let _ = fs::remove_dir_all(&self.retired_junction);
            let _ = fs::remove_dir_all(&self.replacement_junction);
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct RetargetAncestorJunctionObserver<'a> {
        fixture: &'a JunctionRetargetFixture,
    }

    impl ArtifactObserver for RetargetAncestorJunctionObserver<'_> {
        fn after_hard_link_created(&self, role: OwnedFileRole, staging: &Path, final_path: &Path) {
            if role != OwnedFileRole::Primary {
                return;
            }
            let staging_name = staging.file_name().expect("staging file name");
            let final_name = final_path.file_name().expect("final file name");
            fs::copy(
                self.fixture.run_directory.join(RESERVATION_FILE_NAME),
                self.fixture.alternate_directory.join(RESERVATION_FILE_NAME),
            )
            .expect("copy exact reservation to alternate directory");
            let alternate_staging = self.fixture.alternate_directory.join(staging_name);
            fs::copy(staging, &alternate_staging)
                .expect("copy exact staging bytes to alternate directory");
            fs::hard_link(
                &alternate_staging,
                self.fixture.alternate_directory.join(final_name),
            )
            .expect("link alternate staging to alternate final");

            junction::create(
                &self.fixture.alternate_directory,
                &self.fixture.replacement_junction,
            )
            .expect("create replacement junction");
            fs::rename(&self.fixture.run_directory, &self.fixture.retired_junction)
                .expect("retire original junction while source handles are retained");
            fs::rename(
                &self.fixture.replacement_junction,
                &self.fixture.run_directory,
            )
            .expect("retarget nominal run path to alternate directory");
        }
    }

    #[test]
    fn ancestor_junction_retarget_cannot_replace_the_retained_source_object() {
        let fixture = JunctionRetargetFixture::new();
        let reservation = ArtifactReservation::begin("unknown", &fixture.run_directory)
            .expect("reserve through junction");
        let document = json!({"status": "failed"});
        let expected = serde_json::to_vec_pretty(&document).expect("serialize expected document");

        let result = reservation.commit_observed(
            &document,
            &RetargetAncestorJunctionObserver { fixture: &fixture },
        );

        assert!(
            result
                .expect_err("alternate object must not publish")
                .contains("does not reference")
        );
        assert_eq!(
            fs::read(fixture.owned_directory.join("unknown.json")).expect("read owned final link"),
            expected
        );
        assert_eq!(
            fs::read(fixture.alternate_directory.join("unknown.json"))
                .expect("read alternate final link"),
            expected
        );
    }

    #[derive(Default)]
    struct RecordingGuardOpener {
        specs: RefCell<Vec<FinalGuardOpenSpec>>,
    }

    impl FinalGuardOpener for RecordingGuardOpener {
        fn open(&self, path: &Path, spec: FinalGuardOpenSpec) -> Result<File, String> {
            self.specs.borrow_mut().push(spec);
            SYSTEM_FINAL_GUARD_OPENER.open(path, spec)
        }
    }

    #[test]
    fn commit_passes_the_exact_no_follow_final_guard_spec() {
        let directory = TestDirectory::new("final-guard-spec");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let opener = RecordingGuardOpener::default();

        let output = reservation
            .commit_with_services(
                &json!({"status": "failed"}),
                &NOOP_ARTIFACT_OBSERVER,
                &opener,
                &SYSTEM_FINAL_GUARD_METADATA_PROVIDER,
                &SYSTEM_FILE_IDENTITY_PROVIDER,
            )
            .expect("publish artifact");

        assert_eq!(output, PathBuf::from("unknown.json"));
        assert_eq!(opener.specs.borrow().as_slice(), &[final_guard_open_spec()]);
        let spec = opener.specs.borrow()[0];
        assert!(spec.read);
        assert!(!spec.write);
        assert_eq!(spec.share_mode, FILE_SHARE_READ | FILE_SHARE_WRITE);
        assert_ne!(spec.custom_flags & FILE_FLAG_OPEN_REPARSE_POINT, 0);
    }

    struct StaticFinalGuardMetadataProvider {
        metadata: FinalGuardMetadata,
    }

    impl FinalGuardMetadataProvider for StaticFinalGuardMetadataProvider {
        fn metadata(&self, _guard: &File, _path: &Path) -> Result<FinalGuardMetadata, String> {
            Ok(self.metadata)
        }
    }

    #[test]
    fn injected_reparse_metadata_is_rejected_by_the_production_validator() {
        let directory = TestDirectory::new("reparse-final");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let metadata_provider = StaticFinalGuardMetadataProvider {
            metadata: FinalGuardMetadata {
                file_attributes: FILE_ATTRIBUTE_REPARSE_POINT,
                is_file: true,
            },
        };

        let result = reservation.commit_with_services(
            &json!({"status": "failed"}),
            &NOOP_ARTIFACT_OBSERVER,
            &SYSTEM_FINAL_GUARD_OPENER,
            &metadata_provider,
            &SYSTEM_FILE_IDENTITY_PROVIDER,
        );

        assert!(
            result
                .expect_err("reparse final must not publish")
                .contains("reparse point")
        );
        assert!(directory.0.join("unknown.json").exists());
        assert!(directory.0.join(RESERVATION_FILE_NAME).exists());
    }

    struct FailFinalIdentityProvider {
        file_name: &'static str,
    }

    impl FileIdentityProvider for FailFinalIdentityProvider {
        fn high_resolution_identity(
            &self,
            file: &File,
            path: &Path,
        ) -> Result<HighResolutionFileIdentity, String> {
            if path.file_name().and_then(|name| name.to_str()) == Some(self.file_name) {
                return Err(format!(
                    "injected high-resolution identity failure for {}",
                    path.display()
                ));
            }
            SYSTEM_FILE_IDENTITY_PROVIDER.high_resolution_identity(file, path)
        }
    }

    #[test]
    fn auxiliary_identity_failure_preserves_link_and_stops_primary_final() {
        let directory = TestDirectory::new("auxiliary-identity-failure");
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        reservation
            .stage_auxiliary(
                AuxiliaryKind::SequenceTimingsCsv,
                b"owned timing evidence".to_vec(),
            )
            .expect("stage timings");

        let result = reservation.commit_with_services(
            &json!({"status": "failed"}),
            &NOOP_ARTIFACT_OBSERVER,
            &SYSTEM_FINAL_GUARD_OPENER,
            &SYSTEM_FINAL_GUARD_METADATA_PROVIDER,
            &FailFinalIdentityProvider {
                file_name: SEQUENCE_TIMINGS_FILE_NAME,
            },
        );

        assert!(
            result
                .expect_err("identity failure must not publish")
                .contains("injected high-resolution")
        );
        assert!(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME).exists());
        assert!(!directory.0.join("sequence.json").exists());
    }

    #[test]
    fn primary_identity_failure_preserves_auxiliary_and_primary_links() {
        let directory = TestDirectory::new("primary-identity-failure");
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        reservation
            .stage_auxiliary(
                AuxiliaryKind::SequenceTimingsCsv,
                b"owned timing evidence".to_vec(),
            )
            .expect("stage timings");

        let result = reservation.commit_with_services(
            &json!({"status": "failed"}),
            &NOOP_ARTIFACT_OBSERVER,
            &SYSTEM_FINAL_GUARD_OPENER,
            &SYSTEM_FINAL_GUARD_METADATA_PROVIDER,
            &FailFinalIdentityProvider {
                file_name: "sequence.json",
            },
        );

        assert!(
            result
                .expect_err("identity failure must not publish")
                .contains("injected high-resolution")
        );
        assert!(directory.0.join(SEQUENCE_TIMINGS_FILE_NAME).exists());
        assert!(directory.0.join("sequence.json").exists());
    }

    #[derive(Default)]
    struct PrePublishMutationObserver {
        attempts: RefCell<Vec<(bool, bool)>>,
    }

    impl ArtifactObserver for PrePublishMutationObserver {
        fn after_pre_publish_validation(&self, directory: &Path) {
            for entry in fs::read_dir(directory).expect("inspect staged artifacts") {
                let path = entry.expect("read staged artifact").path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("staged") {
                    continue;
                }
                let delete_blocked = fs::remove_file(&path).is_err();
                let write_blocked = fs::write(path, b"replacement staging").is_err();
                self.attempts
                    .borrow_mut()
                    .push((delete_blocked, write_blocked));
            }
        }
    }

    #[test]
    fn retained_staging_handles_reject_mutation_after_pre_publish_readback() {
        let directory = TestDirectory::new("pre-publish-staging-handles");
        let mut reservation =
            ArtifactReservation::begin("sequence", &directory.0).expect("reserve run directory");
        reservation
            .stage_auxiliary(
                AuxiliaryKind::SequenceTimingsCsv,
                b"owned timing evidence".to_vec(),
            )
            .expect("stage timings");
        let observer = PrePublishMutationObserver::default();

        reservation
            .commit_observed(&json!({"status": "failed"}), &observer)
            .expect("publish artifact");

        assert_eq!(observer.attempts.borrow().len(), 2);
        assert!(
            observer
                .attempts
                .borrow()
                .iter()
                .all(|(delete, write)| *delete && *write)
        );
    }

    #[test]
    fn retained_tombstone_rejects_mutation_after_begin_readback() {
        let directory = TestDirectory::new("post-begin-tombstone-handle");
        let reservation =
            ArtifactReservation::begin("unknown", &directory.0).expect("reserve run directory");
        let tombstone = directory.0.join(RESERVATION_FILE_NAME);

        assert!(fs::remove_file(&tombstone).is_err());
        assert!(fs::write(&tombstone, b"replacement reservation").is_err());
        reservation
            .commit(&json!({"status": "failed"}))
            .expect("publish artifact");
    }
}
