use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-env-changed=EASYCON_PROVENANCE_FORCE_UNTRUSTED");
    println!("cargo:rerun-if-env-changed=GIT_EXECUTABLE");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let repository = manifest_dir
        .parent()
        .and_then(Path::parent)
        .expect("hardware workspace is nested under the repository");
    let lock_path = manifest_dir.join("Cargo.lock");
    println!("cargo:rerun-if-changed={}", lock_path.display());

    let commit = git_text(repository, &["rev-parse", "--verify", "HEAD"]);
    let tree = git_text(repository, &["rev-parse", "--verify", "HEAD^{tree}"]);
    let dirty = git_output(
        repository,
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    )
    .map(|output| !output.is_empty());
    let untracked_present = git_output(
        repository,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
    )
    .map(|output| {
        output
            .split(|byte| *byte == 0)
            .any(|entry| entry.starts_with(b"?? "))
    });
    let (tracked_source_sha256, tracked_paths) = tracked_source_digest(repository);
    let cargo_lock_sha256 = fs::read(&lock_path).ok().map(|bytes| sha256(&bytes));

    for path in tracked_paths {
        if !path.to_string_lossy().contains(['\r', '\n']) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    emit_git_rerun_paths(repository);

    let forced_untrusted = env::var_os("EASYCON_PROVENANCE_FORCE_UNTRUSTED").is_some();
    let trusted = !forced_untrusted
        && commit.as_deref().is_some_and(|value| valid_hex(value, 40))
        && tree.as_deref().is_some_and(|value| valid_hex(value, 40))
        && dirty.is_some()
        && untracked_present == Some(false)
        && tracked_source_sha256
            .as_deref()
            .is_some_and(|value| valid_hex(value, 64))
        && cargo_lock_sha256
            .as_deref()
            .is_some_and(|value| valid_hex(value, 64));

    emit("EASYCON_BUILD_PROVENANCE_SCHEMA", "1");
    emit_optional("EASYCON_BUILD_GIT_COMMIT", commit.as_deref());
    emit_optional("EASYCON_BUILD_GIT_TREE", tree.as_deref());
    emit(
        "EASYCON_BUILD_TRACKED_DIRTY",
        dirty.map_or("unknown", |value| if value { "true" } else { "false" }),
    );
    emit(
        "EASYCON_BUILD_UNTRACKED_PRESENT",
        untracked_present.map_or("unknown", |value| if value { "true" } else { "false" }),
    );
    emit_optional(
        "EASYCON_BUILD_TRACKED_SOURCE_SHA256",
        tracked_source_sha256.as_deref(),
    );
    emit_optional(
        "EASYCON_BUILD_CARGO_LOCK_SHA256",
        cargo_lock_sha256.as_deref(),
    );
    emit(
        "EASYCON_BUILD_PROVENANCE_TRUSTED",
        if trusted { "true" } else { "false" },
    );
}

fn emit(name: &str, value: &str) {
    println!("cargo:rustc-env={name}={value}");
}

fn emit_optional(name: &str, value: Option<&str>) {
    emit(name, value.unwrap_or("unknown"));
}

fn git_text(repository: &Path, arguments: &[&str]) -> Option<String> {
    let output = git_output(repository, arguments)?;
    String::from_utf8(output)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn git_output(repository: &Path, arguments: &[&str]) -> Option<Vec<u8>> {
    let program = git_program();
    let output = match Command::new(&program)
        .arg("-C")
        .arg(repository)
        .args(arguments)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            println!("cargo:warning=provenance git launch failed: {error}");
            return None;
        }
    };
    if !output.status.success() {
        println!(
            "cargo:warning=provenance git {:?} failed: {}",
            arguments,
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return None;
    }
    Some(output.stdout)
}

fn git_program() -> PathBuf {
    if let Some(program) = env::var_os("GIT_EXECUTABLE").filter(|value| !value.is_empty()) {
        return PathBuf::from(program);
    }
    #[cfg(windows)]
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(root) = env::var_os(variable) {
            let root = PathBuf::from(root);
            let candidates = if variable == "LOCALAPPDATA" {
                vec![
                    root.join("Programs")
                        .join("Git")
                        .join("cmd")
                        .join("git.exe"),
                ]
            } else {
                vec![root.join("Git").join("cmd").join("git.exe")]
            };
            if let Some(program) = candidates.into_iter().find(|path| path.is_file()) {
                return program;
            }
        }
    }
    PathBuf::from("git")
}

fn tracked_source_digest(repository: &Path) -> (Option<String>, Vec<PathBuf>) {
    let Some(output) = git_output(repository, &["ls-files", "-z"]) else {
        return (None, Vec::new());
    };
    let mut relative_paths = output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8(path.to_vec()))
        .collect::<Result<Vec<_>, _>>()
        .ok();
    let Some(relative_paths) = relative_paths.as_mut() else {
        return (None, Vec::new());
    };
    relative_paths.sort_unstable();

    let mut hasher = Sha256::new();
    hasher.update(b"easycon-tracked-source-v1\0");
    let mut watched = Vec::with_capacity(relative_paths.len());
    for relative in relative_paths {
        let normalized = relative.replace('\\', "/");
        hash_field(&mut hasher, normalized.as_bytes());
        let path = repository.join(relative.as_str());
        watched.push(path.clone());
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hash_field(&mut hasher, b"missing");
                continue;
            }
            Err(_) => return (None, watched),
        };
        if metadata.file_type().is_symlink() {
            hash_field(&mut hasher, b"symlink");
            let Ok(target) = fs::read_link(&path) else {
                return (None, watched);
            };
            hash_field(&mut hasher, target.to_string_lossy().as_bytes());
        } else if metadata.is_file() {
            hash_field(&mut hasher, b"file");
            let Ok(bytes) = fs::read(&path) else {
                return (None, watched);
            };
            hash_field(&mut hasher, &bytes);
        } else {
            hash_field(&mut hasher, b"other");
        }
    }
    (Some(hex_upper(&hasher.finalize())), watched)
}

fn hash_field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update(u64::try_from(bytes.len()).unwrap_or(u64::MAX).to_le_bytes());
    hasher.update(bytes);
}

fn sha256(bytes: &[u8]) -> String {
    hex_upper(&Sha256::digest(bytes))
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

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn emit_git_rerun_paths(repository: &Path) {
    for arguments in [
        &["rev-parse", "--git-path", "HEAD"][..],
        &["rev-parse", "--git-path", "index"][..],
    ] {
        if let Some(path) = git_text(repository, arguments) {
            let path = absolute_git_path(repository, &path);
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    if let Some(reference) = git_text(repository, &["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git_text(repository, &["rev-parse", "--git-path", &reference])
    {
        let path = absolute_git_path(repository, &path);
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn absolute_git_path(repository: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        repository.join(path)
    }
}
