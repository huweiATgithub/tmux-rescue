use std::ffi::{CString, OsStr};
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use thiserror::Error;
use time::OffsetDateTime;
use uuid::{Uuid, Variant};

use crate::{CaptureConsistency, MAX_SNAPSHOT_BYTES, ValidatedSnapshot};

#[derive(Clone, Debug)]
pub struct StateStore {
    root: PathBuf,
}

trait PublicationFileSystem {
    fn write_temporary_snapshot(&self, path: &Path, bytes: &[u8]) -> Result<File, StorageIoError>;
    fn sync_temporary_snapshot(&self, file: &File) -> Result<(), StorageIoError>;
    fn rename_no_replace(&self, source: &Path, destination: &Path) -> Result<(), StorageIoError>;
    fn sync_directory(&self, path: &Path) -> Result<(), StorageIoError>;
    fn create_temporary_latest_symlink(
        &self,
        target: &Path,
        link: &Path,
    ) -> Result<(), StorageIoError>;
    fn rename_latest(&self, source: &Path, destination: &Path) -> Result<(), StorageIoError>;
    fn remove_file(&self, path: &Path);
}

struct RealPublicationFileSystem;

impl PublicationFileSystem for RealPublicationFileSystem {
    fn write_temporary_snapshot(&self, path: &Path, bytes: &[u8]) -> Result<File, StorageIoError> {
        write_temporary_snapshot(path, bytes)
    }

    fn sync_temporary_snapshot(&self, file: &File) -> Result<(), StorageIoError> {
        sync_temporary_snapshot(file)
    }

    fn rename_no_replace(&self, source: &Path, destination: &Path) -> Result<(), StorageIoError> {
        rename_no_replace(source, destination)
    }

    fn sync_directory(&self, path: &Path) -> Result<(), StorageIoError> {
        sync_directory(path)
    }

    fn create_temporary_latest_symlink(
        &self,
        target: &Path,
        link: &Path,
    ) -> Result<(), StorageIoError> {
        symlink(target, link).map_err(|error| StorageIoError {
            operation: "create temporary latest symlink".to_owned(),
            reason: error.to_string(),
        })
    }

    fn rename_latest(&self, source: &Path, destination: &Path) -> Result<(), StorageIoError> {
        fs::rename(source, destination).map_err(|error| StorageIoError {
            operation: "replace latest symlink".to_owned(),
            reason: error.to_string(),
        })
    }

    fn remove_file(&self, path: &Path) {
        let _ = fs::remove_file(path);
    }
}

impl StateStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn from_environment() -> Result<Self, StorageError> {
        let root = match std::env::var_os("XDG_STATE_HOME") {
            Some(root) => PathBuf::from(root).join("tmux-rescue"),
            None => PathBuf::from(std::env::var_os("HOME").ok_or(
                StorageError::StateRootUnavailable("HOME is not set".to_owned()),
            )?)
            .join(".local/state/tmux-rescue"),
        };
        if !root.is_absolute() {
            return Err(StorageError::StateRootUnavailable(
                "state root is not absolute".to_owned(),
            ));
        }
        Ok(Self::new(root))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn publish(&self, snapshot: &ValidatedSnapshot) -> SnapshotPublication {
        self.publish_with_file_system(snapshot, &RealPublicationFileSystem)
    }

    fn publish_with_file_system(
        &self,
        snapshot: &ValidatedSnapshot,
        file_system: &impl PublicationFileSystem,
    ) -> SnapshotPublication {
        let snapshots = self.root.join("snapshots");
        if let Err(error) =
            prepare_directory(&self.root).and_then(|()| prepare_directory(&snapshots))
        {
            return SnapshotPublication::NotPublished(error.into());
        }

        let bytes = match snapshot.to_json_pretty() {
            Ok(bytes) if bytes.len() <= MAX_SNAPSHOT_BYTES => bytes,
            Ok(bytes) => {
                return SnapshotPublication::NotPublished(PublicationFailure::new(
                    "serialize snapshot",
                    format!(
                        "serialized snapshot is {} bytes; the maximum is {MAX_SNAPSHOT_BYTES}",
                        bytes.len()
                    ),
                ));
            }
            Err(error) => {
                return SnapshotPublication::NotPublished(PublicationFailure::new(
                    "serialize snapshot",
                    error.to_string(),
                ));
            }
        };

        let key = SnapshotKey::for_snapshot(snapshot, Uuid::new_v4());
        let file_name = key.file_name();
        let final_path = snapshots.join(&file_name);
        let temporary_path = snapshots.join(format!(".snapshot-{}.tmp", key.suffix));
        if let Err(error) =
            commit_immutable_snapshot(file_system, &temporary_path, &final_path, &bytes)
        {
            return SnapshotPublication::NotPublished(error.into());
        }

        if let Err(error) = file_system.sync_directory(&snapshots) {
            return SnapshotPublication::PublicationIndeterminate {
                candidate_path: final_path,
                failure: error.into(),
            };
        }

        let latest = match self.update_latest(&key, file_system) {
            Ok(disposition) => disposition,
            Err(error) => LatestDisposition::UpdateFailed(error),
        };
        SnapshotPublication::Published {
            snapshot_path: final_path,
            consistency: snapshot.consistency().clone(),
            latest,
        }
    }

