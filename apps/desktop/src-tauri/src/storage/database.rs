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
    app_transition_count INTEGER NOT NULL DEFAULT 0,
    ax_snapshot_count INTEGER NOT NULL DEFAULT 0,
    keyframe_count_cached INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
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

CREATE TABLE IF NOT EXISTS workflow_runs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    external_id TEXT NOT NULL UNIQUE,
    workflow_id TEXT NOT NULL,
    workflow_name TEXT NOT NULL,
    source_session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    workflow_json TEXT NOT NULL,
    status TEXT NOT NULL,
    started_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    step_count INTEGER NOT NULL DEFAULT 0,
    completed_step_count INTEGER NOT NULL DEFAULT 0,
    failed_step_index INTEGER,
    last_error TEXT,
    created_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS workflow_run_logs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    workflow_run_id INTEGER NOT NULL REFERENCES workflow_runs(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    step_index INTEGER,
    event_type TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    recorded_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_workflow_run_logs_run_seq
ON workflow_run_logs (workflow_run_id, seq);

CREATE TABLE IF NOT EXISTS app_settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL,
    updated_at_ms INTEGER NOT NULL
);
"#;

pub const DEFAULT_RETENTION_MAX_COMPLETED_SESSIONS: u32 = 25;
pub const DEFAULT_RETENTION_MAX_SESSION_AGE_DAYS: u32 = 14;
pub const DEFAULT_RETENTION_ORPHAN_GRACE_HOURS: u32 = 24;

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
    pub app_transition_count: i64,
    pub ax_snapshot_count: i64,
    pub keyframe_count_cached: i64,
    pub last_error: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionPolicy {
    pub max_completed_sessions: u32,
    pub max_session_age_days: u32,
    pub orphan_grace_hours: u32,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct WorkflowRunRecord {
    pub id: i64,
    pub external_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub source_session_id: Option<i64>,
    pub workflow_json: String,
    pub status: String,
    pub started_at_ms: u64,
    pub ended_at_ms: Option<u64>,
    pub step_count: i64,
    pub completed_step_count: i64,
    pub failed_step_index: Option<i64>,
    pub last_error: Option<String>,
    pub created_at_ms: u64,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct WorkflowRunLogRecord {
    pub id: i64,
    pub workflow_run_id: i64,
    pub sequence: i64,
    pub step_index: Option<i64>,
    pub event_type: String,
    pub payload_json: String,
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

#[derive(Debug)]
pub struct NewWorkflowRun {
    pub external_id: String,
    pub workflow_id: String,
    pub workflow_name: String,
    pub source_session_id: Option<i64>,
    pub workflow_json: String,
    pub status: String,
    pub started_at_ms: u64,
    pub step_count: i64,
}

#[derive(Debug)]
pub struct NewWorkflowRunLog {
    pub workflow_run_id: i64,
    pub sequence: i64,
    pub step_index: Option<i64>,
    pub event_type: String,
    pub payload_json: String,
    pub recorded_at_ms: u64,
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
                app_transition_count,
                ax_snapshot_count,
                keyframe_count_cached,
                last_error,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
        statement.bind_int64(6, 0)?;
        statement.bind_int64(7, 0)?;
        statement.bind_int64(8, 0)?;
        statement.bind_null(9)?;
        statement.bind_int64(10, now_ms() as i64)?;
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

    pub fn insert_workflow_run(&self, run: &NewWorkflowRun) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            INSERT INTO workflow_runs (
                external_id,
                workflow_id,
                workflow_name,
                source_session_id,
                workflow_json,
                status,
                started_at_ms,
                ended_at_ms,
                step_count,
                completed_step_count,
                failed_step_index,
                last_error,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            "#,
        )?;

        statement.bind_text(1, &run.external_id)?;
        statement.bind_text(2, &run.workflow_id)?;
        statement.bind_text(3, &run.workflow_name)?;
        if let Some(source_session_id) = run.source_session_id {
            statement.bind_int64(4, source_session_id)?;
        } else {
            statement.bind_null(4)?;
        }
        statement.bind_text(5, &run.workflow_json)?;
        statement.bind_text(6, &run.status)?;
        statement.bind_int64(7, run.started_at_ms as i64)?;
        statement.bind_null(8)?;
        statement.bind_int64(9, run.step_count)?;
        statement.bind_int64(10, 0)?;
        statement.bind_null(11)?;
        statement.bind_null(12)?;
        statement.bind_int64(13, now_ms() as i64)?;
        statement.execute()?;

        Ok(connection.last_insert_rowid())
    }

    pub fn append_workflow_run_log(&self, log: &NewWorkflowRunLog) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            INSERT INTO workflow_run_logs (
                workflow_run_id,
                seq,
                step_index,
                event_type,
                payload_json,
                recorded_at_ms,
                created_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?)
            "#,
        )?;

        statement.bind_int64(1, log.workflow_run_id)?;
        statement.bind_int64(2, log.sequence)?;
        if let Some(step_index) = log.step_index {
            statement.bind_int64(3, step_index)?;
        } else {
            statement.bind_null(3)?;
        }
        statement.bind_text(4, &log.event_type)?;
        statement.bind_text(5, &log.payload_json)?;
        statement.bind_int64(6, log.recorded_at_ms as i64)?;
        statement.bind_int64(7, now_ms() as i64)?;
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

    pub fn retention_policy(&self) -> Result<RetentionPolicy, StorageError> {
        Ok(RetentionPolicy {
            max_completed_sessions: self
                .app_setting_u32(
                    "retention.max_completed_sessions",
                    DEFAULT_RETENTION_MAX_COMPLETED_SESSIONS,
                )?,
            max_session_age_days: self
                .app_setting_u32(
                    "retention.max_session_age_days",
                    DEFAULT_RETENTION_MAX_SESSION_AGE_DAYS,
                )?,
            orphan_grace_hours: self
                .app_setting_u32(
                    "retention.orphan_grace_hours",
                    DEFAULT_RETENTION_ORPHAN_GRACE_HOURS,
                )?,
        })
    }

    pub fn update_retention_policy(&self, policy: &RetentionPolicy) -> Result<(), StorageError> {
        self.upsert_app_setting(
            "retention.max_completed_sessions",
            &policy.max_completed_sessions.to_string(),
        )?;
        self.upsert_app_setting(
            "retention.max_session_age_days",
            &policy.max_session_age_days.to_string(),
        )?;
        self.upsert_app_setting(
            "retention.orphan_grace_hours",
            &policy.orphan_grace_hours.to_string(),
        )?;
        Ok(())
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

    #[allow(dead_code)]
    pub fn complete_workflow_run(
        &self,
        workflow_run_id: i64,
        status: &str,
        ended_at_ms: u64,
        completed_step_count: i64,
        failed_step_index: Option<i64>,
        last_error: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            UPDATE workflow_runs
            SET status = ?, ended_at_ms = ?, completed_step_count = ?, failed_step_index = ?, last_error = ?
            WHERE id = ?
            "#,
        )?;
        statement.bind_text(1, status)?;
        statement.bind_int64(2, ended_at_ms as i64)?;
        statement.bind_int64(3, completed_step_count)?;
        if let Some(failed_step_index) = failed_step_index {
            statement.bind_int64(4, failed_step_index)?;
        } else {
            statement.bind_null(4)?;
        }
        if let Some(last_error) = last_error {
            statement.bind_text(5, last_error)?;
        } else {
            statement.bind_null(5)?;
        }
        statement.bind_int64(6, workflow_run_id)?;
        statement.execute()?;
        Ok(())
    }

    pub fn update_workflow_run_state(
        &self,
        workflow_run_id: i64,
        status: &str,
        ended_at_ms: Option<u64>,
        completed_step_count: i64,
        failed_step_index: Option<i64>,
        last_error: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            UPDATE workflow_runs
            SET status = ?, ended_at_ms = ?, completed_step_count = ?, failed_step_index = ?, last_error = ?
            WHERE id = ?
            "#,
        )?;
        statement.bind_text(1, status)?;
        if let Some(ended_at_ms) = ended_at_ms {
            statement.bind_int64(2, ended_at_ms as i64)?;
        } else {
            statement.bind_null(2)?;
        }
        statement.bind_int64(3, completed_step_count)?;
        if let Some(failed_step_index) = failed_step_index {
            statement.bind_int64(4, failed_step_index)?;
        } else {
            statement.bind_null(4)?;
        }
        if let Some(last_error) = last_error {
            statement.bind_text(5, last_error)?;
        } else {
            statement.bind_null(5)?;
        }
        statement.bind_int64(6, workflow_run_id)?;
        statement.execute()?;
        Ok(())
    }

    pub fn update_session_summary(
        &self,
        session_id: i64,
        app_transition_count: i64,
        ax_snapshot_count: i64,
        keyframe_count_cached: i64,
        last_error: Option<&str>,
    ) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            UPDATE sessions
            SET app_transition_count = ?, ax_snapshot_count = ?, keyframe_count_cached = ?, last_error = ?
            WHERE id = ?
            "#,
        )?;
        statement.bind_int64(1, app_transition_count)?;
        statement.bind_int64(2, ax_snapshot_count)?;
        statement.bind_int64(3, keyframe_count_cached)?;
        if let Some(last_error) = last_error {
            statement.bind_text(4, last_error)?;
        } else {
            statement.bind_null(4)?;
        }
        statement.bind_int64(5, session_id)?;
        statement.execute()?;
        Ok(())
    }

    pub fn list_sessions(&self, limit: i64) -> Result<Vec<SessionRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                external_id,
                label,
                started_at_ms,
                ended_at_ms,
                status,
                app_transition_count,
                ax_snapshot_count,
                keyframe_count_cached,
                last_error,
                created_at_ms
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
                app_transition_count: statement.column_int64(6),
                ax_snapshot_count: statement.column_int64(7),
                keyframe_count_cached: statement.column_int64(8),
                last_error: statement.column_text(9)?,
                created_at_ms: statement.column_int64(10) as u64,
            });
        }

        Ok(rows)
    }

    pub fn get_session(&self, session_id: i64) -> Result<Option<SessionRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                external_id,
                label,
                started_at_ms,
                ended_at_ms,
                status,
                app_transition_count,
                ax_snapshot_count,
                keyframe_count_cached,
                last_error,
                created_at_ms
            FROM sessions
            WHERE id = ?
            LIMIT 1
            "#,
        )?;
        statement.bind_int64(1, session_id)?;

        if !statement.step()? {
            return Ok(None);
        }

        Ok(Some(SessionRecord {
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
            app_transition_count: statement.column_int64(6),
            ax_snapshot_count: statement.column_int64(7),
            keyframe_count_cached: statement.column_int64(8),
            last_error: statement.column_text(9)?,
            created_at_ms: statement.column_int64(10) as u64,
        }))
    }

    pub fn list_workflow_runs(&self, limit: i64) -> Result<Vec<WorkflowRunRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                external_id,
                workflow_id,
                workflow_name,
                source_session_id,
                workflow_json,
                status,
                started_at_ms,
                ended_at_ms,
                step_count,
                completed_step_count,
                failed_step_index,
                last_error,
                created_at_ms
            FROM workflow_runs
            ORDER BY started_at_ms DESC, id DESC
            LIMIT ?
            "#,
        )?;
        statement.bind_int64(1, limit)?;

        let mut rows = Vec::new();
        while statement.step()? {
            rows.push(WorkflowRunRecord {
                id: statement.column_int64(0),
                external_id: statement.column_text(1)?.unwrap_or_default(),
                workflow_id: statement.column_text(2)?.unwrap_or_default(),
                workflow_name: statement.column_text(3)?.unwrap_or_default(),
                source_session_id: if statement.column_is_null(4) {
                    None
                } else {
                    Some(statement.column_int64(4))
                },
                workflow_json: statement.column_text(5)?.unwrap_or_default(),
                status: statement.column_text(6)?.unwrap_or_default(),
                started_at_ms: statement.column_int64(7) as u64,
                ended_at_ms: if statement.column_is_null(8) {
                    None
                } else {
                    Some(statement.column_int64(8) as u64)
                },
                step_count: statement.column_int64(9),
                completed_step_count: statement.column_int64(10),
                failed_step_index: if statement.column_is_null(11) {
                    None
                } else {
                    Some(statement.column_int64(11))
                },
                last_error: statement.column_text(12)?,
                created_at_ms: statement.column_int64(13) as u64,
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

    pub fn list_keyframe_paths_for_session(&self, session_id: i64) -> Result<Vec<String>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT relative_path
            FROM keyframes
            WHERE session_id = ?
            ORDER BY id ASC
            "#,
        )?;
        statement.bind_int64(1, session_id)?;

        let mut paths = Vec::new();
        while statement.step()? {
            paths.push(statement.column_text(0)?.unwrap_or_default());
        }

        Ok(paths)
    }

    pub fn delete_session(&self, session_id: i64) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare("DELETE FROM sessions WHERE id = ?")?;
        statement.bind_int64(1, session_id)?;
        statement.execute()?;
        Ok(())
    }

    pub fn next_workflow_run_log_sequence(&self, workflow_run_id: i64) -> Result<i64, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT COALESCE(MAX(seq), -1) + 1
            FROM workflow_run_logs
            WHERE workflow_run_id = ?
            "#,
        )?;
        statement.bind_int64(1, workflow_run_id)?;
        Ok(statement.query_int64()?.unwrap_or(0))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn get_workflow_run(&self, workflow_run_id: i64) -> Result<Option<WorkflowRunRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                external_id,
                workflow_id,
                workflow_name,
                source_session_id,
                workflow_json,
                status,
                started_at_ms,
                ended_at_ms,
                step_count,
                completed_step_count,
                failed_step_index,
                last_error,
                created_at_ms
            FROM workflow_runs
            WHERE id = ?
            LIMIT 1
            "#,
        )?;
        statement.bind_int64(1, workflow_run_id)?;

        if !statement.step()? {
            return Ok(None);
        }

        Ok(Some(WorkflowRunRecord {
            id: statement.column_int64(0),
            external_id: statement.column_text(1)?.unwrap_or_default(),
            workflow_id: statement.column_text(2)?.unwrap_or_default(),
            workflow_name: statement.column_text(3)?.unwrap_or_default(),
            source_session_id: if statement.column_is_null(4) {
                None
            } else {
                Some(statement.column_int64(4))
            },
            workflow_json: statement.column_text(5)?.unwrap_or_default(),
            status: statement.column_text(6)?.unwrap_or_default(),
            started_at_ms: statement.column_int64(7) as u64,
            ended_at_ms: if statement.column_is_null(8) {
                None
            } else {
                Some(statement.column_int64(8) as u64)
            },
            step_count: statement.column_int64(9),
            completed_step_count: statement.column_int64(10),
            failed_step_index: if statement.column_is_null(11) {
                None
            } else {
                Some(statement.column_int64(11))
            },
            last_error: statement.column_text(12)?,
            created_at_ms: statement.column_int64(13) as u64,
        }))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn list_workflow_run_logs(
        &self,
        workflow_run_id: i64,
        limit: i64,
    ) -> Result<Vec<WorkflowRunLogRecord>, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT
                id,
                workflow_run_id,
                seq,
                step_index,
                event_type,
                payload_json,
                recorded_at_ms,
                created_at_ms
            FROM workflow_run_logs
            WHERE workflow_run_id = ?
            ORDER BY seq ASC, id ASC
            LIMIT ?
            "#,
        )?;
        statement.bind_int64(1, workflow_run_id)?;
        statement.bind_int64(2, limit)?;

        let mut rows = Vec::new();
        while statement.step()? {
            rows.push(WorkflowRunLogRecord {
                id: statement.column_int64(0),
                workflow_run_id: statement.column_int64(1),
                sequence: statement.column_int64(2),
                step_index: if statement.column_is_null(3) {
                    None
                } else {
                    Some(statement.column_int64(3))
                },
                event_type: statement.column_text(4)?.unwrap_or_default(),
                payload_json: statement.column_text(5)?.unwrap_or_default(),
                recorded_at_ms: statement.column_int64(6) as u64,
                created_at_ms: statement.column_int64(7) as u64,
            });
        }

        Ok(rows)
    }

    fn migrate(&self) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        connection.exec_batch(BOOTSTRAP_SQL)?;
        apply_optional_migration(&connection, "ALTER TABLE sessions ADD COLUMN app_transition_count INTEGER NOT NULL DEFAULT 0")?;
        apply_optional_migration(&connection, "ALTER TABLE sessions ADD COLUMN ax_snapshot_count INTEGER NOT NULL DEFAULT 0")?;
        apply_optional_migration(&connection, "ALTER TABLE sessions ADD COLUMN keyframe_count_cached INTEGER NOT NULL DEFAULT 0")?;
        apply_optional_migration(&connection, "ALTER TABLE sessions ADD COLUMN last_error TEXT")?;
        connection.exec_batch(
            r#"
            UPDATE sessions
            SET
                app_transition_count = (
                    SELECT COUNT(*)
                    FROM raw_events
                    WHERE raw_events.session_id = sessions.id
                      AND raw_events.event_type = 'frontmost_app_changed'
                ),
                ax_snapshot_count = (
                    SELECT COUNT(*)
                    FROM raw_events
                    WHERE raw_events.session_id = sessions.id
                      AND raw_events.event_type = 'ax_snapshot'
                ),
                keyframe_count_cached = (
                    SELECT COUNT(*)
                    FROM keyframes
                    WHERE keyframes.session_id = sessions.id
                )
            "#,
        )?;

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

    fn upsert_app_setting(&self, key: &str, value: &str) -> Result<(), StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            INSERT INTO app_settings (key, value, updated_at_ms)
            VALUES (?, ?, ?)
            ON CONFLICT(key) DO UPDATE SET
                value = excluded.value,
                updated_at_ms = excluded.updated_at_ms
            "#,
        )?;
        statement.bind_text(1, key)?;
        statement.bind_text(2, value)?;
        statement.bind_int64(3, now_ms() as i64)?;
        statement.execute()?;
        Ok(())
    }

    fn app_setting_u32(&self, key: &str, default: u32) -> Result<u32, StorageError> {
        let connection = self.open_connection()?;
        let mut statement = connection.prepare(
            r#"
            SELECT value
            FROM app_settings
            WHERE key = ?
            LIMIT 1
            "#,
        )?;
        statement.bind_text(1, key)?;

        if !statement.step()? {
            return Ok(default);
        }

        let value = statement.column_text(0)?.unwrap_or_default();
        Ok(value.parse::<u32>().unwrap_or(default))
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn apply_optional_migration(connection: &Connection, sql: &str) -> Result<(), StorageError> {
    match connection.exec_batch(sql) {
        Ok(()) => Ok(()),
        Err(error) if error.to_string().contains("duplicate column name") => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        DEFAULT_RETENTION_MAX_COMPLETED_SESSIONS, DEFAULT_RETENTION_MAX_SESSION_AGE_DAYS,
        DEFAULT_RETENTION_ORPHAN_GRACE_HOURS, NewKeyframe, NewRawEvent, NewSession, NewWorkflowRun,
        NewWorkflowRunLog, RetentionPolicy, Storage,
    };

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
        assert_eq!(sessions[0].app_transition_count, 0);
        assert_eq!(sessions[0].ax_snapshot_count, 0);
        assert_eq!(sessions[0].keyframe_count_cached, 0);
        assert_eq!(sessions[0].last_error, None);

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
            .update_session_summary(session_id, 3, 2, 1, Some("helper exited"))
            .expect("session summary should update");

        storage
            .complete_session(session_id, 3)
            .expect("session should complete");
        let sessions = storage.list_sessions(10).expect("sessions should still load");
        assert_eq!(sessions[0].status, "completed");
        assert_eq!(sessions[0].ended_at_ms, Some(3));
        assert_eq!(sessions[0].app_transition_count, 3);
        assert_eq!(sessions[0].ax_snapshot_count, 2);
        assert_eq!(sessions[0].keyframe_count_cached, 1);
        assert_eq!(sessions[0].last_error.as_deref(), Some("helper exited"));

        let workflow_run_id = storage
            .insert_workflow_run(&NewWorkflowRun {
                external_id: "run_smoke".to_string(),
                workflow_id: "wf_smoke".to_string(),
                workflow_name: "Smoke Workflow".to_string(),
                source_session_id: Some(session_id),
                workflow_json: r#"{"id":"wf_smoke","steps":[]}"#.to_string(),
                status: "running".to_string(),
                started_at_ms: 4,
                step_count: 2,
            })
            .expect("workflow run insert should succeed");
        assert!(workflow_run_id > 0, "workflow run row id should be positive");

        let workflow_log_id = storage
            .append_workflow_run_log(&NewWorkflowRunLog {
                workflow_run_id,
                sequence: 0,
                step_index: Some(0),
                event_type: "step_finished".to_string(),
                payload_json: r#"{"ok":true}"#.to_string(),
                recorded_at_ms: 5,
            })
            .expect("workflow run log insert should succeed");
        assert!(workflow_log_id > 0, "workflow run log row id should be positive");

        storage
            .complete_workflow_run(workflow_run_id, "completed", 6, 2, None, None)
            .expect("workflow run should complete");

        let workflow_run = storage
            .get_workflow_run(workflow_run_id)
            .expect("workflow run should load")
            .expect("workflow run should exist");
        assert_eq!(workflow_run.status, "completed");
        assert_eq!(workflow_run.completed_step_count, 2);
        assert_eq!(workflow_run.failed_step_index, None);

        let workflow_logs = storage
            .list_workflow_run_logs(workflow_run_id, 10)
            .expect("workflow run logs should load");
        assert_eq!(workflow_logs.len(), 1);
        assert_eq!(workflow_logs[0].event_type, "step_finished");

        let retention_policy = storage
            .retention_policy()
            .expect("retention policy should load");
        assert_eq!(
            retention_policy,
            RetentionPolicy {
                max_completed_sessions: DEFAULT_RETENTION_MAX_COMPLETED_SESSIONS,
                max_session_age_days: DEFAULT_RETENTION_MAX_SESSION_AGE_DAYS,
                orphan_grace_hours: DEFAULT_RETENTION_ORPHAN_GRACE_HOURS,
            }
        );

        storage
            .update_retention_policy(&RetentionPolicy {
                max_completed_sessions: 12,
                max_session_age_days: 5,
                orphan_grace_hours: 6,
            })
            .expect("retention policy should update");
        assert_eq!(
            storage.retention_policy().expect("retention policy should reload"),
            RetentionPolicy {
                max_completed_sessions: 12,
                max_session_age_days: 5,
                orphan_grace_hours: 6,
            }
        );

        let keyframe_paths = storage
            .list_keyframe_paths_for_session(session_id)
            .expect("keyframe paths should load");
        assert_eq!(keyframe_paths, vec!["recordings/sess_smoke/frames/frm_smoke.jpg".to_string()]);

        storage
            .delete_session(session_id)
            .expect("session delete should succeed");
        assert_eq!(storage.session_count().expect("session count should update"), 0);
        assert_eq!(storage.raw_event_count().expect("raw event count should update"), 0);
        assert_eq!(storage.keyframe_count().expect("keyframe count should update"), 0);

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
