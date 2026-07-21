use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct BuildProvenance {
    schema_version: u32,
    package_version: String,
    git_commit: Option<String>,
    git_tree: Option<String>,
    tracked_dirty: Option<bool>,
    untracked_present: Option<bool>,
    tracked_source_sha256: Option<String>,
    cargo_lock_sha256: Option<String>,
    declared_trusted: bool,
}

impl BuildProvenance {
    pub(crate) fn embedded() -> Self {
        Self {
            schema_version: env!("EASYCON_BUILD_PROVENANCE_SCHEMA")
                .parse()
                .unwrap_or_default(),
            package_version: env!("CARGO_PKG_VERSION").to_owned(),
            git_commit: known(env!("EASYCON_BUILD_GIT_COMMIT")),
            git_tree: known(env!("EASYCON_BUILD_GIT_TREE")),
            tracked_dirty: match env!("EASYCON_BUILD_TRACKED_DIRTY") {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            untracked_present: match env!("EASYCON_BUILD_UNTRACKED_PRESENT") {
                "true" => Some(true),
                "false" => Some(false),
                _ => None,
            },
            tracked_source_sha256: known(env!("EASYCON_BUILD_TRACKED_SOURCE_SHA256")),
            cargo_lock_sha256: known(env!("EASYCON_BUILD_CARGO_LOCK_SHA256")),
            declared_trusted: env!("EASYCON_BUILD_PROVENANCE_TRUSTED") == "true",
        }
    }

    pub(crate) fn trusted(&self) -> bool {
        self.declared_trusted
            && self.schema_version == 1
            && !self.package_version.trim().is_empty()
            && self
                .git_commit
                .as_deref()
                .is_some_and(|value| valid_hex(value, 40))
            && self
                .git_tree
                .as_deref()
                .is_some_and(|value| valid_hex(value, 40))
            && self.tracked_dirty.is_some()
            && self.untracked_present == Some(false)
            && self
                .tracked_source_sha256
                .as_deref()
                .is_some_and(|value| valid_hex(value, 64))
            && self
                .cargo_lock_sha256
                .as_deref()
                .is_some_and(|value| valid_hex(value, 64))
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "schema_version": self.schema_version,
            "package_version": self.package_version,
            "git_commit": self.git_commit,
            "git_tree": self.git_tree,
            "tracked_dirty": self.tracked_dirty,
            "untracked_present": self.untracked_present,
            "tracked_source_sha256": self.tracked_source_sha256,
            "cargo_lock_sha256": self.cargo_lock_sha256,
            "trusted": self.trusted(),
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeProvenance {
    build: BuildProvenance,
    executable_sha256: Option<String>,
    executable_hash_error: Option<String>,
}

impl RuntimeProvenance {
    pub(crate) fn capture() -> Self {
        let build = BuildProvenance::embedded();
        let (executable_sha256, executable_hash_error) = match std::env::current_exe() {
            Ok(path) => match sha256_file(&path) {
                Ok(hash) => (Some(hash), None),
                Err(error) => (None, Some(error)),
            },
            Err(error) => (None, Some(format!("current_exe:{:?}", error.kind()))),
        };
        Self {
            build,
            executable_sha256,
            executable_hash_error,
        }
    }

    pub(crate) fn trusted(&self) -> bool {
        self.build.trusted()
            && self
                .executable_sha256
                .as_deref()
                .is_some_and(|value| valid_hex(value, 64))
            && self.executable_hash_error.is_none()
    }

    pub(crate) fn to_json(&self) -> Value {
        json!({
            "build": self.build.to_json(),
            "runtime": {
                "executable_sha256": self.executable_sha256,
                "executable_hash_error": self.executable_hash_error,
            },
            "trusted": self.trusted(),
        })
    }

    #[cfg(test)]
    pub(crate) fn synthetic(build: BuildProvenance, executable_sha256: Option<String>) -> Self {
        let executable_hash_error = executable_sha256
            .is_none()
            .then(|| "synthetic_unavailable".to_owned());
        Self {
            build,
            executable_sha256,
            executable_hash_error,
        }
    }

    #[cfg(test)]
    pub(crate) fn synthetic_with_trust(trusted: bool) -> Self {
        let build = BuildProvenance {
            schema_version: 1,
            package_version: "0.1.0".to_owned(),
            git_commit: Some("1".repeat(40)),
            git_tree: Some("2".repeat(40)),
            tracked_dirty: Some(false),
            untracked_present: Some(false),
            tracked_source_sha256: Some("A".repeat(64)),
            cargo_lock_sha256: Some("B".repeat(64)),
            declared_trusted: trusted,
        };
        Self::synthetic(build, Some("C".repeat(64)))
    }
}

pub(crate) fn sha256_bytes(bytes: &[u8]) -> String {
    hex_upper(&Sha256::digest(bytes))
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = File::open(path).map_err(|error| format!("open:{:?}", error.kind()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| format!("read:{:?}", error.kind()))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(hex_upper(&hasher.finalize()))
}

fn known(value: &str) -> Option<String> {
    (value != "unknown").then(|| value.to_owned())
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_upper(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trusted_build() -> BuildProvenance {
        BuildProvenance {
            schema_version: 1,
            package_version: "0.1.0".to_owned(),
            git_commit: Some("1".repeat(40)),
            git_tree: Some("2".repeat(40)),
            tracked_dirty: Some(true),
            untracked_present: Some(false),
            tracked_source_sha256: Some("A".repeat(64)),
            cargo_lock_sha256: Some("B".repeat(64)),
            declared_trusted: true,
        }
    }

    #[test]
    fn embedded_provenance_has_auditable_binary_bound_fields() {
        let provenance = RuntimeProvenance::capture();
        let document = provenance.to_json();

        assert_eq!(document["build"]["schema_version"], 1);
        assert!(
            document["build"]["git_commit"]
                .as_str()
                .is_some_and(|value| valid_hex(value, 40))
        );
        assert!(
            document["build"]["git_tree"]
                .as_str()
                .is_some_and(|value| valid_hex(value, 40))
        );
        assert!(
            document["build"]["cargo_lock_sha256"]
                .as_str()
                .is_some_and(|value| valid_hex(value, 64))
        );
        assert!(
            document["runtime"]["executable_sha256"]
                .as_str()
                .is_some_and(|value| valid_hex(value, 64))
        );
    }

    #[test]
    fn unknown_or_dirty_without_source_digest_is_untrusted() {
        let mut build = trusted_build();
        build.git_commit = None;
        assert!(!build.trusted());

        let mut build = trusted_build();
        build.tracked_source_sha256 = None;
        assert!(!build.trusted());

        let runtime = RuntimeProvenance::synthetic(build, Some("C".repeat(64)));
        assert!(!runtime.trusted());
    }

    #[test]
    fn sha256_is_uppercase_and_exact() {
        assert_eq!(
            sha256_bytes(b"abc"),
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        );
    }
}
