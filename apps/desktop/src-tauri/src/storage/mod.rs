mod database;
mod error;
mod sqlite;

pub use database::{
    NewKeyframe, NewRawEvent, NewSession, NewWorkflowRun, NewWorkflowRunLog, RawEventRecord,
    RetentionPolicy, SessionRecord, Storage, StorageStatus, WorkflowRunLogRecord,
    WorkflowRunRecord,
};
pub use error::StorageError;

pub const STORAGE_SCHEMA_VERSION: u32 = 5;
