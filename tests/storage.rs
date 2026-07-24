use std::ffi::CString;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::sync::{Arc, Barrier, mpsc};
use std::time::Duration;

use serde_json::json;
use tmux_rescue::{
    LatestDisposition, SnapshotPublication, StateStore, StorageError, ValidatedSnapshot,
};

fn encoded(value: &str) -> serde_json::Value {
    json!({"encoding": "utf8", "value": value})
}

fn snapshot(captured_at: &str, session_name: &str) -> ValidatedSnapshot {
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

fn published_path(publication: &SnapshotPublication) -> &std::path::Path {
    let SnapshotPublication::Published { snapshot_path, .. } = publication else {
        panic!("expected committed publication: {publication:?}");
    };
    snapshot_path
}

#[test]
fn publishes_an_immutable_owner_only_snapshot_and_relative_latest_link() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state/tmux-rescue");
    let store = StateStore::new(root.clone());
    let expected = snapshot("2026-07-23T00:00:00Z", "work");

    let publication = store.publish(&expected);

    assert!(matches!(
        publication,
        SnapshotPublication::Published {
            latest: LatestDisposition::Updated,
            ..
        }
    ));
    let path = published_path(&publication);
    assert!(path.starts_with(root.join("snapshots")));
    assert!(
        path.file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".json")
    );
    assert_eq!(
        fs::metadata(&root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(root.join("snapshots"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let latest_target = fs::read_link(root.join("latest")).unwrap();
    assert!(!latest_target.is_absolute());
    assert_eq!(latest_target.components().count(), 2);
    assert_eq!(
        latest_target.components().next().unwrap().as_os_str(),
        "snapshots"
    );
    assert_eq!(store.load_latest().unwrap().snapshot(), &expected);
}

#[test]
fn saves_a_clock_regressed_snapshot_without_moving_latest_backward() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(temp.path().join("state"));
    let newer = snapshot("2026-07-23T01:00:00Z", "newer");
    let older = snapshot("2026-07-23T00:00:00Z", "older");
    assert!(matches!(
        store.publish(&newer),
        SnapshotPublication::Published {
            latest: LatestDisposition::Updated,
            ..
        }
    ));

    let publication = store.publish(&older);

    assert!(matches!(
        publication,
        SnapshotPublication::Published {
            latest: LatestDisposition::KeptNewer,
            ..
        }
    ));
    assert!(published_path(&publication).exists());
    assert_eq!(store.load_latest().unwrap().snapshot(), &newer);
}

#[test]
fn equal_captures_keep_the_lexicographically_greater_immutable_key() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let store = StateStore::new(root.clone());
    let captured_at = "2026-07-23T00:00:00Z";
    let greater = snapshot(captured_at, "greater-key");
    let greater_publication = store.publish(&greater);
    let published_name = published_path(&greater_publication)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let greater_name = format!(
        "{}-ffffffff-ffff-4fff-bfff-ffffffffffff.json",
        &published_name[..32]
    );
    fs::rename(
        published_path(&greater_publication),
        root.join("snapshots").join(&greater_name),
    )
    .unwrap();
    fs::remove_file(root.join("latest")).unwrap();
    symlink(
        std::path::Path::new("snapshots").join(&greater_name),
        root.join("latest"),
    )
    .unwrap();

    let candidate = snapshot(captured_at, "candidate");
    let publication = store.publish(&candidate);
    let candidate_name = published_path(&publication)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();

    assert!(candidate_name < greater_name.as_str());
    assert!(matches!(
        publication,
        SnapshotPublication::Published {
            latest: LatestDisposition::KeptNewer,
            ..
        }
    ));
    assert_eq!(
        fs::read_link(root.join("latest")).unwrap(),
        std::path::Path::new("snapshots").join(&greater_name)
    );
    assert_eq!(store.load_latest().unwrap().snapshot(), &greater);
}

#[test]
fn rejects_hostile_latest_links_and_a_later_capture_replaces_them() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let store = StateStore::new(root.clone());
    let first = snapshot("2026-07-23T00:00:00Z", "first");
    assert!(matches!(
        store.publish(&first),
        SnapshotPublication::Published { .. }
    ));
    fs::remove_file(root.join("latest")).unwrap();
    symlink("/etc/passwd", root.join("latest")).unwrap();

    assert!(matches!(
        store.load_latest(),
        Err(StorageError::InvalidLatest { .. })
    ));

    let second = snapshot("2026-07-23T01:00:00Z", "second");
    assert!(matches!(
        store.publish(&second),
        SnapshotPublication::Published {
            latest: LatestDisposition::ReplacedInvalid,
            ..
        }
    ));
    assert_eq!(store.load_latest().unwrap().snapshot(), &second);
}