    pub fn load_latest(&self) -> Result<LoadedSnapshot, StorageError> {
        self.load_latest_after_selection(|| {})
    }

    fn load_latest_after_selection(
        &self,
        after_selection: impl FnOnce(),
    ) -> Result<LoadedSnapshot, StorageError> {
        let latest = self.root.join("latest");
        let target = fs::read_link(&latest).map_err(|error| StorageError::InvalidLatest {
            reason: error.to_string(),
        })?;
        let key = validate_latest_target(&target)?;
        after_selection();
        load_snapshot_beneath(&self.root.join("snapshots"), &key)
    }

    pub fn load_explicit(&self, path: &Path) -> Result<LoadedSnapshot, StorageError> {
        Self::load_explicit_path(path)
    }

    pub fn load_explicit_path(path: &Path) -> Result<LoadedSnapshot, StorageError> {
        load_snapshot_file(path.to_owned(), false)
    }

    fn update_latest(
        &self,
        key: &SnapshotKey,
        file_system: &impl PublicationFileSystem,
    ) -> Result<LatestDisposition, StorageError> {
        let _lock = LatestLock::acquire(&self.root)?;
        let current = self.inspect_latest();
        let disposition = match &current {
            CurrentLatest::Valid { key: current_key } => {
                if key <= current_key {
                    return Ok(LatestDisposition::KeptNewer);
                }
                LatestDisposition::Updated
            }
            CurrentLatest::Missing => LatestDisposition::Updated,
            CurrentLatest::Invalid => LatestDisposition::ReplacedInvalid,
        };

        let target = Path::new("snapshots").join(key.file_name());
        let temporary = self.root.join(format!(".latest-{}.tmp", Uuid::new_v4()));
        file_system
            .create_temporary_latest_symlink(&target, &temporary)
            .map_err(StorageError::from)?;
        if let Err(error) = file_system.rename_latest(&temporary, &self.root.join("latest")) {
            file_system.remove_file(&temporary);
            return Err(error.into());
        }
        file_system
            .sync_directory(&self.root)
            .map_err(StorageError::from)?;
        Ok(disposition)
    }

