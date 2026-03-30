use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::storage::{RetentionPolicy, SessionRecord, Storage, StorageError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionCleanupReport {
    pub policy: RetentionPolicy,
    pub retained_session_count: usize,
    pub pruned_session_count: usize,
    pub deleted_keyframe_file_count: usize,
    pub deleted_session_directory_count: usize,
    pub deleted_orphan_directory_count: usize,
}

pub fn run_retention_cleanup(
    storage: &Storage,
    recordings_root: &Path,
) -> Result<RetentionCleanupReport, StorageError> {
    let policy = storage.retention_policy()?;
    let sessions = storage.list_sessions(i64::MAX)?;
    let mut known_session_ids: HashSet<String> = sessions.iter().map(|session| session.external_id.clone()).collect();
    let active_session_ids: HashSet<String> = sessions
        .iter()
        .filter(|session| session.status == "recording")
        .map(|session| session.external_id.clone())
        .collect();

    let cutoff_ms = if policy.max_session_age_days == 0 {
        None
    } else {
        Some(now_ms().saturating_sub(u64::from(policy.max_session_age_days) * 24 * 60 * 60 * 1000))
    };

    let mut completed_rank = 0_u32;
    let mut pruned_session_count = 0_usize;
    let mut deleted_keyframe_file_count = 0_usize;
    let mut deleted_session_directory_count = 0_usize;

    for session in sessions.iter().filter(|session| session.status != "recording") {
        let over_count = policy.max_completed_sessions > 0 && completed_rank >= policy.max_completed_sessions;
        let too_old = cutoff_ms
            .map(|cutoff| session_age_reference_ms(session) < cutoff)
            .unwrap_or(false);

        if !(over_count || too_old) {
            completed_rank += 1;
            continue;
        }

        let keyframe_paths = storage.list_keyframe_paths_for_session(session.id)?;
        for path in keyframe_paths {
            let resolved_path = resolve_keyframe_path(recordings_root, &path);
            if remove_file_if_exists(&resolved_path)? {
                deleted_keyframe_file_count += 1;
            }
        }

        let mut deleted_this_session_dirs = 0_usize;
        for directory in session_directory_candidates(recordings_root, session) {
            if remove_directory_if_exists(&directory)? {
                deleted_this_session_dirs += 1;
            }
        }

        storage.delete_session(session.id)?;
        known_session_ids.remove(&session.external_id);
        deleted_session_directory_count += deleted_this_session_dirs;
        pruned_session_count += 1;
    }

    let orphan_grace_ms = u64::from(policy.orphan_grace_hours) * 60 * 60 * 1000;
    let mut deleted_orphan_directory_count = 0_usize;
    for root in [recordings_root.to_path_buf(), temp_recordings_root()] {
        deleted_orphan_directory_count += remove_orphan_session_directories(
            &root,
            &known_session_ids,
            &active_session_ids,
            orphan_grace_ms,
        )?;
    }

    Ok(RetentionCleanupReport {
        policy,
        retained_session_count: known_session_ids.len(),
        pruned_session_count,
        deleted_keyframe_file_count,
        deleted_session_directory_count,
        deleted_orphan_directory_count,
    })
}

fn session_age_reference_ms(session: &SessionRecord) -> u64 {
    session.ended_at_ms.unwrap_or(session.started_at_ms)
}

fn resolve_keyframe_path(recordings_root: &Path, stored_path: &str) -> PathBuf {
    let path = PathBuf::from(stored_path);
    if path.is_absolute() {
        return path;
    }

    let stripped = stored_path.strip_prefix("recordings/").unwrap_or(stored_path);
    recordings_root.join(stripped)
}

fn session_directory_candidates(recordings_root: &Path, session: &SessionRecord) -> Vec<PathBuf> {
    vec![
        recordings_root.join(&session.external_id),
        temp_recordings_root().join(&session.external_id),
    ]
}

fn temp_recordings_root() -> PathBuf {
    std::env::temp_dir().join("cloneaprocess-recordings")
}

fn remove_orphan_session_directories(
    root: &Path,
    known_session_ids: &HashSet<String>,
    active_session_ids: &HashSet<String>,
    orphan_grace_ms: u64,
) -> Result<usize, StorageError> {
    if !root.exists() {
        return Ok(0);
    }

    let mut deleted = 0_usize;
    for entry in fs::read_dir(root).map_err(|source| StorageError::io(root.to_path_buf(), source))? {
        let entry = entry.map_err(|source| StorageError::io(root.to_path_buf(), source))?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if known_session_ids.contains(name) || active_session_ids.contains(name) {
            continue;
        }
        if orphan_grace_ms > 0 && directory_age_ms(&path)? < orphan_grace_ms {
            continue;
        }
        if remove_directory_if_exists(&path)? {
            deleted += 1;
        }
    }

    Ok(deleted)
}

fn directory_age_ms(path: &Path) -> Result<u64, StorageError> {
    let metadata = fs::metadata(path).map_err(|source| StorageError::io(path.to_path_buf(), source))?;
    let modified_at = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    Ok(now_ms().saturating_sub(modified_at))
}

fn remove_file_if_exists(path: &Path) -> Result<bool, StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io(path.to_path_buf(), error)),
    }
}

