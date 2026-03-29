use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::sqlite::Connection;
use super::{StorageError, STORAGE_SCHEMA_VERSION};

const BOOTSTRAP_SQL: &str = r#"
PRAGMA foreign_keys = ON;
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    external_id TEXT NOT NULL UNIQUE,
    label TEXT,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    status TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS raw_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    event_json TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_raw_events_session_seq
ON raw_events (session_id, seq);

CREATE TABLE IF NOT EXISTS semantic_actions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    action_kind TEXT NOT NULL,
    action_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_semantic_actions_session_seq
ON semantic_actions (session_id, seq);

CREATE TABLE IF NOT EXISTS keyframes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    frame_id TEXT NOT NULL UNIQUE,
    relative_path TEXT NOT NULL,
    sha256 TEXT,
    created_at_ms INTEGER NOT NULL
);
"#;

#[derive(Clone, Debug)]
pub struct Storage {
    db_path: PathBuf,
}

#[derive(Debug)]
pub struct StorageStatus {
    pub db_path: PathBuf,
    pub schema_version: u32,
}

#[derive(Clone, Debug)]
pub struct SessionRecord {
    pub id: i64,
    pub external_id: String,
    pub label: Option<String>,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub status: String,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug)]
pub struct RawEventRecord {
    pub id: i64,
    pub session_id: i64,
    pub sequence: i64,
    pub event_type: String,
    pub event_json: String,
    pub recorded_at_ms: u64,
    pub created_at_ms: u64,
}

#[derive(Debug)]
pub struct NewSession {
    pub external_id: String,
    pub label: Option<String>,
    pub started_at_ms: u64,
    pub status: String,
}

#[derive(Debug)]
pub struct NewRawEvent {
    pub session_id: i64,
    pub sequence: i64,
    pub event_type: String,
    pub event_json: String,
    pub recorded_at_ms: u64,
}

#[derive(Debug)]
pub struct NewKeyframe {
    pub session_id: i64,
    pub frame_id: String,
    pub relative_path: String,
    pub sha256: Option<String>,
}

impl Storage {
    pub fn bootstrap(db_path: PathBuf) -> Result<Self, StorageError> {
        if let Some(parent) = db_path.parent() {
            fs::create_dir_all(parent).map_err(|source| StorageError::io(parent.to_path_buf(), source))?;
        }

        let storage = Self { db_path };
        storage.migrate()?;
        Ok(storage)
    }

    pub fn status(&self) -> StorageStatus {
        StorageStatus {
            db_path: self.db_path.clone(),
            schema_version: STORAGE_SCHEMA_VERSION,
        }
    }

    pub fn insert_session(&self, session: &NewSession) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            INSERT INTO sessions (
                external_id,
                label,
                started_at_ms,
                ended_at_ms,
                status,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )?;

        statement.bind_text(1, &session.external_id)?;
        if let Some(label) = &session.label {
            statement.bind_text(2, label)?;
        } else {
            statement.bind_null(2)?;
        }
        statement.bind_int64(3, session.started_at_ms as i64)?;
        statement.bind_null(4)?;
        statement.bind_text(5, &session.status)?;
        statement.bind_int64(6, now_ms() as i64)?;
        statement.execute()?;