    fn inspect_latest(&self) -> CurrentLatest {
        let latest = self.root.join("latest");
        let target = match fs::read_link(&latest) {
            Ok(target) => target,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return CurrentLatest::Missing;
            }
            Err(_) => return CurrentLatest::Invalid,
        };
        let key = match validate_latest_target(&target) {
            Ok(key) => key,
            Err(_) => return CurrentLatest::Invalid,
        };
        match load_snapshot_beneath(&self.root.join("snapshots"), &key) {
            Ok(_) => CurrentLatest::Valid { key },
            Err(_) => CurrentLatest::Invalid,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct SnapshotKey {
    timestamp_nanos: i128,
    suffix: Uuid,
}

impl SnapshotKey {
    fn for_snapshot(snapshot: &ValidatedSnapshot, suffix: Uuid) -> Self {
        Self {
            timestamp_nanos: snapshot.captured_at().value().unix_timestamp_nanos(),
            suffix,
        }
    }

    fn parse(file_name: &OsStr) -> Result<Self, StorageError> {
        let file_name = file_name
            .to_str()
            .ok_or_else(|| StorageError::InvalidLatest {
                reason: "snapshot key is not UTF-8".to_owned(),
            })?;
        let stem = file_name
            .strip_suffix(".json")
            .ok_or_else(|| StorageError::InvalidLatest {
                reason: "snapshot key does not end in .json".to_owned(),
            })?;
        let (timestamp, suffix) =
            stem.split_once('-')
                .ok_or_else(|| StorageError::InvalidLatest {
                    reason: "snapshot key has no unique suffix".to_owned(),
                })?;
        if timestamp.len() != 32 {
            return Err(StorageError::InvalidLatest {
                reason: "snapshot key timestamp is not 32 hexadecimal digits".to_owned(),
            });
        }
        let ordered =
            u128::from_str_radix(timestamp, 16).map_err(|_| StorageError::InvalidLatest {
                reason: "snapshot key timestamp is not hexadecimal".to_owned(),
            })?;
        let timestamp_nanos = (ordered ^ (1_u128 << 127)) as i128;
        OffsetDateTime::from_unix_timestamp_nanos(timestamp_nanos).map_err(|_| {
            StorageError::InvalidLatest {
                reason: "snapshot key timestamp is outside the supported range".to_owned(),
            }
        })?;
        let suffix = Uuid::parse_str(suffix).map_err(|_| StorageError::InvalidLatest {
            reason: "snapshot key suffix is not a UUID".to_owned(),
        })?;
        if suffix.get_version_num() != 4 || suffix.get_variant() != Variant::RFC4122 {
            return Err(StorageError::InvalidLatest {
                reason: "snapshot key suffix is not an RFC 4122 version 4 UUID".to_owned(),
            });
        }

        let key = Self {
            timestamp_nanos,
            suffix,
        };
        if key.file_name() != file_name {
            return Err(StorageError::InvalidLatest {
                reason: "snapshot key is not canonically encoded".to_owned(),
            });
        }
        Ok(key)
    }

    fn file_name(&self) -> String {
        let ordered = (self.timestamp_nanos as u128) ^ (1_u128 << 127);
        format!("{ordered:032x}-{}.json", self.suffix)
    }

    fn matches_snapshot(&self, snapshot: &ValidatedSnapshot) -> bool {
        self.timestamp_nanos == snapshot.captured_at().value().unix_timestamp_nanos()
    }
}

fn prepare_directory(path: &Path) -> Result<(), StorageIoError> {
    let missing = missing_directory_chain(path)?;
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).map_err(|error| StorageIoError {
        operation: format!("create directory {}", path.display()),
        reason: error.to_string(),
    })?;

    make_directory_entries_durable(path, &missing, secure_and_sync_directory, sync_directory)
}

fn make_directory_entries_durable(
    path: &Path,
    missing_before_creation: &[PathBuf],
    mut secure_and_sync: impl FnMut(&Path) -> Result<(), StorageIoError>,
    mut sync_parent: impl FnMut(&Path) -> Result<(), StorageIoError>,
) -> Result<(), StorageIoError> {
    if missing_before_creation.is_empty() {
        secure_and_sync(path)?;
    } else {
        for directory in missing_before_creation.iter().rev() {
            secure_and_sync(directory)?;
        }
    }

    let mut ancestor = path.parent();
    while let Some(directory) = ancestor {
        if directory.as_os_str().is_empty() {
            sync_parent(Path::new("."))?;
            break;
        }
        sync_parent(directory)?;
        ancestor = directory.parent();
    }
    Ok(())
}

fn missing_directory_chain(path: &Path) -> Result<Vec<PathBuf>, StorageIoError> {
    let mut missing = Vec::new();
    let mut current = path.to_owned();
    loop {
        match fs::metadata(&current) {
            Ok(metadata) if metadata.is_dir() => return Ok(missing),
            Ok(_) => {
                return Err(StorageIoError {
                    operation: format!("validate directory {}", current.display()),
                    reason: "path is not a directory".to_owned(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(current.clone());
                current = current
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or_else(|| Path::new("."))
                    .to_owned();
            }
            Err(error) => {
                return Err(StorageIoError {
                    operation: format!("inspect directory {}", current.display()),
                    reason: error.to_string(),
                });
            }
        }
    }
}

fn secure_and_sync_directory(path: &Path) -> Result<(), StorageIoError> {
    let directory = open_real_directory(path)?;
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(|error| StorageIoError {
            operation: format!("set directory permissions on {}", path.display()),
            reason: error.to_string(),
        })?;
    directory.sync_all().map_err(|error| StorageIoError {
        operation: format!("sync directory {}", path.display()),
        reason: error.to_string(),
    })
}

fn open_real_directory(path: &Path) -> Result<File, StorageIoError> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| StorageIoError {
            operation: format!("open real directory {}", path.display()),
            reason: error.to_string(),
        })
}

fn write_temporary_snapshot(path: &Path, bytes: &[u8]) -> Result<File, StorageIoError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| StorageIoError {
            operation: "create temporary snapshot".to_owned(),
            reason: error.to_string(),
        })?;
    if let Err(error) = file.set_permissions(fs::Permissions::from_mode(0o600)) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(StorageIoError {
            operation: "set temporary snapshot permissions".to_owned(),
            reason: error.to_string(),
        });
    }
    if let Err(error) = file.write_all(bytes) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(StorageIoError {
            operation: "write temporary snapshot".to_owned(),
            reason: error.to_string(),
        });
    }
    Ok(file)
}

fn sync_temporary_snapshot(file: &File) -> Result<(), StorageIoError> {
    file.sync_all().map_err(|error| StorageIoError {
        operation: "sync temporary snapshot".to_owned(),
        reason: error.to_string(),
    })
}

fn commit_immutable_snapshot(
    file_system: &impl PublicationFileSystem,
    temporary: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), StorageIoError> {
    let file = file_system.write_temporary_snapshot(temporary, bytes)?;
    if let Err(error) = file_system.sync_temporary_snapshot(&file) {
        drop(file);
        file_system.remove_file(temporary);
        return Err(error);
    }
    drop(file);
    if let Err(error) = file_system.rename_no_replace(temporary, destination) {
        file_system.remove_file(temporary);
        return Err(error);
    }
    Ok(())
}

