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

#[cfg(windows)]
use file_id::FileId;
use serde_json::{Value, json};
#[cfg(windows)]
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

pub(crate) const RESERVATION_FILE_NAME: &str = ".easycon-hardware-run.reservation.json";
pub(crate) const SEQUENCE_TIMINGS_FILE_NAME: &str = "sequence-timings.csv";

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
}

trait ArtifactObserver {
    fn after_file_synced(&self, _role: OwnedFileRole, _path: &Path) {}

    fn after_pre_publish_validation(&self, _directory: &Path) {}

    fn after_hard_link_created(&self, _role: OwnedFileRole, _staging: &Path, _final_path: &Path) {}

    fn after_auxiliary_published(&self, _kind: AuxiliaryKind, _directory: &Path) {}

    fn after_all_published(&self, _directory: &Path) {}
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
    fn high_resolution_identity(&self, path: &Path) -> Result<HighResolutionFileIdentity, String>;
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
        Ok(())
    }
}

struct PublishedFile {
    path: PathBuf,
    guard: File,
}

impl PublishedFile {
    fn verify(&mut self, expected: &[u8]) -> Result<(), String> {
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
        Ok(())
    }
}

struct OwnedAuxiliary {
    plan: AuxiliaryPlan,
    staging: OwnedFile,
}

pub(crate) struct ArtifactReservation {
    directory: PathBuf,
    command: String,
    tombstone: OwnedFile,
    primary: ArtifactPlan,
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
            tombstone,
            primary,
            planned_auxiliaries,
            owned_auxiliaries: Vec::new(),
            poisoned: None,
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
        Ok(PathBuf::from(&self.primary.final_name))
    }

    fn pre_primary_entries(&self) -> BTreeSet<OsString> {
        let mut entries = BTreeSet::new();
        entries.insert(OsString::from(RESERVATION_FILE_NAME));
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
    fn high_resolution_identity(&self, path: &Path) -> Result<HighResolutionFileIdentity, String> {
        #[cfg(windows)]
        {
            match file_id::get_high_res_file_id(path).map_err(|error| {
                format!(
                    "cannot read high-resolution file identity for {}: {error}",
                    path.display()
                )
            })? {
                FileId::HighRes {
                    volume_serial_number,
                    file_id,
                } => Ok(HighResolutionFileIdentity {
                    volume_serial_number,
                    file_id,
                }),
                _ => Err(format!(
                    "high-resolution file identity unavailable for {}",
                    path.display()
                )),
            }
        }
        #[cfg(not(windows))]
        {
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
    let staging_identity = identity_provider.high_resolution_identity(&staging.path)?;
    let final_identity = identity_provider.high_resolution_identity(final_path)?;
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
            path: &Path,
        ) -> Result<HighResolutionFileIdentity, String> {
            if path.file_name().and_then(|name| name.to_str()) == Some(self.file_name) {
                return Err(format!(
                    "injected high-resolution identity failure for {}",
                    path.display()
                ));
            }
            SYSTEM_FILE_IDENTITY_PROVIDER.high_resolution_identity(path)
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