        Ok(connection.last_insert_rowid())
    }

    pub fn insert_raw_event(&self, event: &NewRawEvent) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            INSERT INTO raw_events (
                session_id,
                seq,
                event_type,
                event_json,
                recorded_at_ms,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?)
            "#,
        )?;

        statement.bind_int64(1, event.session_id)?;
        statement.bind_int64(2, event.sequence)?;
        statement.bind_text(3, &event.event_type)?;
        statement.bind_text(4, &event.event_json)?;
        statement.bind_int64(5, event.recorded_at_ms as i64)?;
        statement.bind_int64(6, now_ms() as i64)?;
        statement.execute()?;

        Ok(connection.last_insert_rowid())
    }

    pub fn insert_keyframe(&self, keyframe: &NewKeyframe) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            INSERT INTO keyframes (
                session_id,
                frame_id,
                relative_path,
                sha256,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?)
            "#,
        )?;

        statement.bind_int64(1, keyframe.session_id)?;
        statement.bind_text(2, &keyframe.frame_id)?;
        statement.bind_text(3, &keyframe.relative_path)?;
        if let Some(sha256) = &keyframe.sha256 {
            statement.bind_text(4, sha256)?;
        } else {
            statement.bind_null(4)?;
        }
        statement.bind_int64(5, now_ms() as i64)?;
        statement.execute()?;

        Ok(connection.last_insert_rowid())
    }

    pub fn session_count(&self) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let statement = connection.prepare("SELECT COUNT(*) FROM sessions")?;
        Ok(statement.query_int64()?.unwrap_or(0))
    }

    pub fn raw_event_count(&self) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let statement = connection.prepare("SELECT COUNT(*) FROM raw_events")?;
        Ok(statement.query_int64()?.unwrap_or(0))
    }

    pub fn keyframe_count(&self) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let statement = connection.prepare("SELECT COUNT(*) FROM keyframes")?;
        Ok(statement.query_int64()?.unwrap_or(0))
    }

    pub fn complete_session(&self, session_id: i64, ended_at_ms: u64) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            UPDATE sessions
            SET ended_at_ms = ?, status = ?
            WHERE id = ?
            "#,
        )?;
        statement.bind_int64(1, ended_at_ms as i64)?;
        statement.bind_text(2, "completed")?;
        statement.bind_int64(3, session_id)?;
        statement.execute()?;
        Ok(())
    }

    pub fn list_sessions(&self, limit: i64) -> Result<Vec<SessionRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, external_id, label, started_at_ms, ended_at_ms, status, created_at_ms
            FROM sessions
            ORDER BY started_at_ms DESC, id DESC
            LIMIT ?
            "#,
        )?;
        statement.bind_int64(1, limit)?;

        let mut rows = Vec::new();
        while statement.step()? {
            rows.push(SessionRecord {
                id: statement.column_int64(0),
                external_id: statement.column_text(1)?.unwrap_or_default(),
                label: statement.column_text(2)?,
                started_at_ms: statement.column_int64(3) as u64,
                ended_at_ms: if statement.column_is_null(4) {
                    None
                } else {
                    Some(statement.column_int64(4) as u64)
                },
                status: statement.column_text(5)?.unwrap_or_default(),
                created_at_ms: statement.column_int64(6) as u64,
            });
        }

        Ok(rows)
    }

    pub fn list_raw_events_for_session(
        &self,
        session_id: i64,
        limit: i64,
    ) -> Result<Vec<RawEventRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT id, session_id, seq, event_type, event_json, recorded_at_ms, created_at_ms
            FROM raw_events
            WHERE session_id = ?
            ORDER BY seq ASC, id ASC
            LIMIT ?
            "#,
        )?;
        statement.bind_int64(1, session_id)?;
        statement.bind_int64(2, limit)?;

        let mut rows = Vec::new();
        while statement.step()? {
            rows.push(RawEventRecord {
                id: statement.column_int64(0),
                session_id: statement.column_int64(1),
                sequence: statement.column_int64(2),
                event_type: statement.column_text(3)?.unwrap_or_default(),
                event_json: statement.column_text(4)?.unwrap_or_default(),
                recorded_at_ms: statement.column_int64(5) as u64,
                created_at_ms: statement.column_int64(6) as u64,
            });
        }

        Ok(rows)
    }

    fn migrate(&self) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        connection.exec_batch(BOOTSTRAP_SQL)?;

        let mut statement = connection.prepare(
            "INSERT OR IGNORE INTO schema_migrations (version, applied_at_ms) VALUES (?, ?)",
        )?;
        statement.bind_int64(1, STORAGE_SCHEMA_VERSION as i64)?;
        statement.bind_int64(2, now_ms() as i64)?;
        statement.execute()?;

        Ok(())
    }

    fn open_connection(&self) -> Result<Connection, StorageError> {
        Connection::open(&self.db_path)
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
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{NewKeyframe, NewRawEvent, NewSession, Storage};

    #[test]
    fn bootstraps_schema_and_inserts_records() {
        let root = unique_test_dir();
        let db_path = root.join("storage").join("cloneaprocess.sqlite3");

        let storage = Storage::bootstrap(db_path.clone()).expect("storage bootstrap should succeed");
        assert!(db_path.exists(), "database file should be created");

        let session_id = storage
            .insert_session(&NewSession {
                external_id: "sess_smoke".to_string(),
                label: Some("Smoke test".to_string()),
                started_at_ms: 1,
                status: "recording".to_string(),
            })
            .expect("session insert should succeed");
        assert!(session_id > 0, "session row id should be positive");

        let event_id = storage
            .insert_raw_event(&NewRawEvent {
                session_id,
                sequence: 0,
                event_type: "frontmost_app_changed".to_string(),
                event_json: r#"{"bundleId":"com.apple.TextEdit"}"#.to_string(),
                recorded_at_ms: 2,
            })
            .expect("raw event insert should succeed");
        assert!(event_id > 0, "raw event row id should be positive");
        assert_eq!(storage.session_count().expect("session count should load"), 1);
        assert_eq!(storage.raw_event_count().expect("raw event count should load"), 1);

        let sessions = storage.list_sessions(10).expect("sessions should load");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].external_id, "sess_smoke");

        let events = storage
            .list_raw_events_for_session(session_id, 10)
            .expect("events should load");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "frontmost_app_changed");

        let keyframe_id = storage
            .insert_keyframe(&NewKeyframe {
                session_id,
                frame_id: "frm_smoke".to_string(),
                relative_path: "recordings/sess_smoke/frames/frm_smoke.jpg".to_string(),
                sha256: None,
            })
            .expect("keyframe insert should succeed");
        assert!(keyframe_id > 0, "keyframe row id should be positive");
        assert_eq!(storage.keyframe_count().expect("keyframe count should load"), 1);

        storage
            .complete_session(session_id, 3)
            .expect("session should complete");
        let sessions = storage.list_sessions(10).expect("sessions should still load");
        assert_eq!(sessions[0].status, "completed");
        assert_eq!(sessions[0].ended_at_ms, Some(3));

        let _ = fs::remove_dir_all(&root);
    }

    fn unique_test_dir() -> std::path::PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("cloneaprocess-storage-test-{}", timestamp))
    }
}