fn rename_no_replace(source: &Path, destination: &Path) -> Result<(), StorageIoError> {
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| StorageIoError {
        operation: "publish immutable snapshot".to_owned(),
        reason: "source path contains NUL".to_owned(),
    })?;
    let destination =
        CString::new(destination.as_os_str().as_bytes()).map_err(|_| StorageIoError {
            operation: "publish immutable snapshot".to_owned(),
            reason: "destination path contains NUL".to_owned(),
        })?;

    // renameat2 is the Linux no-replace commit primitive for immutable snapshots.
    let result = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if result == 0 {
        return Ok(());
    }
    Err(StorageIoError {
        operation: "publish immutable snapshot".to_owned(),
        reason: std::io::Error::last_os_error().to_string(),
    })
}

fn sync_directory(path: &Path) -> Result<(), StorageIoError> {
    open_real_directory(path)?
        .sync_all()
        .map_err(|error| StorageIoError {
            operation: format!("sync directory {}", path.display()),
            reason: error.to_string(),
        })
}

fn validate_latest_target(target: &Path) -> Result<SnapshotKey, StorageError> {
    if target.is_absolute() {
        return Err(StorageError::InvalidLatest {
            reason: "target is absolute".to_owned(),
        });
    }
    let components = target.components().collect::<Vec<_>>();
    match components.as_slice() {
        [Component::Normal(directory), Component::Normal(file_name)]
            if *directory == "snapshots" =>
        {
            SnapshotKey::parse(file_name)
        }
        _ => Err(StorageError::InvalidLatest {
            reason: "target is not snapshots/<immutable-key>.json".to_owned(),
        }),
    }
}

fn load_snapshot_beneath(
    snapshots: &Path,
    key: &SnapshotKey,
) -> Result<LoadedSnapshot, StorageError> {
    let directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(snapshots)
        .map_err(|error| StorageError::InvalidLatest {
            reason: format!("snapshots directory is unavailable: {error}"),
        })?;
    let file_name = key.file_name();
    let file_name_c = CString::new(file_name.as_bytes()).expect("snapshot keys contain no NUL");
    let descriptor = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            file_name_c.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
        )
    };
    if descriptor < 0 {
        return Err(StorageError::InvalidLatest {
            reason: std::io::Error::last_os_error().to_string(),
        });
    }
    // A successful openat transfers ownership of this new descriptor to File.
    let file = unsafe { File::from_raw_fd(descriptor) };
    let loaded = load_open_snapshot(snapshots.join(&file_name), file)?;
    if !key.matches_snapshot(loaded.snapshot()) {
        return Err(StorageError::InvalidLatest {
            reason: "snapshot key timestamp does not match snapshot captured_at".to_owned(),
        });
    }
    Ok(loaded)
}

fn load_snapshot_file(
    path: PathBuf,
    selected_by_latest: bool,
) -> Result<LoadedSnapshot, StorageError> {
    if fs::symlink_metadata(&path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(StorageError::SnapshotSymlink { path });
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|error| {
            if selected_by_latest {
                StorageError::InvalidLatest {
                    reason: error.to_string(),
                }
            } else {
                StorageError::Io {
                    operation: format!("open snapshot {}", path.display()),
                    reason: error.to_string(),
                }
            }
        })?;
    load_open_snapshot(path, file)
}

fn load_open_snapshot(path: PathBuf, mut file: File) -> Result<LoadedSnapshot, StorageError> {
    let metadata = file.metadata().map_err(|error| StorageError::Io {
        operation: format!("inspect snapshot {}", path.display()),
        reason: error.to_string(),
    })?;
    if !metadata.is_file() {
        return Err(StorageError::SnapshotNotRegular { path });
    }
    if metadata.len() > MAX_SNAPSHOT_BYTES as u64 {
        return Err(StorageError::SnapshotTooLarge {
            actual: metadata.len(),
            maximum: MAX_SNAPSHOT_BYTES,
        });
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut file)
        .take((MAX_SNAPSHOT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| StorageError::Io {
            operation: format!("read snapshot {}", path.display()),
            reason: error.to_string(),
        })?;
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(StorageError::SnapshotTooLarge {
            actual: bytes.len() as u64,
            maximum: MAX_SNAPSHOT_BYTES,
        });
    }
    let snapshot = ValidatedSnapshot::from_json(&bytes)
        .map_err(|error| StorageError::InvalidSnapshot(error.to_string()))?;
    Ok(LoadedSnapshot { path, snapshot })
}

struct LatestLock(File);

impl LatestLock {
    fn acquire(root: &Path) -> Result<Self, StorageError> {
        let path = root.join(".latest.lock");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(|error| StorageError::Io {
                operation: "open latest lock".to_owned(),
                reason: error.to_string(),
            })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            StorageError::Io {
                operation: "set latest lock permissions".to_owned(),
                reason: error.to_string(),
            }
        })?;
        let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
        if result != 0 {
            return Err(StorageError::Io {
                operation: "lock latest pointer".to_owned(),
                reason: std::io::Error::last_os_error().to_string(),
            });
        }
        Ok(Self(file))
    }
}

