mod database;
mod error;
mod sqlite;

pub use database::{NewRawEvent, NewSession, RawEventRecord, SessionRecord, Storage, StorageStatus};
pub use error::StorageError;

pub const STORAGE_SCHEMA_VERSION: u32 = 1;