#[test]
fn latest_rejects_a_key_whose_timestamp_disagrees_with_the_opened_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let store = StateStore::new(root.clone());
    let first = snapshot("2026-07-23T00:00:00Z", "first");
    let first_publication = store.publish(&first);
    let published_name = published_path(&first_publication)
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let mut mismatched_timestamp = published_name.as_bytes()[..32].to_vec();
    mismatched_timestamp[31] = if mismatched_timestamp[31] == b'0' {
        b'1'
    } else {
        b'0'
    };
    let mismatched_name = format!(
        "{}-00000000-0000-4000-8000-000000000000.json",
        String::from_utf8(mismatched_timestamp).unwrap()
    );
    let mismatched_path = root.join("snapshots").join(&mismatched_name);
    fs::rename(published_path(&first_publication), &mismatched_path).unwrap();
    fs::remove_file(root.join("latest")).unwrap();
    symlink(
        std::path::Path::new("snapshots").join(&mismatched_name),
        root.join("latest"),
    )
    .unwrap();

    assert_eq!(
        store.load_explicit(&mismatched_path).unwrap().snapshot(),
        &first
    );
    assert!(matches!(
        store.load_latest(),
        Err(StorageError::InvalidLatest { reason })
            if reason == "snapshot key timestamp does not match snapshot captured_at"
    ));

    let second = snapshot("2026-07-23T01:00:00Z", "second");
    assert!(matches!(
        store.publish(&second),
        SnapshotPublication::Published {
            latest: LatestDisposition::ReplacedInvalid,
            ..
        }
    ));
    assert_eq!(store.load_latest().unwrap().snapshot(), &second);
}

#[test]
fn explicit_loading_rejects_a_final_symlink() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    let store = StateStore::new(root.clone());
    let publication = store.publish(&snapshot("2026-07-23T00:00:00Z", "work"));
    let link = root.join("snapshots/not-immutable.json");
    symlink(published_path(&publication), &link).unwrap();

    assert!(matches!(
        store.load_explicit(&link),
        Err(StorageError::SnapshotSymlink { .. })
    ));
}

#[test]
fn special_snapshot_files_are_rejected_without_blocking() {
    let temp = tempfile::tempdir().unwrap();
    let fifo = temp.path().join("snapshot.json");
    let encoded = CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);

    let store = StateStore::new(temp.path().join("unused-state"));
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || sender.send(store.load_explicit(&fifo)).unwrap());

    let result = receiver
        .recv_timeout(Duration::from_millis(500))
        .expect("loading a special file must not block");
    assert!(matches!(
        result,
        Err(StorageError::SnapshotNotRegular { .. })
    ));
}

#[test]
fn a_latest_update_failure_does_not_undo_the_committed_snapshot() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("state");
    fs::create_dir_all(root.join("latest")).unwrap();
    let store = StateStore::new(root);

    let publication = store.publish(&snapshot("2026-07-23T00:00:00Z", "work"));

    assert!(matches!(
        publication,
        SnapshotPublication::Published {
            latest: LatestDisposition::UpdateFailed(_),
            ..
        }
    ));
    assert!(published_path(&publication).is_file());
}

#[test]
fn latest_loading_rejects_a_symlinked_snapshots_directory() {
    let temp = tempfile::tempdir().unwrap();
    let real_root = temp.path().join("real");
    let real_store = StateStore::new(real_root.clone());
    let publication = real_store.publish(&snapshot("2026-07-23T00:00:00Z", "work"));
    let file_name = published_path(&publication).file_name().unwrap();

    let hostile_root = temp.path().join("hostile");
    fs::create_dir(&hostile_root).unwrap();
    symlink(real_root.join("snapshots"), hostile_root.join("snapshots")).unwrap();
    symlink(
        std::path::Path::new("snapshots").join(file_name),
        hostile_root.join("latest"),
    )
    .unwrap();
    let hostile_store = StateStore::new(hostile_root);

    assert!(matches!(
        hostile_store.load_latest(),
        Err(StorageError::InvalidLatest { .. })
    ));
}

#[test]
fn concurrent_publications_leave_latest_at_the_newest_capture() {
    let temp = tempfile::tempdir().unwrap();
    let store = StateStore::new(temp.path().join("state"));
    let barrier = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for snapshot in [
        snapshot("2026-07-23T00:00:00Z", "older"),
        snapshot("2026-07-23T01:00:00Z", "newer"),
    ] {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            store.publish(&snapshot)
        }));
    }
    barrier.wait();
    for thread in threads {
        assert!(matches!(
            thread.join().unwrap(),
            SnapshotPublication::Published { .. }
        ));
    }

    assert_eq!(
        store.load_latest().unwrap().snapshot().sessions()[0].name(),
        "newer"
    );
}