fn remove_directory_if_exists(path: &Path) -> Result<bool, StorageError> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io(path.to_path_buf(), error)),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{run_retention_cleanup, temp_recordings_root};
    use crate::storage::{NewKeyframe, NewSession, RetentionPolicy, Storage};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn prunes_old_sessions_and_orphan_directories() {
        let root = unique_test_dir();
        let recordings_root = root.join("recordings");
        let db_path = root.join("storage").join("cloneaprocess.sqlite3");
        fs::create_dir_all(&recordings_root).expect("recordings root should exist");
        let temp_root = temp_recordings_root();
        fs::create_dir_all(&temp_root).expect("temp recordings root should exist");

        let storage = Storage::bootstrap(db_path).expect("storage should bootstrap");
        storage
            .update_retention_policy(&RetentionPolicy {
                max_completed_sessions: 1,
                max_session_age_days: 1,
                orphan_grace_hours: 0,
            })
            .expect("retention policy should update");

        let old_started_at = now_ms().saturating_sub(3 * 24 * 60 * 60 * 1000);
        let keep_started_at = now_ms();
        let old_session_id = storage
            .insert_session(&NewSession {
                external_id: "sess_old".to_string(),
                label: None,
                started_at_ms: old_started_at,
                status: "completed".to_string(),
            })
            .expect("old session should insert");
        let keep_session_id = storage
            .insert_session(&NewSession {
                external_id: "sess_keep".to_string(),
                label: None,
                started_at_ms: keep_started_at,
                status: "completed".to_string(),
            })
            .expect("keep session should insert");

        let old_session_dir = temp_root.join("sess_old").join("frames");
        fs::create_dir_all(&old_session_dir).expect("old session dir should exist");
        let old_frame_path = old_session_dir.join("frm_old.jpg");
        fs::write(&old_frame_path, b"old").expect("old frame should write");
        storage
            .insert_keyframe(&NewKeyframe {
                session_id: old_session_id,
                frame_id: "frm_old".to_string(),
                relative_path: old_frame_path.display().to_string(),
                sha256: None,
            })
            .expect("old keyframe should insert");

        let keep_session_dir = temp_root.join("sess_keep").join("frames");
        fs::create_dir_all(&keep_session_dir).expect("keep session dir should exist");
        let keep_frame_path = keep_session_dir.join("frm_keep.jpg");
        fs::write(&keep_frame_path, b"keep").expect("keep frame should write");
        storage
            .insert_keyframe(&NewKeyframe {
                session_id: keep_session_id,
                frame_id: "frm_keep".to_string(),
                relative_path: keep_frame_path.display().to_string(),
                sha256: None,
            })
            .expect("keep keyframe should insert");

        let orphan_dir = recordings_root.join("orphan_sess");
        fs::create_dir_all(&orphan_dir).expect("orphan dir should exist");
        std::thread::sleep(Duration::from_millis(5));

        let report = run_retention_cleanup(&storage, &recordings_root).expect("cleanup should succeed");

        assert_eq!(report.pruned_session_count, 1);
        assert_eq!(report.deleted_keyframe_file_count, 1);
        assert!(report.deleted_session_directory_count >= 1);
        assert!(report.deleted_orphan_directory_count >= 1);
        let sessions = storage.list_sessions(10).expect("sessions should reload");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].external_id, "sess_keep");
        assert!(!old_frame_path.exists(), "old frame should be deleted");
        assert!(keep_frame_path.exists(), "keep frame should remain");
        assert!(!orphan_dir.exists(), "orphan dir should be deleted");

        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(temp_root.join("sess_keep"));
    }

    fn unique_test_dir() -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("cloneaprocess-retention-test-{}", timestamp))
    }

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
    }
}