impl Drop for LatestLock {
    fn drop(&mut self) {
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

enum CurrentLatest {
    Missing,
    Invalid,
    Valid { key: SnapshotKey },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedSnapshot {
    path: PathBuf,
    snapshot: ValidatedSnapshot,
}

impl LoadedSnapshot {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn snapshot(&self) -> &ValidatedSnapshot {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> ValidatedSnapshot {
        self.snapshot
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LatestDisposition {
    Updated,
    KeptNewer,
    ReplacedInvalid,
    UpdateFailed(StorageError),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SnapshotPublication {
    NotPublished(PublicationFailure),
    PublicationIndeterminate {
        candidate_path: PathBuf,
        failure: PublicationFailure,
    },
    Published {
        snapshot_path: PathBuf,
        consistency: CaptureConsistency,
        latest: LatestDisposition,
    },
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{operation}: {reason}")]
pub struct PublicationFailure {
    operation: String,
    reason: String,
}

impl PublicationFailure {
    fn new(operation: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            reason: reason.into(),
        }
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum StorageError {
    #[error("state root is unavailable: {0}")]
    StateRootUnavailable(String),
    #[error("latest pointer is invalid: {reason}")]
    InvalidLatest { reason: String },
    #[error("snapshot path is a symlink: {path}", path = path.display())]
    SnapshotSymlink { path: PathBuf },
    #[error("snapshot path is not a regular file: {path}", path = path.display())]
    SnapshotNotRegular { path: PathBuf },
    #[error("snapshot is {actual} bytes; the maximum is {maximum}")]
    SnapshotTooLarge { actual: u64, maximum: usize },
    #[error("snapshot validation failed: {0}")]
    InvalidSnapshot(String),
    #[error("{operation}: {reason}")]
    Io { operation: String, reason: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct StorageIoError {
    operation: String,
    reason: String,
}

impl From<StorageIoError> for PublicationFailure {
    fn from(error: StorageIoError) -> Self {
        Self::new(error.operation, error.reason)
    }
}

impl From<StorageIoError> for StorageError {
    fn from(error: StorageIoError) -> Self {
        Self::Io {
            operation: error.operation,
            reason: error.reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::process::Command;

    use serde_json::json;

    use super::*;

    #[test]
    fn existing_directory_still_syncs_its_parent_for_a_concurrent_creator() {
        #[derive(Debug, Eq, PartialEq)]
        enum Event {
            Secure(PathBuf),
            Sync(PathBuf),
        }

        let events = RefCell::new(Vec::new());
        let path = PathBuf::from("/state/tmux-rescue");
        make_directory_entries_durable(
            &path,
            &[],
            |directory| {
                events
                    .borrow_mut()
                    .push(Event::Secure(directory.to_owned()));
                Ok(())
            },
            |directory| {
                events.borrow_mut().push(Event::Sync(directory.to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *events.borrow(),
            [
                Event::Secure(path),
                Event::Sync(PathBuf::from("/state")),
                Event::Sync(PathBuf::from("/")),
            ]
        );
    }

    #[test]
    fn existing_directory_syncs_every_ancestor_for_a_multilevel_concurrent_creator() {
        #[derive(Debug, Eq, PartialEq)]
        enum Event {
            Secure(PathBuf),
            Sync(PathBuf),
        }

        let events = RefCell::new(Vec::new());
        let path = PathBuf::from("/existing/new-parent/tmux-rescue");
        make_directory_entries_durable(
            &path,
            &[],
            |directory| {
                events
                    .borrow_mut()
                    .push(Event::Secure(directory.to_owned()));
                Ok(())
            },
            |directory| {
                events.borrow_mut().push(Event::Sync(directory.to_owned()));
                Ok(())
            },
        )
        .unwrap();

        assert_eq!(
            *events.borrow(),
            [
                Event::Secure(path),
                Event::Sync(PathBuf::from("/existing/new-parent")),
                Event::Sync(PathBuf::from("/existing")),
                Event::Sync(PathBuf::from("/")),
            ]
        );
    }

    #[test]
    fn latest_replacement_after_selection_does_not_change_the_opened_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let store = StateStore::new(root.clone());
        let selected = snapshot("2026-07-23T00:00:00Z", "selected");
        let replacement = snapshot("2026-07-23T01:00:00Z", "replacement");
        let selected_publication = store.publish(&selected);
        let replacement_publication = store.publish(&replacement);
        let selected_target =
            Path::new("snapshots").join(published_path(&selected_publication).file_name().unwrap());
        let replacement_target = Path::new("snapshots").join(
            published_path(&replacement_publication)
                .file_name()
                .unwrap(),
        );
        fs::remove_file(root.join("latest")).unwrap();
        symlink(&selected_target, root.join("latest")).unwrap();

        let loaded = store
            .load_latest_after_selection(|| {
                let temporary = root.join("replacement-latest");
                symlink(&replacement_target, &temporary).unwrap();
                fs::rename(temporary, root.join("latest")).unwrap();
            })
            .unwrap();

        assert_eq!(loaded.snapshot(), &selected);
        assert_eq!(
            fs::read_link(root.join("latest")).unwrap(),
            replacement_target
        );
    }

    #[test]
    fn temporary_snapshot_permissions_do_not_depend_on_umask() {
        const CHILD: &str = "TMUX_RESCUE_STORAGE_UMASK_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::tests::temporary_snapshot_permissions_do_not_depend_on_umask",
                ])
                .env(CHILD, "1")
                .status()
                .unwrap();
            assert!(status.success());
            return;
        }

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("snapshot.tmp");
        let previous = unsafe { libc::umask(0o777) };
        let result = write_temporary_snapshot(&path, b"snapshot");
        unsafe { libc::umask(previous) };
        let file = result.unwrap();
        assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FailurePoint {
        TemporaryWrite,
        TemporarySync,
        ImmutableRename,
        SnapshotsDirectorySync,
        TemporaryLatestSymlink,
        LatestRename,
        StateRootSync,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    enum FileSystemEvent {
        WriteTemporarySnapshot,
        SyncTemporarySnapshot,
        RenameImmutableSnapshot,
        SyncSnapshotsDirectory,
        CreateTemporaryLatestSymlink,
        RenameLatest,
        SyncStateRoot,
        RemoveFile,
    }

    struct FailingPublicationFileSystem {
        failure: FailurePoint,
        root: PathBuf,
        events: RefCell<Vec<FileSystemEvent>>,
    }

    impl FailingPublicationFileSystem {
        fn new(root: PathBuf, failure: FailurePoint) -> Self {
            Self {
                failure,
                root,
                events: RefCell::new(Vec::new()),
            }
        }

        fn events(&self) -> Vec<FileSystemEvent> {
            self.events.borrow().clone()
        }

        fn record(&self, event: FileSystemEvent) {
            self.events.borrow_mut().push(event);
        }
    }

    impl PublicationFileSystem for FailingPublicationFileSystem {
        fn write_temporary_snapshot(
            &self,
            path: &Path,
            bytes: &[u8],
        ) -> Result<File, StorageIoError> {
            self.record(FileSystemEvent::WriteTemporarySnapshot);
            if self.failure == FailurePoint::TemporaryWrite {
                return Err(injected_failure("write temporary snapshot"));
            }
            RealPublicationFileSystem.write_temporary_snapshot(path, bytes)
        }

        fn sync_temporary_snapshot(&self, file: &File) -> Result<(), StorageIoError> {
            self.record(FileSystemEvent::SyncTemporarySnapshot);
            if self.failure == FailurePoint::TemporarySync {
                return Err(injected_failure("sync temporary snapshot"));
            }
            RealPublicationFileSystem.sync_temporary_snapshot(file)
        }

        fn rename_no_replace(
            &self,
            source: &Path,
            destination: &Path,
        ) -> Result<(), StorageIoError> {
            self.record(FileSystemEvent::RenameImmutableSnapshot);
            if self.failure == FailurePoint::ImmutableRename {
                return Err(injected_failure("publish immutable snapshot"));
            }
            RealPublicationFileSystem.rename_no_replace(source, destination)
        }

        fn sync_directory(&self, path: &Path) -> Result<(), StorageIoError> {
            let failure = if path == self.root.join("snapshots") {
                self.record(FileSystemEvent::SyncSnapshotsDirectory);
                self.failure == FailurePoint::SnapshotsDirectorySync
            } else {
                assert_eq!(path, self.root);
                self.record(FileSystemEvent::SyncStateRoot);
                self.failure == FailurePoint::StateRootSync
            };
            if failure {
                return Err(injected_failure(format!(
                    "sync directory {}",
                    path.display()
                )));
            }
            RealPublicationFileSystem.sync_directory(path)
        }

        fn create_temporary_latest_symlink(
            &self,
            target: &Path,
            link: &Path,
        ) -> Result<(), StorageIoError> {
            self.record(FileSystemEvent::CreateTemporaryLatestSymlink);
            if self.failure == FailurePoint::TemporaryLatestSymlink {
                return Err(injected_failure("create temporary latest symlink"));
            }
            RealPublicationFileSystem.create_temporary_latest_symlink(target, link)
        }

        fn rename_latest(&self, source: &Path, destination: &Path) -> Result<(), StorageIoError> {
            self.record(FileSystemEvent::RenameLatest);
            if self.failure == FailurePoint::LatestRename {
                return Err(injected_failure("replace latest symlink"));
            }
            RealPublicationFileSystem.rename_latest(source, destination)
        }

        fn remove_file(&self, path: &Path) {
            self.record(FileSystemEvent::RemoveFile);
            RealPublicationFileSystem.remove_file(path);
        }
    }

    fn injected_failure(operation: impl Into<String>) -> StorageIoError {
        StorageIoError {
            operation: operation.into(),
            reason: "injected failure".to_owned(),
        }
    }

    fn snapshot(captured_at: &str, session_name: &str) -> ValidatedSnapshot {
        let encoded = |value: &str| json!({"encoding": "utf8", "value": value});
        let value = json!({
            "captured_at": captured_at,
            "source": encoded("/tmp/source.sock"),
            "consistency": {"kind": "stable"},
            "sessions": [{
                "name": session_name,
                "working_directory": encoded("/tmp/work"),
                "windows": [{
                    "source_index": 0,
                    "name": "editor",
                    "panes": [{
                        "source_index": 0,
                        "working_directory": encoded("/tmp/work"),
                        "recovery": {"kind": "idle"}
                    }]
                }]
            }]
        });
        ValidatedSnapshot::from_json(&serde_json::to_vec(&value).unwrap()).unwrap()
    }

    fn published_path(publication: &SnapshotPublication) -> &Path {
        let SnapshotPublication::Published { snapshot_path, .. } = publication else {
            panic!("expected published snapshot: {publication:?}");
        };
        snapshot_path
    }

    fn assert_not_published_with_operation(
        publication: SnapshotPublication,
        expected_operation: &str,
    ) {
        let SnapshotPublication::NotPublished(failure) = publication else {
            panic!("expected not-published result: {publication:?}");
        };
        assert_eq!(failure.operation, expected_operation);
    }

    fn assert_update_failed_with_operation(
        publication: &SnapshotPublication,
        expected_operation: &str,
    ) {
        let SnapshotPublication::Published {
            latest:
                LatestDisposition::UpdateFailed(StorageError::Io {
                    operation,
                    reason: _,
                }),
            ..
        } = publication
        else {
            panic!("expected published snapshot with latest update failure: {publication:?}");
        };
        assert_eq!(operation, expected_operation);
    }

    #[test]
    fn temporary_write_or_sync_failure_is_not_published_before_immutable_rename() {
        for (failure, operation) in [
            (FailurePoint::TemporaryWrite, "write temporary snapshot"),
            (FailurePoint::TemporarySync, "sync temporary snapshot"),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("state");
            let store = StateStore::new(root.clone());
            let file_system = FailingPublicationFileSystem::new(root.clone(), failure);

            let publication = store
                .publish_with_file_system(&snapshot("2026-07-23T00:00:00Z", "work"), &file_system);

            assert_not_published_with_operation(publication, operation);
            assert!(
                !file_system
                    .events()
                    .contains(&FileSystemEvent::RenameImmutableSnapshot)
            );
            assert_eq!(fs::read_dir(root.join("snapshots")).unwrap().count(), 0);
            assert!(!root.join("latest").exists());
        }
    }

    #[test]
    fn immutable_rename_failure_is_not_published_and_cleans_owned_temporary() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let store = StateStore::new(root.clone());
        let file_system =
            FailingPublicationFileSystem::new(root.clone(), FailurePoint::ImmutableRename);

        let publication =
            store.publish_with_file_system(&snapshot("2026-07-23T00:00:00Z", "work"), &file_system);

        assert_not_published_with_operation(publication, "publish immutable snapshot");
        assert!(file_system.events().contains(&FileSystemEvent::RemoveFile));
        assert_eq!(fs::read_dir(root.join("snapshots")).unwrap().count(), 0);
        assert!(!root.join("latest").exists());
    }

    #[test]
    fn snapshots_directory_sync_failure_is_indeterminate_without_latest_update() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let store = StateStore::new(root.clone());
        let prior = snapshot("2026-07-23T00:00:00Z", "prior");
        assert!(matches!(
            store.publish(&prior),
            SnapshotPublication::Published { .. }
        ));
        let prior_latest = fs::read_link(root.join("latest")).unwrap();
        let file_system =
            FailingPublicationFileSystem::new(root.clone(), FailurePoint::SnapshotsDirectorySync);

        let publication = store
            .publish_with_file_system(&snapshot("2026-07-23T01:00:00Z", "candidate"), &file_system);

        let SnapshotPublication::PublicationIndeterminate {
            candidate_path,
            failure,
        } = publication
        else {
            panic!("expected indeterminate publication: {publication:?}");
        };
        assert_eq!(
            failure.operation,
            format!("sync directory {}", root.join("snapshots").display())
        );
        assert!(candidate_path.is_file());
        assert_eq!(fs::read_link(root.join("latest")).unwrap(), prior_latest);
        assert_eq!(store.load_latest().unwrap().snapshot(), &prior);
        assert!(
            !file_system
                .events()
                .contains(&FileSystemEvent::CreateTemporaryLatestSymlink)
        );
    }

    #[test]
    fn temporary_latest_symlink_failure_reports_update_failed_and_keeps_prior_pointer() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let store = StateStore::new(root.clone());
        let prior = snapshot("2026-07-23T00:00:00Z", "prior");
        assert!(matches!(
            store.publish(&prior),
            SnapshotPublication::Published { .. }
        ));
        let prior_latest = fs::read_link(root.join("latest")).unwrap();
        let file_system =
            FailingPublicationFileSystem::new(root.clone(), FailurePoint::TemporaryLatestSymlink);

        let publication = store
            .publish_with_file_system(&snapshot("2026-07-23T01:00:00Z", "candidate"), &file_system);

        assert_update_failed_with_operation(&publication, "create temporary latest symlink");
        assert!(published_path(&publication).is_file());
        assert_eq!(fs::read_link(root.join("latest")).unwrap(), prior_latest);
        assert_eq!(store.load_latest().unwrap().snapshot(), &prior);
        assert!(
            !file_system
                .events()
                .contains(&FileSystemEvent::RenameLatest)
        );
    }

    #[test]
    fn latest_rename_failure_reports_update_failed_keeps_prior_pointer_and_cleans_temporary() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let store = StateStore::new(root.clone());
        let prior = snapshot("2026-07-23T00:00:00Z", "prior");
        assert!(matches!(
            store.publish(&prior),
            SnapshotPublication::Published { .. }
        ));
        let prior_latest = fs::read_link(root.join("latest")).unwrap();
        let file_system =
            FailingPublicationFileSystem::new(root.clone(), FailurePoint::LatestRename);

        let publication = store
            .publish_with_file_system(&snapshot("2026-07-23T01:00:00Z", "candidate"), &file_system);

        assert_update_failed_with_operation(&publication, "replace latest symlink");
        assert!(published_path(&publication).is_file());
        assert_eq!(fs::read_link(root.join("latest")).unwrap(), prior_latest);
        assert_eq!(store.load_latest().unwrap().snapshot(), &prior);
        assert!(file_system.events().contains(&FileSystemEvent::RemoveFile));
        assert!(fs::read_dir(&root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".latest-")
        }));
    }

    #[test]
    fn state_root_sync_failure_reports_update_failed_after_pointer_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("state");
        let store = StateStore::new(root.clone());
        let prior = snapshot("2026-07-23T00:00:00Z", "prior");
        assert!(matches!(
            store.publish(&prior),
            SnapshotPublication::Published { .. }
        ));
        let candidate = snapshot("2026-07-23T01:00:00Z", "candidate");
        let file_system =
            FailingPublicationFileSystem::new(root.clone(), FailurePoint::StateRootSync);

        let publication = store.publish_with_file_system(&candidate, &file_system);

        assert_update_failed_with_operation(
            &publication,
            &format!("sync directory {}", root.display()),
        );
        assert!(published_path(&publication).is_file());
        assert_eq!(store.load_latest().unwrap().snapshot(), &candidate);
        let events = file_system.events();
        let rename = events
            .iter()
            .position(|event| *event == FileSystemEvent::RenameLatest)
            .unwrap();
        let sync = events
            .iter()
            .position(|event| *event == FileSystemEvent::SyncStateRoot)
            .unwrap();
        assert!(rename < sync);
    }

    #[test]
    fn rename_no_replace_collision_preserves_destination_and_source() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::write(&source, b"candidate").unwrap();
        fs::write(&destination, b"existing").unwrap();

        let result = rename_no_replace(&source, &destination);

        assert!(result.is_err());
        assert_eq!(fs::read(&source).unwrap(), b"candidate");
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
    }

    #[test]
    fn immutable_commit_collision_cleans_temporary_without_overwriting_destination() {
        let temp = tempfile::tempdir().unwrap();
        let temporary = temp.path().join("temporary");
        let destination = temp.path().join("destination");
        fs::write(&destination, b"existing").unwrap();

        let result = commit_immutable_snapshot(
            &RealPublicationFileSystem,
            &temporary,
            &destination,
            b"candidate",
        );

        assert!(result.is_err());
        assert!(!temporary.exists());
        assert_eq!(fs::read(&destination).unwrap(), b"existing");
    }

    #[test]
    fn immutable_commit_preserves_an_unowned_temporary_on_creation_collision() {
        let temp = tempfile::tempdir().unwrap();
        let temporary = temp.path().join("temporary");
        let destination = temp.path().join("destination");
        fs::write(&temporary, b"existing-temporary").unwrap();

        let result = commit_immutable_snapshot(
            &RealPublicationFileSystem,
            &temporary,
            &destination,
            b"candidate",
        );

        assert!(result.is_err());
        assert_eq!(fs::read(&temporary).unwrap(), b"existing-temporary");
        assert!(!destination.exists());
    }
}
